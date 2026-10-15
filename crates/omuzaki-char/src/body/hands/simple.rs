//! Simple hands: four fused fingers on the same block, parted by shallow grooves, and a splayed thumb. No decal lines:
//! the grooves show the fingers.

use super::Style;
use crate::kit::curve::sstep;

/// How much the thumb stands out more than a mitten's, degrees.
const THUMB_SPLAY_DEG: f64 = 18.0;
/// The grooves lie between the fingers, on the finger lines' offsets (`HAND.finger_across` is `+-0.01275`, `+-0.00425`).
const GROOVE_AT: [f64; 3] = [-0.0085, 0.0, 0.0085];
/// How deep (a fraction of the radius) and how wide (a Gaussian's scale, metres) a groove is.
const GROOVE_DEPTH: f64 = 0.22;
const GROOVE_WIDTH: f64 = 0.0022;
/// Where the grooves fade in (over the knuckle row) and out (towards the tip, where the cap closes).
const GROOVE_IN: (f64, f64) = (0.026, 0.038);
const GROOVE_OUT: (f64, f64) = (0.040, 0.049);

/// A simple hand's block is cut with finger grooves and its ring is finer, so the grooves have room.
pub(super) const STYLE: Style = Style { around: 30, splay_deg: THUMB_SPLAY_DEG, groove };

/// The radial scale at distance `u` along the hand and `lateral` across it: 1 away from a groove, less in one.
fn groove(u: f64, lateral: f64) -> f64 {
    let fade = sstep(GROOVE_IN.0, GROOVE_IN.1, u) * (1.0 - sstep(GROOVE_OUT.0, GROOVE_OUT.1, u));
    let dent: f64 = GROOVE_AT.iter().map(|g| (-((lateral - g) / GROOVE_WIDTH).powi(2)).exp()).sum();
    (1.0 - GROOVE_DEPTH * fade * dent).max(0.5)
}
