//! Scalp-shell generator: the dense underlayer that keeps skin from showing between locks (port of the cap shell of
//! `parts/hair.js`, lines 142 to 170).
//!
//! **The shell.** A grid of [`SEGMENTS`] x [`RINGS`] quads laid over the skull in polar coordinates: ring `i` is the
//! polar angle `theta` from the crown (0) down to the hairline, column `j` the azimuth `phi` from the front, round
//! through the character's left (+x), back to the front. A vertex sits on the head surface along direction
//! `(sin theta sin phi, cos theta, sin theta cos phi)`, pushed out by the shell's offset.
//!
//! **The hairline.** The shell ends at `theta_max(phi)`: [`CapGen::hairline_back_deg`] at the back, easing (smoothstep
//! of `cos phi` from [`FRONT_BLEND`]) to [`CapGen::hairline_front_deg`] at the forehead, minus an ear notch,
//! [`CapGen::ear_notch_deg`] x `exp(-cos^2 phi / `[`NOTCH_WIDTH`]`)` x smoothstep of `cos phi` over [`NOTCH_EDGES`],
//! that lifts the edge above each ear. Rings past the hairline collapse onto it (the prototype's `min`), so the grid
//! keeps its shape and the surplus triangles have no area.
//!
//! **The offset.** [`CapGen::thickness`] at the hairline, rising by [`CROWN_RISE`] of itself (0.010 m on the default
//! 0.016 m) toward the crown, so the shell is a thin cap over the scalp and stands a little off it at the top where the
//! locks overlap.
//!
//! **Clearance.** Every vertex, triangle middle and edge middle stands at least [`MIN_CLEARANCE`] (2 mm) off the head:
//! a face is flat where the head curves, so vertices that are only just clear leave the middle of a face inside it.
//! Vertices that would fall short are lifted ([`fill_shell`]); at the prototype's offsets none is.
//!
//! **Attributes.** `hair-uv` is `(3 sin phi, theta / reach)`, `reach` being the larger hairline angle (the back one
//! by default), and `hair-info` is `(0.37, 0.93, theta)`: a fixed strand seed and an underlayer shade, with the polar
//! angle the angel ring reads. Normals are the area-weighted average of the faces at each vertex (three.js
//! `computeVertexNormals`), seam and pole vertices kept separate as in the prototype.
//!
//! The generator draws no random numbers. [`fill_shell`] writes into a caller-owned mesh and, once that mesh has the
//! capacity, allocates nothing, so a rebuild on a slider drag reuses the same buffers.

use std::f64::consts::TAU;

use omuzaki_char_api::{CapGen, HAIR_INFO, HAIR_UV, HeadSurface};
use omuzaki_core::glam::Vec3;
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};

use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};

/// Quads round the head.
pub const SEGMENTS: usize = 96;
/// Rings of quads from the crown to the hairline.
pub const RINGS: usize = 48;
/// `cos phi` range over which the hairline eases from the back height to the front one: the part of the head that
/// counts as "forehead".
pub const FRONT_BLEND: (f64, f64) = (-0.2, 0.75);
/// Width of the ear notch's gaussian in `cos^2 phi`: how far along the side of the head it reaches.
pub const NOTCH_WIDTH: f64 = 0.10;
/// `cos phi` range over which the ear notch fades in from the back to the front half of the head.
pub const NOTCH_EDGES: (f64, f64) = (-0.6, 0.2);
/// How much higher the shell stands at the crown than at the hairline, as a fraction of [`CapGen::thickness`].
pub const CROWN_RISE: f64 = 0.625;
/// Scale of `hair-uv.x` round the head: the strand lines repeat three times across the shell's half width.
const UV_ACROSS: f64 = 3.0;
/// `hair-info.x`, the cap's strand seed.
const SEED: f32 = 0.37;
/// `hair-info.y`, the shade of the underlayer (it sits in the shadow of the locks).
const SHADE: f32 = 0.93;

/// Compiles the group: one shell and no clumps.
pub fn compile(g: &CapGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let mut shell = MeshData::default();
    fill_shell(g, ctx.head, &mut shell);
    CompiledGroup::empty(GroupGeometry::Shell(shell))
}

/// How many clumps [`compile`] makes: none, the cap is one shell.
pub fn clump_count(_g: &CapGen) -> usize {
    0
}

/// three.js `MathUtils.smoothstep`: 0 at or below `lo`, 1 at or above `hi`, an S-curve between.
pub(super) fn smoothstep(x: f64, lo: f64, hi: f64) -> f64 {
    if x <= lo {
        0.0
    } else if x >= hi {
        1.0
    } else {
        let u = (x - lo) / (hi - lo);
        u * u * (3.0 - 2.0 * u)
    }
}

/// Vertices in the shell.
pub const fn vertex_count() -> usize {
    (SEGMENTS + 1) * (RINGS + 1)
}

/// The polar angle (radians) where the shell ends at azimuth `phi`: the hairline, lifted over the ears.
fn hairline(g: &CapGen, back: f64, phi: f64) -> f64 {
    let front = phi.cos();
    let blend = smoothstep(front, FRONT_BLEND.0, FRONT_BLEND.1);
    let edge = back + (g.hairline_front_deg.to_radians() - back) * blend;
    let notch = g.ear_notch_deg.to_radians()
        * (-front * front / NOTCH_WIDTH).exp()
        * smoothstep(front, NOTCH_EDGES.0, NOTCH_EDGES.1);
    (edge - notch).max(0.0)
}

/// The unit direction from the head centre at polar angle `theta` and azimuth `phi`.
#[allow(clippy::cast_possible_truncation)] // unit vector components: f32 is the mesh's precision
fn direction(theta: f64, phi: f64) -> Vec3 {
    let (st, ct) = theta.sin_cos();
    let (sp, cp) = phi.sin_cos();
    Vec3::new((st * sp) as f32, ct as f32, (st * cp) as f32)
}

/// Writes the shell over `head` into `out`, replacing its positions, normals, `hair-uv`, `hair-info` and indices and
/// leaving its other fields untouched. Allocates only while `out` is still growing to its first full size (and when
/// its two attributes are first created).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // grid indices are small; attributes are f32
pub fn fill_shell(g: &CapGen, head: &dyn HeadSurface, out: &mut MeshData) {
    ensure_attributes(&mut out.extra);
    let MeshData { positions, normals, indices, extra, .. } = out;
    let [uv, info] = extra.as_mut_slice() else { return };
    // A hairline at 0 would divide by zero below; the contract's range keeps it far from that.
    let back = g.hairline_back_deg.to_radians().max(1e-6);
    // The grid reaches the farther of the two hairlines, whichever end that is (the contract allows a front hairline
    // lower on the head than the back one); by default that is the back, as in the prototype.
    let reach = back.max(g.hairline_front_deg.to_radians());
    positions.clear();
    uv.data.clear();
    info.data.clear();
    for i in 0..=RINGS {
        let theta = i as f64 / RINGS as f64 * reach;
        for j in 0..=SEGMENTS {
            let phi = j as f64 / SEGMENTS as f64 * TAU;
            let t = theta.min(hairline(g, back, phi));
            let offset = g.thickness * (1.0 + CROWN_RISE * (1.0 - t / reach));
            positions.push(head.point(direction(t, phi), offset as f32));
            uv.data.extend([(phi.sin() * UV_ACROSS) as f32, (t / reach) as f32]);
            info.data.extend([SEED, SHADE, t as f32]);
        }
    }
    indices.clear();
    indices.extend(triangles());
    // The normals' buffer doubles as scratch for the lifts, so the pass allocates nothing.
    normals.clear();
    normals.resize(positions.len(), Vec3::ZERO);
    lift_to_clearance(head, positions, indices, normals);
    normals.fill(Vec3::ZERO);
    smooth_normals(positions, indices, normals);
}

/// The least distance, metres, any point of the shell keeps from the head surface.
pub const MIN_CLEARANCE: f32 = 0.002;

/// How far `p` stands outside `head`, along the ray from the head centre.
fn clearance(head: &dyn HeadSurface, p: Vec3) -> f32 {
    let d = p - head.centre();
    d.length() - head.radius(d.try_normalize().unwrap_or(Vec3::Y))
}

/// Lifts vertices outward until every vertex, every triangle's middle and every edge's middle stands [`MIN_CLEARANCE`]
/// off the head. A flat face dips below the curved surface under its vertices (by the chord's sag, which grows with
/// the square of the grid step and with the head's curvature), so clearing the vertices is not enough. The prototype's
/// offsets are far above the floor, so nothing moves unless the thickness is near the contract's minimum.
///
/// `lifts` is scratch, one slot (the `x` component) per vertex.
fn lift_to_clearance(head: &dyn HeadSurface, positions: &mut [Vec3], indices: &[u32], lifts: &mut [Vec3]) {
    // A vertex rises along its ray, the face above it slightly less than it does: a margin covers that.
    const MARGIN: f32 = 1.05;
    for (p, lift) in positions.iter().zip(lifts.iter_mut()) {
        lift.x = (MIN_CLEARANCE - clearance(head, *p)).max(0.0);
    }
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| positions[i as usize]);
        let samples = [(a + b + c) / 3.0, (a + b) / 2.0, (b + c) / 2.0, (c + a) / 2.0];
        let deficit = samples.iter().map(|s| MIN_CLEARANCE - clearance(head, *s)).fold(0.0, f32::max);
        if deficit > 0.0 {
            for v in tri {
                let lift = &mut lifts[*v as usize].x;
                *lift = lift.max(deficit * MARGIN + 1e-6);
            }
        }
    }
    for (p, lift) in positions.iter_mut().zip(lifts.iter()) {
        if lift.x > 0.0 {
            *p += (*p - head.centre()).try_normalize().unwrap_or(Vec3::Y) * lift.x;
        }
    }
}

/// The shell's triangle list: two per quad, wound counter-clockwise seen from outside.
#[allow(clippy::cast_possible_truncation)] // the grid has a few thousand vertices
fn triangles() -> impl Iterator<Item = u32> {
    const ROW: u32 = SEGMENTS as u32 + 1;
    (0..RINGS as u32).flat_map(|i| {
        (0..SEGMENTS as u32).flat_map(move |j| {
            let (a, b) = (i * ROW + j, (i + 1) * ROW + j);
            [a, b, a + 1, b, b + 1, a + 1]
        })
    })
}

/// Makes `extra` exactly `hair-uv` (2 floats) then `hair-info` (3 floats), keeping them when they are already there.
fn ensure_attributes(extra: &mut Vec<ExtraAttribute>) {
    let right = matches!(extra.as_slice(), [a, b]
        if a.name.as_str() == HAIR_UV && a.components == 2 && b.name.as_str() == HAIR_INFO && b.components == 3);
    if right {
        return;
    }
    extra.clear();
    if let (Ok(uv), Ok(info)) = (Slug::new(HAIR_UV), Slug::new(HAIR_INFO)) {
        extra.push(ExtraAttribute { name: uv, components: 2, data: Vec::new() });
        extra.push(ExtraAttribute { name: info, components: 3, data: Vec::new() });
    }
}

/// Area-weighted vertex normals (three.js `computeVertexNormals`). A vertex whose faces all have no area (a pole, seen
/// from the collapsed triangles around it) has no normal; like the prototype's `fixNormals` it gets the direction from
/// the middle of the mesh's bounding box instead, so a shader never normalises a zero vector.
fn smooth_normals(positions: &[Vec3], indices: &[u32], normals: &mut [Vec3]) {
    for tri in indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| i as usize);
        let face = (positions[b] - positions[a]).cross(positions[c] - positions[a]);
        normals[a] += face;
        normals[b] += face;
        normals[c] += face;
    }
    let (lo, hi) = positions.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
    let middle = (lo + hi) * 0.5;
    for (n, p) in normals.iter_mut().zip(positions) {
        *n = if n.length_squared() > 1e-30 { n.normalize() } else { (*p - middle).try_normalize().unwrap_or(Vec3::Y) };
    }
}
