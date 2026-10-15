//! Mitten hands: a smooth palm-and-finger block, a separate thumb, and three thin finger-hint lines on the back of the
//! hand (`body.js` `buildHand`'s decals).

use omuzaki_core::glam::DVec3;

use super::{BACK_PLUMP, Block, Hand, KitError, Style, count, palm_skin};
use crate::kit::curve::clamp;
use crate::kit::grid::{GridOptions, MeshBuilder, Skin};
use crate::kit::sweep::tube;

/// A mitten's block has no grooves and a thumb at the template's rest angle.
pub(super) const STYLE: Style = Style { around: 24, splay_deg: 0.0, groove: |_, _| 1.0 };

/// The lines' lateral offsets from the axis (symmetric about 0: they sit where the fingers part), where they start and
/// end along the hand, how much they converge towards the tip (a fraction of the offset), their tube radius, and how
/// far they lie off the surface.
const LINE_OFFSETS: [f64; 3] = [-0.0085, 0.0, 0.0085];
const LINE_START: f64 = 0.030;
const LINE_END: f64 = 0.046;
const LINE_CONVERGE: f64 = 0.15;
const LINE_RADIUS: f64 = 0.0006;
const LINE_LIFT: f64 = 0.0004;
/// Points along a line, and segments round its tube.
const LINE_POINTS: usize = 8;
const LINE_SEGMENTS: usize = 5;

/// The finger-hint lines of the left hand: thin tubes lying a small lift off the back, with the hand's skin weights.
pub(super) fn lines(hand: &Hand, block: &Block) -> Result<MeshBuilder, KitError> {
    let mut builder = MeshBuilder::new();
    // The back of the hand is the side against the palm normal (`-N`); its height at a lateral offset follows the
    // section's ellipse, as the body's does.
    let surface = |u: f64, lat: f64| {
        let (a, b) = block.section(u);
        let s = clamp(lat / a, -0.98, 0.98);
        let back = b * BACK_PLUMP * (1.0 - s * s).sqrt();
        hand.place(u, lat, Hand::curl(u) - (back + LINE_LIFT))
    };
    let last = count(LINE_POINTS - 1);
    for offset in LINE_OFFSETS {
        let at = |k: usize| {
            let f = count(k) / last;
            (LINE_START + (LINE_END - LINE_START) * f, offset * (1.0 - LINE_CONVERGE * f))
        };
        let points: Vec<DVec3> = (0..LINE_POINTS).map(|k| surface(at(k).0, at(k).1)).collect();
        let sweep = tube(&points, |_| hand.size * LINE_RADIUS, LINE_SEGMENTS)?;
        let inside = |i: usize, _j: usize| sweep.centres[i];
        let skin = |_p: DVec3, i: usize, _j: usize| {
            let (along, across) = at(i);
            palm_skin(along, across)
        };
        builder.add_grid(
            &sweep.grid,
            &GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&skin), ..GridOptions::default() },
        )?;
    }
    Ok(builder)
}
