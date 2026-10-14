//! Human ears: a small flattened shell with a rolled rim, tilted back (port of `buildEar`, `head.js` 142 to 175).
//!
//! The grid has `SEGMENTS + 1` columns of angle `a` once round a C-shaped outline (taller than wide, narrower below
//! with the lobe) and `RINGS + 1` rows from the concha's centre to the rim. Row `s` sits at `s^1.4` of the outline, its
//! height above the plane is a half sine (the shell) plus a bump at `s = 0.88` (the rolled rim), less a little at the
//! centre (the concha's dip). Vertex `(column i, row k)` is index `i * (RINGS + 1) + k`, the prototype's order.

use std::f64::consts::{PI, TAU};

use omuzaki_char_api::{Ears, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::{MeshData, MeshError};

use super::{Frame, RINGS, SEGMENTS, Side, shell_mesh};

/// Half the outline's width at size 1, metres.
const HALF_WIDTH: f64 = 0.028;
/// Half the outline's height at size 1 (the top; below the middle it is [`LOBE`] of it), metres.
const HALF_HEIGHT: f64 = super::HALF_HEIGHT;
/// How much of the height is left below the middle: the lobe is shorter than the top.
const LOBE: f64 = 0.85;
/// How much wider the outline is on its upper half than its lower.
const SKEW: f64 = 0.15;
/// The shell's thickness at its crest, metres.
const THICKNESS: f64 = 0.012;
/// The rolled rim's extra height, and where and how wide it is (in rows' fraction), metres.
const ROLL: f64 = 0.005;
const ROLL_AT: f64 = 0.88;
const ROLL_WIDTH: f64 = 0.08;
/// How far the concha dips below the plane at the centre, metres.
const DIP: f64 = 0.004;
/// How much the outline stretches towards the rim: row `s` is at `s^RIM_POWER`.
const RIM_POWER: f64 = 1.4;

/// The outline point at angle `a`: half-extent across and along the ear's height, metres at size 1.
fn outline(a: f64) -> (f64, f64) {
    let lower = if a > PI { LOBE } else { 1.0 };
    (HALF_WIDTH * a.cos() * (1.0 + SKEW * a.sin()), HALF_HEIGHT * a.sin() * lower)
}

/// The angle of grid column `i`.
pub(super) fn angle(column: u32) -> f64 {
    f64::from(column) / f64::from(SEGMENTS) * TAU
}

/// The rim factor of grid row `k`: how far out the outline the row sits, `0` at the centre and `1` at the rim.
pub(super) fn rim(row: u32) -> f64 {
    (f64::from(row) / f64::from(RINGS)).powf(RIM_POWER)
}

/// The grid vertex at `(column, row)` in the ear's own axes (across, up, out of the shell), metres at size 1.
fn local(column: u32, row: u32) -> DVec3 {
    let s = f64::from(row) / f64::from(RINGS);
    let (ox, oy) = outline(angle(column));
    let shell = THICKNESS * (PI * (s * 1.05).min(1.0)).sin();
    let roll = ROLL * (-((s - ROLL_AT) / ROLL_WIDTH).powi(2)).exp();
    let r = rim(row);
    DVec3::new(ox * r, oy * r, shell + roll - DIP * (1.0 - s))
}

/// How far (metres) outside the head's surface every vertex of an ear stands, at least.
pub const CLEARANCE: f64 = 0.0005;

/// `point` pushed straight out from the head centre (along the surface's direction there, which is its normal to
/// within the head's curvature) until it is [`CLEARANCE`] outside the head's surface. A point already clear is
/// returned as it is, so an ear that does not touch the head keeps the prototype's shape exactly. Where the shell
/// would sink into the head (its concha dips 4 mm behind the root's plane, and a flared or small ear cuts into the
/// curving side of the head) the vertices ride the head's surface instead, so the ear is seated on it and bent to it.
fn cleared(head: &dyn HeadSurface, point: DVec3) -> DVec3 {
    let centre = head.centre().as_dvec3();
    let offset = point - centre;
    let distance = offset.length();
    let direction = offset / distance;
    let wanted = f64::from(head.radius(direction.as_vec3())) + CLEARANCE;
    if distance.is_finite() && distance < wanted { centre + direction * wanted } else { point }
}

/// The shell of an ear: the grid at `size`, placed on `frame` at `root`, each vertex moved by `displace(a, rim)` (a
/// head-space offset in metres, given the vertex's angle and rim factor) and then out of the head if it is in it
/// or within [`CLEARANCE`] of it. Human ears displace nothing.
pub(super) fn shell(
    frame: &Frame,
    size: f64,
    root: Vec3,
    head: &dyn HeadSurface,
    displace: &dyn Fn(f64, f64) -> DVec3,
) -> Result<MeshData, MeshError> {
    let root = root.as_dvec3();
    let mut positions = Vec::with_capacity(((SEGMENTS + 1) * (RINGS + 1)) as usize);
    for column in 0..=SEGMENTS {
        for row in 0..=RINGS {
            let p = local(column, row) * size;
            let at = root + frame.across * p.x + frame.up * p.y + frame.normal * p.z;
            positions.push(cleared(head, at + displace(angle(column), rim(row))).as_vec3());
        }
    }
    shell_mesh(positions)
}

/// The human ear of `ears.size` and orientation on the `side` of the head, rooted at `root` and kept out of `head`.
///
/// # Errors
/// [`MeshError`] if the size makes a vertex that is not finite.
pub fn build(ears: &Ears, side: Side, root: Vec3, head: &dyn HeadSurface) -> Result<MeshData, MeshError> {
    shell(&Frame::of(ears, side), ears.size, root, head, &|_, _| DVec3::ZERO)
}
