//! Making way for pointed ears (#2449): locks that would cover an elf ear are routed behind it, so its tip shows
//! through the hair as anime draws it.
//!
//! **The ear.** Each pointed ear the head publishes ([`EarCover`]) is taken as a capsule from its root to its tip, its
//! radius [`EAR_REACH`] of the ear's half height plus [`MARGIN`]: the shell is flat, so the capsule is what a lock has to
//! clear, not the disc.
//!
//! **The way round.** A rest point inside a capsule is moved to its surface on the far side from the face: along the
//! part of `-z` (backwards) square to the capsule's axis, or, for a point already behind the axis, straight out from
//! it. The moves are smoothed along each lock ([`SMOOTHING`] passes of a `1 2 1` kernel, so a lock bends round the ear
//! instead of kinking) and the pass is repeated [`ROUNDS`] times, so smoothing never leaves a point back inside. A point
//! the move would take into the head is lifted back to [`CLEARANCE`] outside it. Roots never move.

use omuzaki_char_api::{HairClump, HeadSurface};
use omuzaki_core::glam::Vec3;

use crate::head::ears::EarCover;

/// How much of the ear's half height the capsule's radius is: the shell's width round its axis.
const EAR_REACH: f32 = 0.55;
/// Extra clearance between a lock's centre line and the ear, metres: about a lock's half width.
const MARGIN: f32 = 0.012;
/// Smoothing passes over each lock's moves.
const SMOOTHING: usize = 4;
/// Move-and-smooth rounds.
const ROUNDS: usize = 4;
/// How far outside the head a moved point is kept, metres.
const CLEARANCE: f32 = 0.004;

/// A pointed ear as the capsule a lock has to clear.
#[derive(Copy, Clone, Debug)]
struct Capsule {
    root: Vec3,
    tip: Vec3,
    radius: f32,
}

impl Capsule {
    /// The closest point of the axis to `p`.
    fn closest(&self, p: Vec3) -> Vec3 {
        let axis = self.tip - self.root;
        let length = axis.length_squared();
        let t = if length > 0.0 { ((p - self.root).dot(axis) / length).clamp(0.0, 1.0) } else { 0.0 };
        self.root + axis * t
    }

    /// How far `p`, the centre of a lock `half_width` wide there, has to move for the lock to clear the capsule round
    /// the back of the ear; zero when it already does.
    fn way_out(&self, p: Vec3, half_width: f32) -> Vec3 {
        let radius = self.radius + half_width;
        let on_axis = self.closest(p);
        let off = p - on_axis;
        let depth = radius - off.length();
        if depth <= 0.0 {
            return Vec3::ZERO;
        }
        let axis = (self.tip - self.root).normalize_or_zero();
        let back = (Vec3::NEG_Z - axis * Vec3::NEG_Z.dot(axis)).normalize_or_zero();
        let back = if back == Vec3::ZERO { Vec3::NEG_Z } else { back };
        if off.dot(back) > 0.0 && off.length() > 1e-6 {
            off.normalize() * depth
        } else {
            // In front of the axis (over the face side of the ear): go round the back, to the capsule's far surface.
            let across = off - back * off.dot(back);
            on_axis + across + back * (radius * radius - across.length_squared()).max(0.0).sqrt() - p
        }
    }
}

/// Routes every lock of `clumps` behind the pointed ears of `covers`, over `head`. Ears that aren't pointed, and an
/// empty list, change nothing.
#[allow(clippy::cast_precision_loss)] // point indices are small
pub fn make_way(covers: &[EarCover], head: &dyn HeadSurface, clumps: &mut [HairClump]) {
    let capsules: Vec<Capsule> = covers
        .iter()
        .filter(|c| c.pointed)
        .map(|c| Capsule { root: c.centre, tip: c.tip, radius: c.radius * EAR_REACH + MARGIN })
        .collect();
    if capsules.is_empty() {
        return;
    }
    let mut moves = Vec::new();
    for clump in clumps {
        for _ in 0..ROUNDS {
            moves.clear();
            let last = clump.rest.len().saturating_sub(1).max(1) as f32;
            moves.extend(clump.rest.iter().enumerate().map(|(i, p)| {
                let half_width = 0.5 * clump.width.at(i as f32 / last);
                capsules.iter().fold(Vec3::ZERO, |sum, c| sum + c.way_out(*p, half_width))
            }));
            if moves.iter().all(|m| *m == Vec3::ZERO) {
                break;
            }
            smooth(&mut moves);
            for (i, (p, m)) in clump.rest.iter_mut().zip(&moves).enumerate() {
                if i == 0 {
                    continue;
                }
                *p = outside(head, *p + *m);
            }
        }
    }
}

/// `moves` smoothed along the lock, each pass a `1 2 1` kernel that keeps the ends, never shrinking a move below
/// what it was before the pass, so a point that has to clear the ear still does.
fn smooth(moves: &mut [Vec3]) {
    let n = moves.len();
    if n < 3 {
        return;
    }
    let mut previous = moves.to_vec();
    for _ in 0..SMOOTHING {
        for i in 1..n - 1 {
            let mixed = (previous[i - 1] + previous[i] * 2.0 + previous[i + 1]) * 0.25;
            moves[i] = if mixed.length_squared() >= previous[i].length_squared() { mixed } else { previous[i] };
        }
        previous.copy_from_slice(moves);
    }
}

/// `p`, lifted to [`CLEARANCE`] outside `head` if it is inside.
fn outside(head: &dyn HeadSurface, p: Vec3) -> Vec3 {
    let from = p - head.centre();
    let dir = from.normalize_or_zero();
    if dir == Vec3::ZERO {
        return p;
    }
    let surface = head.radius(dir) + CLEARANCE;
    if from.length() < surface { head.centre() + dir * surface } else { p }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_point_in_front_of_the_ear_goes_round_the_back_and_one_behind_goes_straight_out() {
        let ear = Capsule { root: Vec3::new(0.1, 0.0, 0.0), tip: Vec3::new(0.12, 0.08, -0.02), radius: 0.03 };
        let front = Vec3::new(0.1, 0.02, 0.01);
        let moved = front + ear.way_out(front, 0.0);
        assert!(moved.z < front.z - 0.01, "it went backwards: {moved:?}");
        assert!((moved - ear.closest(moved)).length() >= ear.radius - 1e-4, "it left the capsule: {moved:?}");
        let behind = Vec3::new(0.1, 0.02, -0.015);
        let moved = behind + ear.way_out(behind, 0.0);
        assert!((moved - ear.closest(moved)).length() >= ear.radius - 1e-4);
        assert_eq!(ear.way_out(Vec3::new(0.3, 0.0, 0.0), 0.0), Vec3::ZERO, "a point clear of the ear stays");
        assert_ne!(ear.way_out(Vec3::new(0.15, 0.0, 0.0), 0.03), Vec3::ZERO, "a wide lock clears by its half width");
    }

    #[test]
    fn smoothing_spreads_a_move_and_never_shrinks_one() {
        let mut moves = vec![Vec3::ZERO, Vec3::ZERO, Vec3::X, Vec3::ZERO, Vec3::ZERO];
        smooth(&mut moves);
        assert_eq!(moves[2], Vec3::X);
        assert!(moves[1].x > 0.0 && moves[3].x > 0.0, "{moves:?}");
        assert_eq!((moves[0], moves[4]), (Vec3::ZERO, Vec3::ZERO), "the ends stay");
    }
}
