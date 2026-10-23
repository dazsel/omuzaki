//! Animal ears (cat, fox, rabbit, wolf): a closed, cupped shell standing on top of the head, with an inner panel.
//!
//! **Placement.** The left ear's root is on the head as built ([`root`]), [`Species::from_top_deg`] down from the top
//! and [`Species::round_deg`] round from the front toward the left (an [`AnimalEars`] field overrides either); the right
//! ear is the left one mirrored across the head's centre, so the pair is symmetric to float precision.
//!
//! **The frame** ([`frame`]). The ear rises between the head's outward direction at the root and straight up
//! (`UPRIGHT`), leaning back by the species' lean plus `Ears::tilt_deg`'s difference from its default; its opening
//! faces the front, turned outward by the species' turn plus `Ears::flare_deg`'s difference from its default.
//!
//! **The shell.** Rings of [`AROUND`] points from the base (`v = 0`) to just under the tip, then one tip vertex. At
//! height `v` the outline's half-width is `width x (1 - v^taper)^(1/round)`: a taper of 1 and round of 1 is a straight
//! triangle, a larger round bulges the sides, a larger taper keeps them parallel longer (the rabbit's rounded top). A
//! ring is a crescent: the back is a half ellipse `depth` deep and the front is cupped `CUP` of that, meeting at the two
//! edges. The ear curves back a little as it rises. The base ring is laid onto the head's surface, so no gap shows at
//! the root, and a fan from the root closes it: the shell is watertight. No vertex above the base is inside the head.
//!
//! **The panel** is an open sheet just in front of the cup, `PANEL_WIDTH` of the outline wide, from `PANEL_FROM` to
//! `PANEL_TO` of the height, narrowing to a point; the head part draws it in the `ear_inner` colour.
//!
//! **Rabbit ears sway**: [`AnimalEar::chain`] lists three bone positions up the ear's middle, which the head part turns
//! into a soft spring chain; every vertex carries how far up the ear it is ([`AnimalEar::shell_along`]), for its weights.
//! The other species are rigid on the head.

#![allow(
    clippy::many_single_char_names,
    reason = "the shape's own notation: s the species, f the frame, v the height up the ear, t the angle round it"
)]

use omuzaki_char_api::{AnimalEars, EarSpecies, Ears, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::{MeshData, MeshError};
use omuzaki_face_api::narrow;

use super::Side;
use crate::head::mesh::vertex_normals;

/// Points round each ring of the shell (half on the cupped front, half on the back).
pub const AROUND: usize = 32;
/// Rings from the base to just under the tip.
pub const RINGS: usize = 16;
/// How far the ear's rise leans from the head's outward direction toward straight up, `0..=1`.
const UPRIGHT: f64 = 0.45;
/// How deep the front is cupped, as a share of the back's depth.
const CUP: f64 = 0.6;
/// The panel's width as a share of the outline's.
const PANEL_WIDTH: f64 = 0.62;
/// Where the panel starts and ends, as shares of the height.
const PANEL_FROM: f64 = 0.06;
const PANEL_TO: f64 = 0.84;
/// How far the panel stands in front of the cup, metres.
const PANEL_LIFT: f64 = 0.0007;
/// Columns and rows of the panel's grid.
const PANEL_COLUMNS: usize = 8;
const PANEL_ROWS: usize = 12;
/// No shell vertex above the base comes closer to the head's surface than this, metres.
pub const CLEARANCE: f32 = 0.0005;
/// The contract's default flare and tilt (`Ears::default`), which stand for the species' own stance.
const DEFAULT_FLARE_DEG: f64 = 42.0;
const DEFAULT_TILT_DEG: f64 = 7.0;
/// Where along the rabbit's ear its three spring bones sit.
pub const CHAIN_AT: [f64; 3] = [0.0, 0.4, 0.75];

/// One species' shape and stance, metres and degrees at size 1.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Species {
    /// Root to tip.
    pub height: f64,
    /// Half the width at the base.
    pub half_width: f64,
    /// How deep the back is at the base.
    pub depth: f64,
    /// The outline: `(1 - v^taper)^(1/round)` of the base width at height `v`.
    pub taper: f64,
    /// See `taper`.
    pub round: f64,
    /// How far the ear curves back by its tip, as a share of its height.
    pub curl: f64,
    /// Where the root stands: degrees down from the top...
    pub from_top_deg: f64,
    /// ...and round from the front toward the left.
    pub round_deg: f64,
    /// How far the ear leans back, degrees.
    pub lean_deg: f64,
    /// How far the opening turns outward from the front, degrees.
    pub turn_deg: f64,
    /// Whether it sways on a spring chain.
    pub sways: bool,
}

impl Species {
    /// The parameter set of `species`.
    #[must_use]
    pub const fn of(species: EarSpecies) -> Self {
        match species {
            EarSpecies::Cat => Self {
                height: 0.135,
                half_width: 0.048,
                depth: 0.02,
                taper: 1.15,
                round: 1.0,
                curl: 0.06,
                from_top_deg: 34.0,
                round_deg: 48.0,
                lean_deg: 6.0,
                turn_deg: 22.0,
                sways: false,
            },
            EarSpecies::Fox => Self {
                height: 0.175,
                half_width: 0.05,
                depth: 0.022,
                taper: 1.0,
                round: 0.85,
                curl: 0.05,
                from_top_deg: 30.0,
                round_deg: 44.0,
                lean_deg: 8.0,
                turn_deg: 20.0,
                sways: false,
            },
            EarSpecies::Rabbit => Self {
                height: 0.24,
                half_width: 0.03,
                depth: 0.014,
                taper: 2.6,
                round: 2.0,
                curl: 0.12,
                from_top_deg: 22.0,
                round_deg: 30.0,
                lean_deg: 12.0,
                turn_deg: 12.0,
                sways: true,
            },
            EarSpecies::Wolf => Self {
                height: 0.13,
                half_width: 0.064,
                depth: 0.024,
                taper: 1.0,
                round: 1.45,
                curl: 0.04,
                from_top_deg: 36.0,
                round_deg: 56.0,
                lean_deg: 4.0,
                turn_deg: 32.0,
                sways: false,
            },
        }
    }

    /// The outline's half-width at height `v` (`0..=1`) as a share of the base's.
    #[must_use]
    pub fn width_at(&self, v: f64) -> f64 {
        (1.0 - v.clamp(0.0, 1.0).powf(self.taper)).max(0.0).powf(1.0 / self.round)
    }
}

/// An ear's basis: where it rises, where its opening faces, and across it (bind-space unit vectors).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct EarFrame {
    /// Root to tip.
    pub up: DVec3,
    /// Out of the opening.
    pub facing: DVec3,
    /// Across the ear, square to both.
    pub across: DVec3,
}

/// The left ear's root on `head`: on its surface at the species' (or the overridden) place.
#[must_use]
pub fn root(ears: &Ears, head: &dyn HeadSurface) -> Vec3 {
    head.point(direction(&ears.animal), 0.0)
}

/// The direction from the head's centre to the left ear's root.
fn direction(a: &AnimalEars) -> Vec3 {
    let s = Species::of(a.species);
    let (theta, phi) =
        (a.from_top_deg.unwrap_or(s.from_top_deg).to_radians(), a.round_deg.unwrap_or(s.round_deg).to_radians());
    let d = DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos());
    d.as_vec3()
}

/// The left ear's frame at `root` on `head` (see the module docs).
#[must_use]
pub fn frame(ears: &Ears, root: Vec3, head: &dyn HeadSurface) -> EarFrame {
    let s = Species::of(ears.animal.species);
    let out = (root - head.centre()).as_dvec3().normalize_or(DVec3::Y);
    let rise = (out * (1.0 - UPRIGHT) + DVec3::Y * UPRIGHT).normalize();
    let lean = (s.lean_deg + ears.tilt_deg - DEFAULT_TILT_DEG).to_radians();
    let up = (rise * lean.cos() + DVec3::NEG_Z * lean.sin()).normalize();
    let square = |v: DVec3| (v - up * v.dot(up)).normalize_or(DVec3::Z);
    let (front, side) = (square(DVec3::Z), square(DVec3::X));
    let turn = (s.turn_deg + ears.flare_deg - DEFAULT_FLARE_DEG).to_radians();
    let facing = (front * turn.cos() + side * turn.sin()).normalize();
    EarFrame { up, facing, across: up.cross(facing).normalize() }
}

/// One built ear.
#[derive(Clone, Debug, PartialEq)]
pub struct AnimalEar {
    /// The closed shell (the fur).
    pub shell: MeshData,
    /// How far up the ear each shell vertex is, `0..=1`.
    pub shell_along: Vec<f32>,
    /// The inner panel, an open sheet.
    pub panel: MeshData,
    /// How far up the ear each panel vertex is.
    pub panel_along: Vec<f32>,
    /// The tip, bind space.
    pub tip: Vec3,
    /// The ear's frame.
    pub frame: EarFrame,
    /// The spring bones' rest positions up the ear's middle ([`CHAIN_AT`]), or none for a rigid ear.
    pub chain: Vec<Vec3>,
}

/// The `side` animal ear of `ears` on `head`, `left_root` being the left ear's root ([`root`]): the right ear is the
/// left one mirrored across the head's centre.
///
/// # Errors
/// [`MeshError`] if the size makes a vertex that is not finite.
pub fn build(ears: &Ears, side: Side, left_root: Vec3, head: &dyn HeadSurface) -> Result<AnimalEar, MeshError> {
    let centre_x = head.centre().x;
    let mirror = |p: Vec3| Vec3::new(2.0 * centre_x - p.x, p.y, p.z);
    // The left ear is built and the right mirrored from it, so the two are symmetric whatever the head's tessellation.
    let left = build_left(ears, left_root, head)?;
    if side == Side::Left {
        return Ok(left);
    }
    let flip = |mut mesh: MeshData| -> Result<MeshData, MeshError> {
        mesh.positions.iter_mut().for_each(|p| *p = mirror(*p));
        mesh.normals.iter_mut().for_each(|n| n.x = -n.x);
        mesh.indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
        mesh.validate()?;
        Ok(mesh)
    };
    let m = |v: DVec3| DVec3::new(-v.x, v.y, v.z);
    Ok(AnimalEar {
        shell: flip(left.shell)?,
        shell_along: left.shell_along,
        panel: flip(left.panel)?,
        panel_along: left.panel_along,
        tip: mirror(left.tip),
        frame: EarFrame { up: m(left.frame.up), facing: m(left.frame.facing), across: -m(left.frame.across) },
        chain: left.chain.into_iter().map(mirror).collect(),
    })
}

/// Where the ear's middle is at height `v`, before the cross-section: up the frame, curving back toward the tip.
fn spine(s: &Species, f: &EarFrame, root: DVec3, size: f64, v: f64) -> DVec3 {
    root + f.up * (v * s.height * size) - f.facing * (v * v * s.curl * s.height * size)
}

/// The cross-section's offset from the spine at height `v` and angle `t` round the ring: across by the outline, and
/// along the facing by the crescent (the cup in front for `t` in `0..pi`, the back for `pi..2pi`).
fn section(s: &Species, f: &EarFrame, size: f64, v: f64, t: f64) -> DVec3 {
    let w = s.width_at(v);
    let (a, b) = (s.half_width * size * w, s.depth * size * w.powf(0.7));
    let (sin, cos) = t.sin_cos();
    let z = if sin >= 0.0 { -CUP * b * sin * sin } else { -b * -sin };
    f.across * (a * cos) + f.facing * (z + 0.5 * b)
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // ring counts are small
fn build_left(ears: &Ears, root: Vec3, head: &dyn HeadSurface) -> Result<AnimalEar, MeshError> {
    let s = Species::of(ears.animal.species);
    let f = frame(ears, root, head);
    let (size, r) = (ears.size, root.as_dvec3());
    let centre = head.centre();
    let lift = |p: Vec3, base: bool| {
        let d = p - centre;
        let dir = d.normalize_or(Vec3::Y);
        let surface = head.radius(dir);
        if base {
            centre + dir * surface
        } else if d.length() < surface + CLEARANCE {
            centre + dir * (surface + CLEARANCE)
        } else {
            p
        }
    };

    // The shell: the root, the rings, the tip.
    let mut positions = vec![root];
    let mut along = vec![0.0f32];
    for k in 0..RINGS {
        let v = k as f64 / RINGS as f64;
        for i in 0..AROUND {
            let t = std::f64::consts::TAU * i as f64 / AROUND as f64;
            let p = (spine(&s, &f, r, size, v) + section(&s, &f, size, v, t)).as_vec3();
            positions.push(lift(p, k == 0));
            along.push(narrow(v));
        }
    }
    let tip = spine(&s, &f, r, size, 1.0).as_vec3();
    positions.push(tip);
    along.push(1.0);
    let ring = |k: usize, i: usize| (1 + k * AROUND + i % AROUND) as u32;
    let top = (positions.len() - 1) as u32;
    let mut indices = Vec::with_capacity(AROUND * (RINGS + 1) * 6);
    for i in 0..AROUND {
        indices.extend([0, ring(0, i + 1), ring(0, i)]);
        for k in 0..RINGS - 1 {
            indices.extend([
                ring(k, i),
                ring(k, i + 1),
                ring(k + 1, i + 1),
                ring(k, i),
                ring(k + 1, i + 1),
                ring(k + 1, i),
            ]);
        }
        indices.extend([ring(RINGS - 1, i), ring(RINGS - 1, i + 1), top]);
    }
    outward(&positions, &mut indices);
    let shell = mesh(positions, indices)?;

    // The panel: a grid in front of the cup, narrowing to a point.
    let (mut positions, mut panel_along) = (Vec::new(), Vec::new());
    for row in 0..=PANEL_ROWS {
        let u = row as f64 / PANEL_ROWS as f64;
        let v = PANEL_FROM + (PANEL_TO - PANEL_FROM) * u;
        let narrowing = PANEL_WIDTH * (1.0 - u.powi(3)).max(0.0).sqrt();
        for col in 0..=PANEL_COLUMNS {
            let x = (2.0 * col as f64 / PANEL_COLUMNS as f64 - 1.0) * narrowing;
            // The cup's front at `cos t = x`, `sin t >= 0`.
            let t = x.clamp(-1.0, 1.0).acos();
            let p = spine(&s, &f, r, size, v) + section(&s, &f, size, v, t) + f.facing * PANEL_LIFT;
            positions.push(lift(p.as_vec3(), false));
            panel_along.push(narrow(v));
        }
    }
    let columns = (PANEL_COLUMNS + 1) as u32;
    let mut indices = Vec::new();
    for row in 0..PANEL_ROWS as u32 {
        for col in 0..PANEL_COLUMNS as u32 {
            let a = row * columns + col;
            let b = a + columns;
            indices.extend([a, a + 1, b + 1, a, b + 1, b]);
        }
    }
    // The panel faces out of the opening.
    let normal = f.facing.as_vec3();
    let facing_out = indices.as_chunks::<3>().0.iter().map(|t| {
        let [a, b, c] = [t[0], t[1], t[2]].map(|i| positions[i as usize]);
        (b - a).cross(c - a).dot(normal)
    });
    if facing_out.sum::<f32>() < 0.0 {
        indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
    }
    let panel = mesh(positions, indices)?;

    let chain =
        if s.sways { CHAIN_AT.iter().map(|&v| spine(&s, &f, r, size, v).as_vec3()).collect() } else { Vec::new() };
    Ok(AnimalEar { shell, shell_along: along, panel, panel_along, tip, frame: f, chain })
}

/// Turns the triangles to face out of the closed shell `positions` bounds (a positive signed volume).
fn outward(positions: &[Vec3], indices: &mut [u32]) {
    let volume: f32 = indices
        .as_chunks::<3>()
        .0
        .iter()
        .map(|t| positions[t[0] as usize].dot(positions[t[1] as usize].cross(positions[t[2] as usize])))
        .sum();
    if volume < 0.0 {
        indices.as_chunks_mut::<3>().0.iter_mut().for_each(|t| t.swap(1, 2));
    }
}

/// A mesh with smooth normals.
fn mesh(positions: Vec<Vec3>, indices: Vec<u32>) -> Result<MeshData, MeshError> {
    let normals = vertex_normals(&positions, &indices);
    let mesh = MeshData { positions, normals, indices, ..MeshData::default() };
    mesh.validate()?;
    Ok(mesh)
}
