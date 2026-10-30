//! Scalp-shell generator: the dense underlayer that keeps skin from showing between locks (port of the cap shell of
//! `parts/hair.js`, lines 142 to 170).
//!
//! **The shell.** A grid of [`SEGMENTS`] x [`RINGS`] quads laid over the skull in polar coordinates: ring `i` is the
//! polar angle `theta` from the crown (0) down to the hairline, column `j` the azimuth `phi` from the front, round
//! through the character's left (+x), back to the front. A vertex sits on the head surface along direction
//! `(sin theta sin phi, cos theta, sin theta cos phi)`, pushed out by the shell's offset.
//!
//! **The hairline.** The shell ends at `theta_max(phi)`: [`CapGen::hairline_back_deg`] at the back, easing (smoothstep
//! of `cos phi` over [`CapGen::front_blend`]) to [`CapGen::hairline_front_deg`] at the forehead, minus an ear notch,
//! [`CapGen::ear_notch_deg`] x `exp(-cos^2 phi / `[`CapGen::notch_width`]`)` x smoothstep of `cos phi` over [`CapGen::notch_edges`],
//! that lifts the edge above each ear. Rings past the hairline collapse onto it (the prototype's `min`), so the grid
//! keeps its shape and the surplus triangles have no area.
//!
//! **The offset.** [`CapGen::thickness`] at the hairline, rising by [`CapGen::crown_rise`] of itself (0.010 m on the default
//! 0.016 m) toward the crown, so the shell is a thin cap over the scalp and stands a little off it at the top where the
//! locks overlap.
//!
//! **Clearance.** Every vertex, triangle middle and edge middle stands at least [`MIN_CLEARANCE`] (2 mm) off the head:
//! a face is flat where the head curves, so vertices that are only just clear leave the middle of a face inside it.
//! Vertices that would fall short are lifted ([`fill_shell`]); at the prototype's offsets none is.
//!
//! **Attributes.** `hair-uv` is `(sin phi, theta / reach)` (`across` in `-1..=1` as the renderer's contract says; the prototype scaled it by 3), `reach` being the larger hairline angle (the back one
//! by default), and `hair-info` is `(0.37, 0.93, theta)`: a fixed strand seed and an underlayer shade, with the polar
//! angle the angel ring reads. Normals are the area-weighted average of the faces at each vertex (three.js
//! `computeVertexNormals`), seam and pole vertices kept separate as in the prototype.
//!
//! The generator draws no random numbers. [`fill_shell`] writes into a caller-owned mesh and, once that mesh has the
//! capacity, allocates nothing: a tool that keeps its own mesh (the bench, the zero-allocation test) gets that path.
//! [`compile`], which every whole-hair rebuild goes through, starts from a fresh mesh: measured in release on the
//! default style, a fresh shell takes 4.2 ms against 3.9 ms on a warm one (a 7 % difference inside a 7.7 ms rebuild), so
//! the fresh mesh is not worth threading a kept shell through the compile pipeline.

use std::f64::consts::TAU;

use omuzaki_char_api::{Bulge, CapGen, HAIR_INFO, HAIR_UV, HeadSurface, Taper};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::id::Slug;
use omuzaki_core::mesh::{ExtraAttribute, MeshData};

use crate::hair::compile::{CompiledGroup, GroupGeometry, HairGenCtx};
use crate::hair::strands;
use crate::hair::volume::Field;

/// Quads round the head.
pub const SEGMENTS: usize = 96;
/// Rings of quads from the crown to the hairline.
pub const RINGS: usize = 48;
/// How many times the strand texture repeats round the head: a whole number, so its seam at the back closes.
pub const CAP_STRANDS: f32 = 14.0;

/// Compiles the group: one shell and no clumps, its strands combed by the hair's shared flow where it has one.
pub fn compile(g: &CapGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let mut shell = MeshData::default();
    fill_shell_combed(g, ctx.head, ctx.volume, &mut shell);
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
    let blend = smoothstep(front, g.front_blend[0], g.front_blend[1]);
    let edge = back + (g.hairline_front_deg.to_radians() - back) * blend;
    let notch = g.ear_notch_deg.to_radians()
        * (-front * front / g.notch_width).exp()
        * smoothstep(front, g.notch_edges[0], g.notch_edges[1]);
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
pub fn fill_shell(g: &CapGen, head: &dyn HeadSurface, out: &mut MeshData) {
    fill_shell_combed(g, head, None, out);
}

/// Strands of the strand texture per radian along a parting: as dense as round a whorl a radian away.
const PART_STRANDS: f64 = CAP_STRANDS as f64 / (TAU * 0.841_470_984_807_896_5);

/// [`fill_shell`] with its strands combed by the hair's shared flow `field` (#2478): `hair-uv.y` and the strand texture
/// run away from the parting, or from the whorl where that is nearer ([`Field::comb`]), instead of down from the top,
/// and the texture's strands lie along the parting and round the whorl. Without a field it is the prototype's shell.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // grid indices are small; attributes are f32
pub fn fill_shell_combed(g: &CapGen, head: &dyn HeadSurface, field: Option<&Field>, out: &mut MeshData) {
    ensure_attributes(&mut out.extra);
    let MeshData { positions, normals, uvs, indices, extra, .. } = out;
    let [uv, info] = extra.as_mut_slice() else {
        // `ensure_attributes` leaves exactly these two, and `attribute_names_are_slugs` proves it can create them; a
        // shell without them would be silently empty, so a broken invariant stops a debug build.
        debug_assert!(false, "ensure_attributes leaves exactly hair-uv and hair-info");
        return;
    };
    // A hairline at 0 would divide by zero below; the contract's range keeps it far from that.
    let back = g.hairline_back_deg.to_radians().max(1e-6);
    // The grid reaches the farther of the two hairlines, whichever end that is (the contract allows a front hairline
    // lower on the head than the back one); by default that is the back, as in the prototype.
    let reach = back.max(g.hairline_front_deg.to_radians());
    let (seed, shade) = (g.strand_seed as f32, g.shade as f32);
    positions.clear();
    uvs.clear();
    uv.data.clear();
    info.data.clear();
    for i in 0..=RINGS {
        let theta = i as f64 / RINGS as f64 * reach;
        for j in 0..=SEGMENTS {
            let phi = j as f64 / SEGMENTS as f64 * TAU;
            let t = theta.min(hairline(g, back, phi));
            let offset = g.thickness * (1.0 + g.crown_rise * (1.0 - t / reach));
            positions.push(head.point(direction(t, phi), offset as f32));
            // The strand texture runs down from the crown, [`CAP_STRANDS`] locks' worth of strands round the head,
            // all of it the texture's body: the shell has no tips. Combed, it runs from the parting or the whorl.
            let (down, strand) = match field.map(|f| f.comb(direction(t, phi).as_dvec3())) {
                Some(comb) if comb.from_part => ((comb.distance / reach).min(1.0), (comb.across * PART_STRANDS) as f32),
                Some(comb) => ((comb.distance / reach).min(1.0), (comb.across / TAU) as f32 * CAP_STRANDS),
                None => (t / reach, (j as f64 / SEGMENTS as f64) as f32 * CAP_STRANDS),
            };
            uv.data.extend([phi.sin() as f32, down as f32]);
            let along = down as f32 * (1.0 - strands::TIP) * strands::V_END;
            uvs.push(Vec2::new(strand, along));
            info.data.extend([seed, shade, t as f32]);
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

/// A lock the shell tucks under ([`tuck`]): its rest points, width and thickness.
pub struct Lock<'a> {
    /// Root to tip.
    pub rest: &'a [Vec3],
    /// Its width along it.
    pub width: &'a Taper,
    /// Its thickness along it.
    pub thickness: &'a Bulge,
}

/// How far under a lock's inner face the shell tucks, metres.
pub const TUCK_MARGIN: f32 = 0.0015;
/// The share of a lock's half width the tuck covers, so the shell stays up along the lock's edges, where a dip would
/// show between locks.
const TUCK_FOOTPRINT: f32 = 0.85;

/// Lowers the shell under every lock that would cut through it (#2412). The shell stands up to 26 mm off the scalp at
/// the crown and a lock that lies across it at a shallow angle, a fringe diving to the forehead, crosses its surface
/// along a ragged line: in a close-up a jagged patchwork of shell and lock. Under the middle of each lock (the lock's
/// footprint), the shell drops to [`TUCK_MARGIN`] below the lock's inner face, never below [`MIN_CLEARANCE`] off the
/// head; elsewhere it keeps its height. Locks are culled by their boxes, so the cost is the shell's few thousand vertices
/// against the locks that pass near each.
#[allow(clippy::many_single_char_names)] // vector geometry: points, directions and fractions
pub fn tuck(shell: &mut MeshData, head: &dyn HeadSurface, locks: &[Lock<'_>]) {
    let centre = head.centre();
    let boxes: Vec<(Vec3, Vec3)> = locks
        .iter()
        .map(|l| {
            #[allow(clippy::cast_possible_truncation)] // a lock's width in metres
            let reach = l.width.start.max(l.width.mid).max(l.width.tip) as f32 * 0.5;
            let (lo, hi) = l.rest.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
            (lo - Vec3::splat(reach), hi + Vec3::splat(reach))
        })
        .collect();
    let mut moved = false;
    for v in &mut shell.positions {
        let offset = *v - centre;
        let (r, Some(u)) = (offset.length(), offset.try_normalize()) else { continue };
        let floor = head.radius(u) + MIN_CLEARANCE * 1.5;
        let mut limit = r;
        for (lock, (lo, hi)) in locks.iter().zip(&boxes) {
            if v.clamp(*lo, *hi) != *v {
                continue;
            }
            let last = lock.rest.len().saturating_sub(1);
            for (i, pair) in lock.rest.windows(2).enumerate() {
                let (p, q) = (pair[0], pair[1]);
                let d = q - p;
                let s =
                    if d.length_squared() > 0.0 { ((*v - p).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
                let near = p + d * s;
                let along = (near - centre).dot(u);
                let lateral = ((near - centre) - u * along).length();
                #[allow(clippy::cast_precision_loss)] // a few dozen points
                let t = (i as f32 + s) / last.max(1) as f32;
                if lateral < lock.width.at(t) * 0.5 * TUCK_FOOTPRINT {
                    limit = limit.min(along - lock.thickness.at(t) * 0.5 - TUCK_MARGIN);
                }
            }
        }
        let target = limit.max(floor);
        if target < r {
            *v = centre + u * target;
            moved = true;
        }
    }
    if moved {
        let MeshData { positions, normals, indices, .. } = shell;
        normals.clear();
        normals.resize(positions.len(), Vec3::ZERO);
        lift_to_clearance(head, positions, indices, normals);
        normals.fill(Vec3::ZERO);
        smooth_normals(positions, indices, normals);
    }
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
    // Both names are constants that are valid slugs (`attribute_names_are_slugs` proves it), so this never fails; if
    // it ever did, `fill_shell` would stop on its own `debug_assert!` instead of building an empty shell.
    let names = (Slug::new(HAIR_UV), Slug::new(HAIR_INFO));
    debug_assert!(names.0.is_ok() && names.1.is_ok(), "the hair attribute names are valid slugs");
    if let (Ok(uv), Ok(info)) = names {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attribute_names_are_slugs() {
        assert!(Slug::new(HAIR_UV).is_ok() && Slug::new(HAIR_INFO).is_ok());
        let mut extra = Vec::new();
        ensure_attributes(&mut extra);
        let names: Vec<&str> = extra.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, [HAIR_UV, HAIR_INFO]);
    }
}
