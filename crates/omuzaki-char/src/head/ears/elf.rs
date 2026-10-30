//! Elf ears: the human shell drawn out into a pointed tip that sweeps up, slightly back and out.
//!
//! The shell scales by `size` as the human ear does. The tip is `Ears::tip_length` metres long when set (#823), and
//! otherwise [`TIP_PER_SIZE`] metres per unit of size, held between [`MIN_TIP`] and [`MAX_TIP`].
//!
//! The tip leans back by `Ears::tip_angle_deg` (default [`TIP_ANGLE_DEG`]) and out by [`TIP_OUT_DEG`], both *in the
//! plane of the shell*, so it stays clear of the head however the ear flares.
//!
//! The outline's top (`a = pi / 2`) is pulled along the ear's height by a falloff that is linear in the angle, which
//! leaves a point rather than a round end; the pull scales with the row's rim factor so the concha stays put.

use std::f64::consts::FRAC_PI_2;

use omuzaki_char_api::{Ears, HeadSurface};
use omuzaki_core::glam::{DVec3, Vec3};
use omuzaki_core::mesh::{MeshData, MeshError};

use super::{Frame, Side, human};

/// Tip length per unit of `ears.size`, metres.
pub const TIP_PER_SIZE: f64 = 0.09;
/// The shortest tip (a small elf ear is still pointed), metres.
pub const MIN_TIP: f64 = 0.04;
/// The longest tip, metres.
pub const MAX_TIP: f64 = 0.14;
/// How far the tip leans back from straight up, degrees.
pub const TIP_ANGLE_DEG: f64 = Ears::TIP_ANGLE_DEG;
/// How far the tip leans away from the head (towards the side), degrees. Above the ear the head widens up to its
/// widest at the cranium's middle, so a tip that only rose would end inside it on a small ear.
pub const TIP_OUT_DEG: f64 = 40.0;
/// Half the angle (radians) of the outline the tip draws out, either side of the top.
const TIP_HALF_ARC: f64 = 0.7;
/// How sharply the pull falls off from the top: above 1 keeps the flanks concave-in, so the tip is slender.
const TIP_SHARPNESS: f64 = 1.6;

/// The length of the tip of an ear of `size`, metres.
#[must_use]
pub fn tip_length(size: f64) -> f64 {
    (TIP_PER_SIZE * size).clamp(MIN_TIP, MAX_TIP)
}

/// The length of `ears`' tip: its own when set, else the one its size gives.
#[must_use]
pub fn tip_length_of(ears: &Ears) -> f64 {
    ears.tip_length.map_or_else(|| tip_length(ears.size), |length| length.clamp(MIN_TIP, MAX_TIP))
}

/// How much of the tip's pull the outline at angle `a` gets: `1` at the top, `0` beyond [`TIP_HALF_ARC`].
fn pull(a: f64) -> f64 {
    (1.0 - (a - FRAC_PI_2).abs() / TIP_HALF_ARC).max(0.0).powf(TIP_SHARPNESS)
}

/// `direction` projected into the shell's plane and made a unit vector.
fn in_plane(frame: &Frame, direction: DVec3) -> DVec3 {
    (direction - frame.normal * direction.dot(frame.normal)).normalize_or_zero()
}

/// The direction the tip points: the ear's height leaned back by `back_deg` and out, in the shell's plane (so the tip
/// never turns towards the head however the ear flares).
fn sweep(frame: &Frame, side: Side, back_deg: f64) -> DVec3 {
    let back = in_plane(frame, DVec3::NEG_Z);
    let out = in_plane(frame, DVec3::X * side.sign());
    let (angle, lean) = (back_deg.clamp(0.0, 45.0).to_radians(), TIP_OUT_DEG.to_radians());
    ((frame.up * angle.cos() + back * angle.sin()) * lean.cos() + out * lean.sin()).normalize_or_zero()
}

/// The direction the `side` ear's tip points, for what has to make way for it.
#[must_use]
pub fn tip_direction(ears: &Ears, side: Side) -> DVec3 {
    sweep(&Frame::of(ears, side), side, ears.tip_angle_deg)
}

/// The elf ear of `ears.size`, tip and orientation on the `side` of the head, rooted at `root`. The lean out of the head is
/// a look, not a guarantee: a small ear at a low flare still ends in the head, so the shell is pushed out of it where
/// it would ([`human::CLEARANCE`]).
///
/// # Errors
/// [`MeshError`] if the size makes a vertex that is not finite.
pub fn build(ears: &Ears, side: Side, root: Vec3, head: &dyn HeadSurface) -> Result<MeshData, MeshError> {
    let frame = Frame::of(ears, side);
    let (length, direction) = (tip_length_of(ears), sweep(&frame, side, ears.tip_angle_deg));
    human::shell(&frame, ears.size, root, head, &|a, rim| direction * (length * pull(a) * rim))
}
