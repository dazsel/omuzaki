//! Eye aperture shape: the lid opening from openness, arc, tilt and lower lid (port of `aperture()` and the anchors of
//! `eye()` in the prototype's `face.js`, lines 99-125). Closed eyes (the relaxed lash curve and the happy ^ arc) are
//! drawn elsewhere; this module only says which eye is shut ([`Eye::lid`]).
//!
//! [`Eye`] is the shared geometry of one eye. It is a pure function of the profile, the paint parameters and the
//! side, so every piece of the eye (sclera, iris, lash, crease, ...) builds its own copy instead of passing one
//! around, and two pieces can never disagree about where the lid is.
//!
//! # Eye units
//!
//! The curves live in the prototype's *eye units*: `u` runs from the inner corner (-1) to the outer corner (+1), `v`
//! is up, and both are in multiples of the eye's half-width. [`Eye::point`] puts a `(u, v)` pair on the face plane;
//! the outer direction is +x for the character's left eye and -x for the right one.
//!
//! # From the JS constants to the profile's [`Aperture`]
//!
//! The prototype hard-codes one almond. The profile's [`Aperture`] describes a family of eyes, built so that the
//! numbers below reproduce the prototype's curves exactly. Every parameter replaces constants of `face.js`; its
//! value at the prototype's eye is the "JS value" column (the character files set these; the defaults are a generic
//! eye, a little flatter).
//!
//! | Parameter | Replaces (JS) | Mapping | JS value |
//! |---|---|---|---|
//! | `lower_depth` | `0.56` depth of `bot0` | `D = lower_depth` | 0.56 |
//! | `upper_height` | amplitude of `top0` | the curve is scaled so `top(0)` at `open = 1` is exactly `upper_height` | `0.447_78` |
//! | `roundness` | `0.72` exponent, `0.12` centre and `1.12` half-width of `bot0`; `0.2` centre and `0.08` bulge of `top0` | `exponent = 1 - roundness / 2`, `centre_b = 0.12 + (0.56 - roundness) 0.2`, `half_width = 1.12 + (0.56 - roundness) 0.12`, `centre_t = max(0, 0.2 + (0.56 - roundness) 0.45)`, `bulge = 0.08 + (roundness - 0.56) 0.1` | 0.56 |
//! | `inner_sharpness` | `0.22` width of the rise from the inner corner | `rise = 0.22 + (0.8 - inner_sharpness) 0.85` | 0.8 |
//! | `outer_bluntness` | `1.10` control point of the outer corner's quadratic | `control = 1 + 0.1 outer_bluntness / 0.6`; below 0.6 the lids also converge on the outer corner (up to 95 %) | 0.6 |
//! | `tilt` | (none; the prototype's eye is level) | shears both lids by `0.25 tilt` per unit of `u`; positive lifts the outer corner | 0 |
//!
//! The constants that stay the prototype's are named below. A round eye (`roundness` toward 1) has fuller lids and a
//! hump centred over the pupil; a flat almond (toward 0) has its hump moved toward the outer corner.

use libm::powf;
use omuzaki_core::glam::Vec2;
use omuzaki_face_api::{Aperture, DrawList, FaceProfile, PaintParams, PathRef};

use super::Side;

// ---- the prototype's constants that the profile does not parametrise (`face.js` `aperture()` and `eye()`)

/// Width of the eye at scale 1 grows the spacing by this fraction of the scale change (`ex` of `eye()`).
const SPACING_PER_SCALE: f64 = 0.35;
/// Samples per lid; the polygon has `N + 1` points on each lid.
const SAMPLES: usize = 44;
/// The lower lid's slope: `+ 0.02 u`.
const BOT_SLOPE: f32 = 0.02;
/// How far the cheek lifts the lower lid at the middle: `lowLid * 0.32 * (1 - u^2)`.
const LOW_LID_LIFT: f32 = 0.32;
/// The upper lid's resting offset at the inner corner.
const TOP_BASE: f32 = -0.02;
/// The upper lid's hump: base height, and the extra over the eye toward the outer corner.
const TOP_HUMP_BASE: f32 = 0.36;
/// See [`TOP_HUMP_BASE`]: `+ 0.06 (u + 1) / 2`.
const TOP_HUMP_SLOPE: f32 = 0.06;
/// Half-width of the hump's parabola.
const TOP_HUMP_WIDTH: f32 = 1.2;
/// Wide eyes (`open > 1`): the lid lifts by `(open - 1) * 0.42 * (1 - u^2)^0.6`.
const WIDE_LIFT: f32 = 0.42;
/// Exponent of the wide lift.
const WIDE_LIFT_EXPONENT: f32 = 0.6;
/// Angry tilt presses the inner corner down by this much per unit of tilt.
const ANGRY_PRESS: f32 = 0.20;
/// Sad tilt: the inner corner rises by this much ...
const SAD_INNER_UP: f32 = 0.16;
/// ... and the outer corner droops by this much, per unit of (negative) tilt.
const SAD_OUTER_DROP: f32 = 0.10;
/// The lids never meet closer than this, so a nearly shut eye keeps a hairline of white.
const MIN_GAP: f32 = 0.004;
/// How far from the inner corner, in eye units, a sad lid takes to open to its full gap.
const SAD_POINT_SPAN: f32 = 0.4;
/// The point is full from a tilt of `1 / SAD_POINT_RAMP` toward sad, so a faint sadness does not leave a half-open corner.
const SAD_POINT_RAMP: f32 = 4.0;
/// How much openness a squeeze fades the open eye over, above [`SHUT_BELOW`].
const HANDOVER_BAND: f32 = 0.2;
/// An eye whose openness is below this is shut (`op < 0.08`).
const SHUT_BELOW: f32 = 0.08;
/// A lid with `arc` at or above this is a happy ^ arc (`smileEye > 0.5`).
const ARC_FROM: f32 = 0.5;
/// An arc starts to show, over the open eye, from this value.
const ARC_BAND_FROM: f32 = 0.35;
/// Largest openness (the mapping's own bound).
const MAX_OPEN: f32 = 1.9;

// ---- the parametrisation: the profile's numbers at the prototype's eye (see the table above)

/// `roundness` at the prototype's eye.
const JS_ROUNDNESS: f64 = 0.56;
/// `inner_sharpness` at the prototype's eye.
const JS_INNER_SHARPNESS: f64 = 0.8;
/// `outer_bluntness` at the prototype's eye.
const JS_OUTER_BLUNTNESS: f64 = 0.6;
/// The control point of the outer corner at the prototype's eye (`1.10`), as the offset beyond `u = 1`.
const JS_OUTER_BULGE: f64 = 0.10;
/// How much the lids may converge on a pointed outer corner.
const MAX_TAPER: f64 = 0.95;
/// Shear of both lids per unit of resting tilt, per unit of `u`.
const TILT_SHEAR: f64 = 0.25;

/// A smooth 0 to 1 ramp (`ss01` of the prototype).
pub(super) fn smooth01(x: f32) -> f32 {
    let t = x.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// `max(0, 1 - x^2)`, the window both lids' curves are built on.
fn window(x: f32) -> f32 {
    (1.0 - x * x).max(0.0)
}

/// The profile's [`Aperture`] reduced to the numbers the lid curves use (see the module table).
#[derive(Copy, Clone, Debug, PartialEq)]
struct Shape {
    depth: f32,
    bot_exponent: f32,
    bot_centre: f32,
    bot_half_width: f32,
    rise: f32,
    top_scale: f32,
    top_centre: f32,
    top_bulge: f32,
    outer_control: f32,
    taper: f32,
    shear: f32,
}

impl Shape {
    #[allow(clippy::cast_possible_truncation)] // the profile is f64 for the file's sake; the painter works in f32
    fn new(a: &Aperture) -> Self {
        let round = a.roundness - JS_ROUNDNESS;
        let top_centre = (0.2 - round * 0.45).max(0.0);
        let top_bulge = 0.08 + round * 0.1;
        // `upper_height` is the lid's height at the middle, so scale the whole hump to hit it exactly.
        let hump = |u: f64| {
            f64::from(TOP_HUMP_BASE)
                + f64::from(TOP_HUMP_SLOPE) * (u + 1.0) / 2.0
                + top_bulge * (1.0 - ((u - top_centre) / f64::from(TOP_HUMP_WIDTH)).powi(2))
        };
        let top_scale = (a.upper_height - f64::from(TOP_BASE)) / hump(0.0);
        let blunt = (a.outer_bluntness / JS_OUTER_BLUNTNESS).max(0.0);
        Self {
            depth: a.lower_depth as f32,
            bot_exponent: (1.0 - a.roundness / 2.0) as f32,
            bot_centre: (0.12 - round * 0.2) as f32,
            bot_half_width: (1.12 - round * 0.12) as f32,
            rise: (0.22 + (JS_INNER_SHARPNESS - a.inner_sharpness) * 0.85).max(0.05) as f32,
            top_scale: top_scale as f32,
            top_centre: top_centre as f32,
            top_bulge: top_bulge as f32,
            outer_control: (1.0 + JS_OUTER_BULGE * blunt) as f32,
            taper: ((1.0 - blunt).max(0.0) * MAX_TAPER) as f32,
            shear: (a.tilt * TILT_SHEAR) as f32,
        }
    }
}

/// How an eye is drawn: open, or shut as one of two curves (drawn elsewhere).
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Lid {
    /// The aperture, eye white, iris and the rest.
    Open,
    /// Shut as a happy ^ arc (`arc >= 0.5`: ^^ eyes, a wink).
    Arc,
    /// Shut as the relaxed lash curve (a blink, or openness near 0).
    Relaxed,
}

/// One eye's geometry: where it is, how big, and the shape of its opening this frame.
#[derive(Copy, Clone, Debug)]
pub(super) struct Eye {
    side: Side,
    /// Centre on the face plane.
    centre: Vec2,
    /// Half-width in metres: one eye unit.
    width: f32,
    shape: Shape,
    /// Effective openness: `open`, closed by the blink, by the arc and by the squeeze, `0..=1.9`.
    open: f32,
    /// The same without the squeeze: how open the eye would be if it were not squeezed.
    unsqueezed: f32,
    tilt: f32,
    low_lid: f32,
    arc: f32,
}

impl Eye {
    /// The eye on `side` for this frame: anchors from the layout and eye style (`EX`, `EW`, spacing scaled by
    /// `1 + (scale - 1) 0.35`, lowered by `drop`), lids from the side's [`EyeParams`](omuzaki_face_api::EyeParams).
    #[allow(clippy::cast_possible_truncation)] // see `Shape::new`
    pub(super) fn new(profile: &FaceProfile, params: &PaintParams, side: Side) -> Self {
        let (layout, style) = (&profile.layout, &profile.eyes);
        let (sign, eye) = match side {
            Side::Left => (1.0, &params.left_eye),
            Side::Right => (-1.0, &params.right_eye),
        };
        let x = sign * layout.eye_spacing * (1.0 + (style.scale - 1.0) * SPACING_PER_SCALE);
        // A blink or wink closes the lid in proportion; an arc closes it too (the prototype's `1 - smileEye`).
        // A squeeze closes the lid too, as it turns the eye into a `>` or `<`.
        let squeeze = params.effects.squeeze.clamp(0.0, 1.0);
        let unsqueezed =
            eye.open.clamp(0.0, MAX_OPEN) * (1.0 - eye.close.clamp(0.0, 1.0)) * (1.0 - eye.arc.clamp(0.0, 1.0));
        let open = unsqueezed * (1.0 - squeeze);
        Self {
            side,
            centre: Vec2::new(x as f32, (layout.eye_y - style.drop) as f32),
            width: (layout.eye_width * style.scale) as f32,
            shape: Shape::new(&style.aperture),
            open,
            unsqueezed,
            tilt: params.eye_tilt,
            low_lid: eye.lower_lid,
            arc: eye.arc,
        }
    }

    /// Whether the squeeze (not a blink or an arc) is what is shutting this eye.
    fn squeeze_shut(&self, squeeze: f32) -> bool {
        squeeze > 0.0 && self.unsqueezed > SHUT_BELOW && self.arc < ARC_FROM
    }

    /// How much of the open eye's pieces show, `0..=1`. A squeeze that shuts an open eye fades them out over the
    /// last [`HANDOVER_BAND`] of openness before [`SHUT_BELOW`], where the closed curve takes over, so no piece
    /// vanishes in one step. Everything else is `1`.
    pub(super) fn open_fade(&self, squeeze: f32) -> f32 {
        let by_squeeze = if self.squeeze_shut(squeeze) && self.open < SHUT_BELOW + HANDOVER_BAND {
            ((self.open - SHUT_BELOW) / HANDOVER_BAND).clamp(0.0, 1.0)
        } else {
            1.0
        };
        by_squeeze * (1.0 - self.arc_growth_while_open())
    }

    /// How far an arc has grown, `0..=1`: `0` up to [`ARC_BAND_FROM`], then rising to `1` where the eye becomes the
    /// happy `^` ([`ARC_FROM`]). In between the open pieces fade out as the arc fades in.
    pub(super) fn arc_growth(&self) -> f32 {
        ((self.arc - ARC_BAND_FROM) / (ARC_FROM - ARC_BAND_FROM)).clamp(0.0, 1.0)
    }

    /// [`Eye::arc_growth`] while the open eye is still the one drawn, `0` otherwise.
    fn arc_growth_while_open(&self) -> f32 {
        if self.lid() == Lid::Open { self.arc_growth() } else { 0.0 }
    }

    /// Whether the closed curve is the happy arc: the eye is an arc, or an arc is growing over the open eye.
    pub(super) fn arc_like(&self) -> bool {
        self.lid() == Lid::Arc || self.arc_growth_while_open() > 0.0
    }

    /// How much of the closed curve shows when a squeeze (`squeeze`, `0..=1`) is what shut the eye: it comes in as the
    /// open pieces fade out (see [`Eye::open_fade`]), is at full strength where they are gone, and fades out to nothing
    /// at full squeeze, where the chevron has fully replaced it. An eye shut by a blink or an arc just fades with the
    /// squeeze.
    pub(super) fn closed_strength(&self, squeeze: f32) -> f32 {
        // Only an eye the squeeze itself is shutting crossfades; an arc, a blink or no squeeze keeps the plain fade.
        let arc_in = if self.lid() == Lid::Open { self.arc_growth() } else { 1.0 };
        if self.squeeze_shut(squeeze) {
            self.takeover(squeeze) * (1.0 - self.open_fade(squeeze))
        } else {
            (1.0 - squeeze.clamp(0.0, 1.0)) * arc_in
        }
    }

    /// The closed curve's share once it has taken over from an open eye the squeeze is shutting: full where the
    /// openness reaches [`SHUT_BELOW`], then fading out with the squeeze.
    fn takeover(&self, squeeze: f32) -> f32 {
        ((1.0 - squeeze.clamp(0.0, 1.0)) * self.unsqueezed / SHUT_BELOW).min(1.0)
    }

    /// Whether the closed curve is drawn: the eye is shut, or a squeeze is fading its open pieces out.
    pub(super) fn shows_closed(&self, squeeze: f32) -> bool {
        self.lid() != Lid::Open || self.open_fade(squeeze) < 1.0
    }

    /// How far the closed curve of an eye shut by the squeeze has moved from the open eye it took over from, `0..=1`:
    /// `0` where it takes over (it lies on the lens the open eye had shrunk to), `1` at full squeeze, where it is
    /// invisible anyway. An eye shut by a blink or an arc is `1`: the curve is drawn in its own place, as in the prototype.
    pub(super) fn handover_drift(&self, squeeze: f32) -> f32 {
        if self.squeeze_shut(squeeze) { 1.0 - self.takeover(squeeze) } else { 1.0 }
    }

    /// The middle of the aperture at `u`: halfway between the lids.
    pub(super) fn midline(&self, u: f32) -> f32 {
        f32::midpoint(self.top(u), self.bottom(u))
    }

    /// How the eye is drawn: the open aperture, or which shut curve (`closedEye`'s `dir`).
    pub(super) fn lid(&self) -> Lid {
        if self.arc >= ARC_FROM {
            Lid::Arc
        } else if self.open < SHUT_BELOW {
            Lid::Relaxed
        } else {
            Lid::Open
        }
    }

    /// True when the aperture is drawn.
    pub(super) fn is_open(&self) -> bool {
        self.lid() == Lid::Open
    }

    /// Half the eye's width in metres: one eye unit.
    pub(super) fn width(&self) -> f32 {
        self.width
    }

    /// `+1` for the character's left eye, whose outer corner is toward +x, and `-1` for the right.
    pub(super) fn outward(&self) -> f32 {
        match self.side {
            Side::Left => 1.0,
            Side::Right => -1.0,
        }
    }

    /// How far the cheek raises the lower lid, `0..=1` (the eye's `lower_lid` channel).
    pub(super) fn lower_lid(&self) -> f32 {
        self.low_lid
    }

    /// The face-plane point at eye coordinates `(u, v)`.
    pub(super) fn point(&self, u: f32, v: f32) -> Vec2 {
        Vec2::new(self.centre.x + self.outward() * u * self.width, self.centre.y + v * self.width)
    }

    /// The lower lid at `u` (`bot0`): a bowl whose depth and fullness come from the profile, raised by the cheek.
    pub(super) fn bottom(&self, u: f32) -> f32 {
        let s = &self.shape;
        -s.depth * powf(window((u - s.bot_centre) / s.bot_half_width), s.bot_exponent)
            + BOT_SLOPE * u
            + self.low_lid * LOW_LID_LIFT * (1.0 - u * u)
            + s.shear * u
    }

    /// The upper lid at the rest openness (`top0`), before the lid moves.
    fn top_at_rest(&self, u: f32) -> f32 {
        let s = &self.shape;
        let hump = TOP_HUMP_BASE
            + TOP_HUMP_SLOPE * (u + 1.0) / 2.0
            + s.top_bulge * (1.0 - ((u - s.top_centre) / TOP_HUMP_WIDTH).powi(2));
        TOP_BASE + smooth01((u + 1.0) / s.rise) * s.top_scale * hump
    }

    /// The upper lid at `u`: openness, tilt and the lower lid applied.
    pub(super) fn top(&self, u: f32) -> f32 {
        let (op, tilt) = (self.open, self.tilt);
        let lower = self.bottom(u) - self.shape.shear * u;
        let mut v = self.top_at_rest(u);
        if op > 1.0 {
            // Surprised: the lid lifts into a round eye.
            v += (op - 1.0) * WIDE_LIFT * powf(window(u), WIDE_LIFT_EXPONENT);
        }
        v = lower + (v - lower) * op.min(1.0);
        let lid_weight = (op * 2.0).min(1.0);
        // Angry: the inner corner is pressed down.
        v -= tilt * ANGRY_PRESS * (1.0 - u) * 0.5 * lid_weight;
        if tilt < 0.0 {
            // Sad: the inner corner rises and the outer one droops.
            v += -tilt * (SAD_INNER_UP * (1.0 - u) * 0.5 - SAD_OUTER_DROP * (1.0 + u) * 0.5) * lid_weight;
        }
        let gap = (v - lower).max(MIN_GAP);
        // A pointed outer corner pulls the lids together over the last stretch.
        let gap = (gap * (1.0 - self.shape.taper * smooth01((u - 0.55) / 0.45))).max(MIN_GAP);
        // Sad lifts the inner corner of the lid; the lids still meet there, in the tear duct's point, instead of the
        // aperture closing with a vertical edge.
        let sad = (-tilt * SAD_POINT_RAMP).clamp(0.0, 1.0);
        let gap = (gap * (1.0 - sad * (1.0 - smooth01((u + 1.0) / SAD_POINT_SPAN)))).max(MIN_GAP);
        lower + gap + self.shape.shear * u
    }

    /// Where the outer corner's quadratic bulges to, as `u` beyond the lids' ends.
    fn outer_control(&self) -> f32 {
        self.shape.outer_control
    }
}

/// Appends the aperture outline to `list`: the upper lid inner to outer in [`SAMPLES`] steps, a quadratic round the
/// outer corner, the lower lid back, closed. 92 path elements.
pub(super) fn outline(list: &mut DrawList, eye: &Eye) -> PathRef {
    let lid_point = |i: usize, lid: fn(&Eye, f32) -> f32| {
        // `i` is at most 44, exact in f32.
        #[allow(clippy::cast_precision_loss)]
        let u = -1.0 + 2.0 * i as f32 / SAMPLES as f32;
        eye.point(u, lid(eye, u))
    };
    let mut path = list.path().move_to(lid_point(0, Eye::top));
    for i in 1..=SAMPLES {
        path = path.line_to(lid_point(i, Eye::top));
    }
    let (top_end, bottom_end) = (eye.top(1.0), eye.bottom(1.0));
    path = path.quad_to(eye.point(eye.outer_control(), f32::midpoint(top_end, bottom_end)), eye.point(1.0, bottom_end));
    for i in (0..=SAMPLES).rev() {
        path = path.line_to(lid_point(i, Eye::bottom));
    }
    path.close().finish()
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // sample indices are at most 44
mod tests {
    use super::*;
    use omuzaki_face_api::{EyeParams, FaceProfile};

    /// The prototype's eye: the parameters that reproduce `face.js` (the table in the module docs).
    fn js_aperture() -> Aperture {
        Aperture {
            roundness: JS_ROUNDNESS,
            tilt: 0.0,
            inner_sharpness: JS_INNER_SHARPNESS,
            outer_bluntness: JS_OUTER_BLUNTNESS,
            // `top0(0)` of the prototype: -0.02 + 0.36 + 0.06 / 2 + 0.08 (1 - (0.2 / 1.2)^2).
            upper_height: -0.02 + 0.36 + 0.03 + 0.08 * (1.0 - (0.2_f64 / 1.2).powi(2)),
            lower_depth: 0.56,
        }
    }

    fn rest_params(open: f32, tilt: f32, low_lid: f32) -> PaintParams {
        let eye = EyeParams { open, close: 0.0, arc: 0.0, lower_lid: low_lid };
        let mut p = omuzaki_face_api::FaceMapping::default().rest_params();
        p.left_eye = eye;
        p.right_eye = eye;
        p.eye_tilt = tilt;
        p
    }

    fn eye(aperture: Aperture, open: f32, tilt: f32, low_lid: f32) -> Eye {
        let mut profile = FaceProfile::default();
        profile.eyes.aperture = aperture;
        Eye::new(&profile, &rest_params(open, tilt, low_lid), Side::Left)
    }

    /// `top(u)` and `bot(u)` of `face.js`' `aperture({ open, tilt, lowLid })` at [`JS_US`], evaluated in node on the
    /// prototype's own function (`tools/` carries no copy: the function is sliced out of `face.js` and run).
    const JS_US: [f32; 5] = [-1.0, -0.5, 0.0, 0.6, 1.0];
    /// `(open, tilt, lowLid, top, bot)`.
    type JsCase = (f32, f32, f32, [f32; 5], [f32; 5]);
    // Printed by node as 7 decimals; separators would only make them harder to diff against the script's output.
    #[allow(clippy::unreadable_literal, clippy::excessive_precision)]
    const JS_CASES: [JsCase; 7] = [
        (
            1.0,
            0.0,
            0.0,
            [-0.016, 0.4077778, 0.4477778, 0.4591111, 0.4444444],
            [-0.02, -0.4402966, -0.5553640, -0.4718715, -0.2604192],
        ),
        (
            1.9,
            0.0,
            0.0,
            [-0.016, 0.7258521, 0.8257778, 0.7483121, 0.4444444],
            [-0.02, -0.4402966, -0.5553640, -0.4718715, -0.2604192],
        ),
        (
            0.5,
            0.85,
            0.0,
            [-0.016, -0.1437594, -0.1387931, -0.0403802, 0.0920126],
            [-0.02, -0.4402966, -0.5553640, -0.4718715, -0.2604192],
        ),
        (
            1.0,
            -1.1,
            0.22,
            [0.376, 0.6772778, 0.5907778, 0.4503111, 0.3344444],
            [-0.02, -0.3874966, -0.4849640, -0.4268155, -0.2604192],
        ),
        (
            0.04,
            0.0,
            0.0,
            [-0.016, -0.4063736, -0.5152383, -0.4346322, -0.2322247],
            [-0.02, -0.4402966, -0.5553640, -0.4718715, -0.2604192],
        ),
        (
            1.85,
            0.0,
            0.1,
            [-0.016, 0.7081813, 0.8047778, 0.7322454, 0.4444444],
            [-0.02, -0.4162966, -0.5233640, -0.4513915, -0.2604192],
        ),
        (
            0.6,
            0.5,
            0.3,
            [-0.016, 0.0223480, 0.0349211, 0.0912941, 0.1624990],
            [-0.02, -0.3682966, -0.4593640, -0.4104315, -0.2604192],
        ),
    ];

    #[test]
    fn the_prototypes_eye_is_reproduced() {
        for (open, tilt, low_lid, top, bot) in JS_CASES {
            let e = eye(js_aperture(), open, tilt, low_lid);
            for (i, u) in JS_US.into_iter().enumerate() {
                // The one place this port departs from the prototype: a sad lid still meets the lower lid at the inner
                // corner (see `sad_eyes_end_in_a_point_at_the_inner_corner`), where the prototype's closes with an edge.
                if tilt < 0.0 && (u + 1.0).abs() < 1e-6 {
                    continue;
                }
                assert!(
                    (e.top(u) - top[i]).abs() < 1e-5,
                    "top({u}) at open {open}, tilt {tilt}: {} vs {}",
                    e.top(u),
                    top[i]
                );
                assert!(
                    (e.bottom(u) - bot[i]).abs() < 1e-5,
                    "bot({u}) at low lid {low_lid}: {} vs {}",
                    e.bottom(u),
                    bot[i]
                );
            }
        }
    }

    #[test]
    fn sad_eyes_end_in_a_point_at_the_inner_corner() {
        // The aperture closes at `u = -1` with the lids at most 0.02 eye units apart for every tilt, so the corner is a
        // tear-duct point and never a vertical edge; and the lid still opens to its full gap a little way in.
        for tilt in [-1.1, -1.0, -0.5, -0.25, 0.0, 0.5, 1.0] {
            let e = eye(js_aperture(), 1.0, tilt, 0.0);
            let corner = e.top(-1.0) - e.bottom(-1.0);
            assert!(corner < 0.02, "tilt {tilt}: the lids are {corner} apart at the inner corner");
        }
        let sad = eye(js_aperture(), 1.0, -1.0, 0.0);
        assert!(sad.top(-0.4) - sad.bottom(-0.4) > 0.2, "the sad lid opens past the point");
    }

    // ---- properties

    /// The outline's corner points, with the outer quadratic flattened into 8 segments.
    fn flatten(list: &DrawList, path: PathRef) -> Vec<Vec2> {
        let mut points: Vec<Vec2> = Vec::new();
        for el in list.elements(path) {
            match *el {
                omuzaki_face_api::PathEl::Move(p) | omuzaki_face_api::PathEl::Line(p) => points.push(p),
                omuzaki_face_api::PathEl::Quad(c, end) => {
                    let start = points.last().copied().unwrap_or(end);
                    for i in 1..=8_u8 {
                        let t = f32::from(i) / 8.0;
                        points.push(start * (1.0 - t) * (1.0 - t) + c * (2.0 * t * (1.0 - t)) + end * (t * t));
                    }
                }
                omuzaki_face_api::PathEl::Cubic(..) | omuzaki_face_api::PathEl::Close => {}
            }
        }
        points
    }

    fn cross(a: Vec2, b: Vec2, c: Vec2) -> f32 {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    }

    /// True if segments `ab` and `cd` cross at a point interior to both.
    fn properly_cross((a, b): (Vec2, Vec2), (c, d): (Vec2, Vec2)) -> bool {
        let sides_of_cd = cross(c, d, a) * cross(c, d, b);
        let sides_of_ab = cross(a, b, c) * cross(a, b, d);
        sides_of_cd < 0.0 && sides_of_ab < 0.0
    }

    fn assert_simple(points: &[Vec2]) {
        let count = points.len();
        for i in 0..count {
            for j in i + 2..count {
                if i == 0 && j == count - 1 {
                    continue; // neighbours across the closing edge
                }
                let (first, second) = ((points[i], points[(i + 1) % count]), (points[j], points[(j + 1) % count]));
                assert!(!properly_cross(first, second), "edges {i} and {j} cross");
            }
        }
    }

    fn outline_of(eye: &Eye) -> Vec<Vec2> {
        let mut list = DrawList::new();
        let path = outline(&mut list, eye);
        flatten(&list, path)
    }

    use proptest::prelude::*;

    fn any_aperture() -> impl Strategy<Value = Aperture> {
        (0.0..=1.0, -1.0..=1.0, 0.0..=1.0, 0.0..=1.0, 0.1..=1.0, 0.1..=1.0).prop_map(
            |(roundness, tilt, inner_sharpness, outer_bluntness, upper_height, lower_depth)| Aperture {
                roundness,
                tilt,
                inner_sharpness,
                outer_bluntness,
                upper_height,
                lower_depth,
            },
        )
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn any_valid_aperture_and_lids_give_a_finite_closed_simple_outline(
            aperture in any_aperture(),
            open in 0.0_f32..=1.9,
            tilt in -2.0_f32..=2.0,
            low_lid in 0.0_f32..=1.0,
        ) {
            let e = eye(aperture, open, tilt, low_lid);
            let points = outline_of(&e);
            prop_assert_eq!(points.len(), 2 * (SAMPLES + 1) + 8);
            prop_assert!(points.iter().all(|p| p.is_finite()));
            assert_simple(&points);
            // The lids never cross: the top is above the bottom everywhere.
            for i in 0..=SAMPLES {
                let u = -1.0 + 2.0 * i as f32 / SAMPLES as f32;
                prop_assert!(e.top(u) >= e.bottom(u) + MIN_GAP - 1e-6, "lids cross at u = {u}");
            }
        }

        #[test]
        fn a_shut_lid_collapses_to_a_line(aperture in any_aperture(), tilt in -2.0_f32..=2.0, low_lid in 0.0_f32..=1.0) {
            let e = eye(aperture, 0.0, tilt, low_lid);
            for i in 0..=SAMPLES {
                let u = -1.0 + 2.0 * i as f32 / SAMPLES as f32;
                prop_assert!(e.top(u) - e.bottom(u) <= MIN_GAP + 1e-6, "open 0 leaves a gap at u = {u}");
            }
        }
    }

    /// Height over width of the aperture's bounding box.
    fn aspect(e: &Eye) -> f32 {
        let points = outline_of(e);
        let (lo, hi) =
            points.iter().fold((Vec2::splat(f32::MAX), Vec2::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        (hi.y - lo.y) / (hi.x - lo.x)
    }

    #[test]
    fn a_wide_eye_is_rounder_than_a_rest_eye() {
        let rest = aspect(&eye(js_aperture(), 1.0, 0.0, 0.0));
        let wide = aspect(&eye(js_aperture(), 1.9, 0.0, 0.0));
        assert!(wide > rest * 1.3, "open 1.9 aspect {wide} vs open 1 aspect {rest}");
    }

    #[test]
    fn roundness_runs_from_an_almond_to_a_round_eye() {
        let almond = aspect(&eye(Aperture { roundness: 0.0, ..js_aperture() }, 1.0, 0.0, 0.0));
        let round = aspect(&eye(
            Aperture { roundness: 1.0, upper_height: 0.7, lower_depth: 0.7, ..js_aperture() },
            1.0,
            0.0,
            0.0,
        ));
        assert!(round > almond * 1.3 && round > 0.6, "round {round}, almond {almond}");
    }

    #[test]
    fn resting_tilt_lifts_or_drops_the_outer_corner() {
        let level = eye(js_aperture(), 1.0, 0.0, 0.0);
        let cat = eye(Aperture { tilt: 1.0, ..js_aperture() }, 1.0, 0.0, 0.0);
        let droop = eye(Aperture { tilt: -1.0, ..js_aperture() }, 1.0, 0.0, 0.0);
        assert!(cat.bottom(1.0) > level.bottom(1.0) && cat.top(1.0) > level.top(1.0));
        assert!(droop.bottom(1.0) < level.bottom(1.0) && droop.top(1.0) < level.top(1.0));
        assert!(cat.bottom(-1.0) < level.bottom(-1.0), "the inner corner goes the other way");
    }

    #[test]
    fn the_outer_corner_goes_from_pointed_to_blunt() {
        let width_at_outer = |bluntness: f64| {
            let e = eye(Aperture { outer_bluntness: bluntness, ..js_aperture() }, 1.0, 0.0, 0.0);
            e.top(1.0) - e.bottom(1.0)
        };
        assert!(width_at_outer(0.0) < 0.05 * width_at_outer(0.6) + 0.03);
        assert!(
            (width_at_outer(0.6) - width_at_outer(1.0)).abs() < 1e-6,
            "blunter than the prototype's does not change the lids"
        );
    }

    #[test]
    fn upper_height_is_the_lid_height_at_the_middle() {
        for roundness in [0.0, 0.3, 0.56, 1.0] {
            let e = eye(Aperture { roundness, upper_height: 0.5, ..js_aperture() }, 1.0, 0.0, 0.0);
            assert!((e.top(0.0) - 0.5).abs() < 1e-5, "roundness {roundness}: {}", e.top(0.0));
        }
    }

    fn lid_of(open: f32, close: f32, arc: f32) -> Lid {
        let mut params = rest_params(open, 0.0, 0.0);
        params.left_eye = EyeParams { open, close, arc, lower_lid: 0.0 };
        Eye::new(&FaceProfile::default(), &params, Side::Left).lid()
    }

    #[test]
    fn the_lid_is_shut_by_a_blink_an_arc_or_no_openness() {
        assert_eq!(lid_of(1.0, 0.0, 0.0), Lid::Open);
        assert_eq!(lid_of(0.0, 0.0, 0.0), Lid::Relaxed);
        assert_eq!(lid_of(1.0, 1.0, 0.0), Lid::Relaxed, "a blink");
        assert_eq!(lid_of(1.0, 0.95, 0.0), Lid::Relaxed, "a nearly complete blink");
        assert_eq!(lid_of(1.0, 0.0, 0.5), Lid::Arc, "arc 0.5 is a happy arc");
        assert_eq!(lid_of(1.0, 1.0, 1.0), Lid::Arc, "the arc wins over a blink");
        assert_eq!(lid_of(1.0, 0.5, 0.0), Lid::Open, "half a blink is a narrow open eye");
    }

    #[test]
    fn the_eyes_sit_where_the_layout_puts_them() {
        let mut profile = FaceProfile::default();
        profile.layout.eye_spacing = 0.09;
        profile.layout.eye_y = -0.1;
        profile.layout.eye_width = 0.05;
        profile.eyes.scale = 1.5;
        profile.eyes.drop = 0.01;
        let params = rest_params(1.0, 0.0, 0.0);
        let (left, right) = (Eye::new(&profile, &params, Side::Left), Eye::new(&profile, &params, Side::Right));
        // Spacing grows by a third of the scale change (0.35 in the prototype), the size by all of it.
        assert!((left.point(0.0, 0.0).x - 0.09 * 1.175).abs() < 1e-6);
        assert!((right.point(0.0, 0.0).x + 0.09 * 1.175).abs() < 1e-6);
        assert!((left.point(0.0, 0.0).y + 0.11).abs() < 1e-6);
        assert!(
            (left.point(1.0, 0.0).x - left.point(0.0, 0.0).x - 0.075).abs() < 1e-6,
            "the outer corner is further from the nose"
        );
        assert!(right.point(1.0, 0.0).x < right.point(0.0, 0.0).x);
    }

    #[test]
    fn the_right_eye_is_the_left_eye_mirrored() {
        let params = rest_params(1.3, 0.4, 0.2);
        let profile = FaceProfile::default();
        let (left, right) = (Eye::new(&profile, &params, Side::Left), Eye::new(&profile, &params, Side::Right));
        for (l, r) in outline_of(&left).into_iter().zip(outline_of(&right)) {
            assert!((l.x + r.x).abs() < 1e-6 && (l.y - r.y).abs() < 1e-6);
        }
    }
}
