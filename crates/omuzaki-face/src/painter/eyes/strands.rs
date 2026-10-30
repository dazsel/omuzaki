//! Strand lashes (#2468): individual tapered lashes along the upper and the lower lid, drawn just before the lash line
//! so the line covers their roots, for an open eye only (they fade with the rest of the open eye as it shuts).
//!
//! Each strand is a curved lens from its root on the lid to its tip: it leaves the lid along the lid's normal, leans
//! toward the outer corner by [`StrandRow::lift`] (the outer lashes the most), bends further as it grows by
//! [`StrandRow::curl`], and gathers its tip toward its clump's by [`StrandRow::clumping`]. Lengths and places are in eye
//! units, so lashes grow with the eye. Every strand of a row is one sub-path of one fill, so a row costs one command
//! whatever its count; the pattern is a pure function of the eye's seed and the same on both eyes (mirrored).
//!
//! [`StrandRow::lift`]: omuzaki_face_api::StrandRow::lift
//! [`StrandRow::curl`]: omuzaki_face_api::StrandRow::curl
//! [`StrandRow::clumping`]: omuzaki_face_api::StrandRow::clumping

use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Cmd, FaceLayerId, Paint, StrandRow, narrow};

use super::aperture::Eye;
use super::{FALLBACK_COLOUR, Side};
use crate::painter::marks::Rng;
use crate::painter::{PaintCtx, fixed_colour};

/// The rows' streams, mixed into the eye's seed.
const UPPER_STREAM: u32 = 0x9b05_688c;
const LOWER_STREAM: u32 = 0x1f83_d9ab;
/// Strands per clump.
const CLUMP: u32 = 4;
/// The largest lean toward the outer corner at full lift, in radians, and the inner lashes' lean the other way.
const LEAN: f32 = 1.05;
const INNER_LEAN: f32 = 0.25;
/// The largest extra bend along a strand at full curl, in radians.
const CURL: f32 = 0.9;
/// Where along a strand its control point sits.
const CONTROL: f32 = 0.5;
/// Below this an opacity counts as off.
const OFF: f32 = 0.005;

/// Which lid a row grows from.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Lid {
    Upper,
    Lower,
}

/// Paints both rows of strands of the eye on `side`; nothing for a shut eye or a row with no strands.
pub(super) fn paint(ctx: &mut PaintCtx<'_>, side: Side) {
    let eye = Eye::new(ctx.profile, ctx.params, side);
    if !eye.is_open() {
        return;
    }
    let lash = &ctx.profile.eyes.lash;
    let colour = fixed_colour(&lash.colour, FALLBACK_COLOUR);
    let seed = ctx.profile.eyes.seed;
    let (upper, lower) = (lash.strands.upper.clone(), lash.strands.lower.clone());
    for (row, lid, stream) in [(&upper, Lid::Upper, UPPER_STREAM), (&lower, Lid::Lower, LOWER_STREAM)] {
        let opacity = narrow(row.opacity).clamp(0.0, 1.0);
        if row.count == 0 || opacity < OFF {
            continue;
        }
        let list = ctx.list(FaceLayerId::Base);
        let mut path = list.path();
        let mut rng = Rng::new(seed ^ stream);
        let mut clump_tip = Vec2::ZERO;
        for i in 0..row.count {
            let strand = Strand::new(&eye, row, lid, i, &mut rng);
            if i % CLUMP == 0 {
                // The clump's tip is its first strand's; the others gather toward it.
                clump_tip = strand.tip;
            }
            let pull = narrow(row.clumping).clamp(0.0, 1.0) * 0.6;
            let tip = strand.tip + (clump_tip - strand.tip) * pull;
            let control = strand.control + (clump_tip - strand.tip) * pull * 0.4;
            let side = strand.across * strand.half_width;
            path = path
                .move_to(eye.point(strand.root.x - side.x, strand.root.y - side.y))
                .quad_to(eye.point(control.x - side.x * 0.6, control.y - side.y * 0.6), eye.point(tip.x, tip.y))
                .quad_to(
                    eye.point(control.x + side.x * 0.6, control.y + side.y * 0.6),
                    eye.point(strand.root.x + side.x, strand.root.y + side.y),
                )
                .close();
        }
        let path = path.finish();
        list.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity });
    }
}

/// One strand in eye units: its root on the lid, its curve's control point, its tip, the direction across it and its
/// half-width at the root.
struct Strand {
    root: Vec2,
    control: Vec2,
    tip: Vec2,
    across: Vec2,
    half_width: f32,
}

impl Strand {
    /// The `i`th strand of `row` on `lid`. Draws the same number of values from `rng` for every strand, so the
    /// pattern of one strand never depends on another's settings.
    fn new(eye: &Eye, row: &StrandRow, lid: Lid, i: u32, rng: &mut Rng) -> Self {
        let (jitter, grow, sway) = (rng.unit(), rng.unit(), rng.unit());
        let random = narrow(row.randomness).clamp(0.0, 1.0);
        let from = narrow(row.from).clamp(-1.0, 0.95);
        let to = 0.99;
        // At most 200 strands, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let t = ((i as f32 + 0.5 + (jitter - 0.5) * random) / row.count as f32).clamp(0.0, 1.0);
        let u = from + (to - from) * t;
        let lid_at = |u: f32| match lid {
            Lid::Upper => eye.top(u),
            Lid::Lower => eye.bottom(u),
        };
        let root = Vec2::new(u, lid_at(u));
        // The lid's tangent and the normal away from the eye.
        let step = 0.02;
        let tangent =
            (Vec2::new(u + step, lid_at(u + step)) - Vec2::new(u - step, lid_at(u - step))).normalize_or_zero();
        let away = match lid {
            Lid::Upper => Vec2::new(-tangent.y, tangent.x),
            Lid::Lower => Vec2::new(tangent.y, -tangent.x),
        };
        let lift = narrow(row.lift).clamp(0.0, 1.0);
        // Positive leans toward the outer corner (+u): the outer lashes the most, the innermost a little inward.
        let lean = lift * (LEAN * t.powf(1.4) - INNER_LEAN * (1.0 - t)) + (sway - 0.5) * random * 0.35;
        let lean = match lid {
            Lid::Upper => lean,
            Lid::Lower => -lean,
        };
        let dir = rotate(away, -lean);
        let length = narrow(row.length)
            * (0.5 + 0.5 * t.powf(0.7))
            * (1.0 + 0.35 * lift * t)
            * (1.0 + (grow - 0.5) * random * 0.8);
        let curl = narrow(row.curl).clamp(0.0, 1.0) * CURL;
        // The strand bends further outward as it grows.
        let bend = match lid {
            Lid::Upper => -curl,
            Lid::Lower => curl,
        };
        let control = root + dir * (length * CONTROL);
        let tip = control + rotate(dir, bend) * (length * (1.0 - CONTROL));
        let half_width = 0.5 * narrow(row.thickness) / eye.width().max(1e-6);
        Self { root, control, tip, across: Vec2::new(-dir.y, dir.x), half_width }
    }
}

/// `v` turned by `angle` radians counter-clockwise in eye units (`u` right toward the outer corner, `v` up).
fn rotate(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = (libm::sinf(angle), libm::cosf(angle));
    Vec2::new(c * v.x - s * v.y, s * v.x + c * v.y)
}
