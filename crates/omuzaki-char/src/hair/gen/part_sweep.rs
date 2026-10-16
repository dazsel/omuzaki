//! Part-sweep generator: locks that leave the part line and sweep down each side of the head to the braid line or
//! behind the ear (port of `parts/hair.js`, lines 174 to 197).
//!
//! **The path of one lock.** It starts on the part line, a line in the sagittal plane from [`PartSweepGen::part_front_deg`]
//! (forward of vertical) to [`PartSweepGen::part_back_deg`], at the fraction `part_start + (1 - part_start) s` of the
//! way along, nudged [`PartSweepGen::part_spread`] to its own side so the two sides do not share a root. It ends on the braid line,
//! a ring round the head whose height follows [`PartSweepGen::end_height_front`] at the front and
//! [`PartSweepGen::end_height_back`] at the back (smoothstep of the azimuth over [`PartSweepGen::height_blend_deg`]), at azimuth
//! `lerp(end_from_deg, end_to_deg, s^end_spread)`. In between it follows the great circle from start to end direction
//! (slerp at `t^sweep_ease`), riding [`PartSweepGen::lift`] metres above the head plus a [`PartSweepGen::belly`] bulge in the middle and
//! [`PartSweepGen::alternate_step`] on every other lock, so neighbours stack instead of z-fighting. Nine control points are resampled
//! to [`REST_POINTS`] by the geometry kit's Catmull-Rom.
//!
//! `s = i / (count - 1)` indexes the lock; the `+x` side comes first, then the `-x` side, each `count` locks. Shade
//! alternates [`PartSweepGen::shades`]. Widths and thickness are the group's [`Taper`](omuzaki_char_api::Taper) and
//! [`Bulge`](omuzaki_char_api::Bulge).
//!
//! **Units.** The braid line's heights are metres above the head centre, which are the character's own data
//! (`end_height_front`, `end_height_back`; the contract's defaults are 0.04 and -0.04, not the prototype's). The angle
//! they stand for is `acos(height / crown radius)` with the crown radius the head's radius straight up, so a style
//! keeps its shape when the head is scaled. (The prototype does the same sum with one fixed head radius, a per-character
//! constant, on purpose: a height names a ring round the head, found as if the head were a sphere, and the lock then rides
//! the real surface at that direction. The crown radius stands in for that constant.)
//!
//! The generator draws no random numbers.

use omuzaki_char_api::{HeadSurface, PartSweepGen};
use omuzaki_core::glam::{DQuat, DVec3, Vec3};

use super::cap::smoothstep;
use crate::hair::compile::{CompiledGroup, HairGenCtx};
use crate::kit::catmull::spaced_points_into;

/// Control points per lock, before the Catmull-Rom resample.
pub const CONTROL_POINTS: usize = 9;
/// Points per lock after the resample.
pub const REST_POINTS: usize = 14;
/// Azimuth range (degrees, from the front) over which the braid line drops from its front height to its back height:
/// the default of [`PartSweepGen::height_blend_deg`], and the braid crown's own (it has no field for it).
pub const HEIGHT_BLEND: (f64, f64) = (70.0, 180.0);
/// How far along the path the belly peaks and falls again (`sin(pi min(1, t x BELLY_REACH))`).
const BELLY_REACH: f64 = 1.2;

/// One lock's rest points, root first.
pub type Lock = [Vec3; REST_POINTS];

/// Compiles the group: `count` locks per side, `+x` side first.
pub fn compile(g: &PartSweepGen, ctx: &mut HairGenCtx<'_>) -> CompiledGroup {
    let mut locks = Vec::new();
    rebuild(g, ctx.head, &mut locks);
    let per_side = usize::from(g.count.max(1));
    let shade = |n: usize| g.shades[usize::from(!(n % per_side).is_multiple_of(2))];
    let clumps = locks.iter().enumerate().map(|(n, lock)| ctx.clump(lock.to_vec(), g.width, g.thickness, shade(n)));
    CompiledGroup::tubes(clumps.collect())
}

/// Replaces `locks` with the group's locks, `+x` side first. Allocates nothing once `locks` has the capacity for
/// [`clump_count`] locks, so an editor that rebuilds on every slider move reuses one buffer.
pub fn rebuild(g: &PartSweepGen, head: &dyn HeadSurface, locks: &mut Vec<Lock>) {
    locks.clear();
    let mut controls = [DVec3::ZERO; CONTROL_POINTS];
    let mut spaced = [DVec3::ZERO; REST_POINTS];
    for side in [1.0, -1.0] {
        for i in 0..usize::from(g.count) {
            control_points(g, head, side, i, &mut controls);
            // The resample refuses only a non-finite or zero-length path; take the nearest control points then, so
            // the clump count the dispatch checks never changes.
            if spaced_points_into(&controls, &mut spaced).is_err() {
                for (k, p) in spaced.iter_mut().enumerate() {
                    *p = controls[(k * (CONTROL_POINTS - 1) + REST_POINTS / 2) / (REST_POINTS - 1)];
                }
            }
            locks.push(spaced.map(DVec3::as_vec3));
        }
    }
}

/// How many clumps [`compile`] makes: `count` locks on each side.
pub fn clump_count(g: &PartSweepGen) -> usize {
    2 * usize::from(g.count)
}

/// The nine control points of lock `i` on `side` (`+1` for +x, `-1` for -x), root first, written into `out`. Allocates
/// nothing.
#[allow(clippy::cast_precision_loss)] // lock indices are small
pub fn control_points(
    g: &PartSweepGen,
    head: &dyn HeadSurface,
    side: f64,
    i: usize,
    out: &mut [DVec3; CONTROL_POINTS],
) {
    let s = if g.count > 1 { i as f64 / f64::from(g.count - 1) } else { 0.0 };
    let start =
        (part_direction(g, g.part_start + (1.0 - g.part_start) * s) + DVec3::new(side * g.part_spread, 0.0, 0.0)).normalize();
    let azimuth = side * (g.end_from_deg + (g.end_to_deg - g.end_from_deg) * s.powf(g.end_spread)).to_radians();
    let blend = (g.height_blend_deg[0], g.height_blend_deg[1]);
    let end = height_direction(g.end_height_front, g.end_height_back, blend, head, azimuth);
    let alternate = if i.is_multiple_of(2) { 0.0 } else { g.alternate_step };
    for (k, point) in out.iter_mut().enumerate() {
        let t = k as f64 / (CONTROL_POINTS - 1) as f64;
        let lift = g.lift + g.belly * (std::f64::consts::PI * (t * BELLY_REACH).min(1.0)).sin() + alternate;
        *point = on_head(head, slerp(start, end, t.powf(g.sweep_ease)), lift);
    }
}

/// The unit direction `s` of the way along the part line (`0..=1`): in the plane `x = 0`, tilted back from vertical.
fn part_direction(g: &PartSweepGen, s: f64) -> DVec3 {
    let a = (g.part_front_deg + (g.part_back_deg - g.part_front_deg) * s).to_radians();
    DVec3::new(0.0, a.cos(), -a.sin())
}

/// The unit direction of the braid line at azimuth `phi` (radians from the front) with the default blend
/// ([`HEIGHT_BLEND`]). Shared with the braid-crown generator.
pub fn braid_direction(front: f64, back: f64, head: &dyn HeadSurface, phi: f64) -> DVec3 {
    height_direction(front, back, HEIGHT_BLEND, head, phi)
}

/// The unit direction of a ring round the head at azimuth `phi` (radians from the front): at that azimuth's height,
/// blended from `front` to `back` over the azimuth range `blend` (degrees), round the head.
pub fn height_direction(front: f64, back: f64, blend: (f64, f64), head: &dyn HeadSurface, phi: f64) -> DVec3 {
    let a = phi.sin().atan2(phi.cos()).abs().to_degrees();
    let blend = smoothstep(a, blend.0, blend.1);
    let height = front + (back - front) * blend;
    let crown = f64::from(head.radius(Vec3::Y)).max(1e-6);
    let theta = (height / crown).clamp(-1.0, 1.0).acos();
    DVec3::new(theta.sin() * phi.sin(), theta.cos(), theta.sin() * phi.cos())
}

/// The point on the head in direction `dir`, `offset` metres out.
fn on_head(head: &dyn HeadSurface, dir: DVec3, offset: f64) -> DVec3 {
    let unit = dir.as_vec3();
    head.centre().as_dvec3() + dir * (f64::from(head.radius(unit)) + offset)
}

/// `d0` turned `t` of the way toward `d1` about the axis perpendicular to both (three.js: `slerp` of the identity to
/// `setFromUnitVectors`, applied to `d0`).
fn slerp(d0: DVec3, d1: DVec3, t: f64) -> DVec3 {
    DQuat::IDENTITY.slerp(DQuat::from_rotation_arc(d0, d1), t) * d0
}
