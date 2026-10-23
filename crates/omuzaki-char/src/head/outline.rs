//! The lower face: the face-shape outline, the chin and the cheek puff (CB-10).
//!
//! The cranium ([`super::cranium`]) is a ball; the face a character sheet draws is a soft U, an egg or a heart. The lower
//! face is reshaped from the cranium in four steps, each smooth in height and in every parameter, so that every slider
//! position and every blend between two face shapes gives a smooth, plausible head:
//!
//! 1. **Plausible space.** [`plausible`] clamps the outline to the face shapes a stylised anime head can have on its
//!    cranium. The limits are the constants below; the tuned face profiles sit inside them. Every head then has a chin:
//!    the explicit [`Chin`], or the point where the outline closes (`top - height`).
//! 2. **Chin.** The lower face is stretched vertically so its lowest vertex lands exactly on the chin. The stretch is
//!    full on the face and a third of that toward the back, so the jaw stays tucked under, and it eases in (C2) below
//!    the stretch line. This is the prototype's `stretchLowerFace` (`head.js` 106-121); a chin a little above the
//!    cranium's own lowest point compresses instead.
//! 3. **Outline.** Every height is scaled sideways so the front silhouette is the outline, the superellipse
//!    [`half_width_at`], from its top down to the chin. The cranium's width at each height is measured on the continuous
//!    stretched surface, not the mesh, then averaged over 1 cm, as the prototype's smoothed 2 mm bins did but without
//!    their steps. The result is the same at any grid resolution and moves smoothly with every parameter. The ratio
//!    of widths is held smoothly within `min_ratio..=2`, which keeps the chin's tip round. Above the outline's top the
//!    scale eases out into the untouched cranium over a blend band (the eye-line join, below).
//! 4. **Cheeks.** A chubby `cheek_puff` pushes the front of the cheeks forward, placed relative to the outline.
//!
//! **The eye-line join.** The outline starts straight down at its top, where the ball above is already
//! narrowing, so the side of the face must turn from the cranium's slope to vertical above the top and back in below
//! it: an S-bend. Squeezed into the prototype's 3 cm ([`FADE`]) that turn read as a ledge across the face in every view
//! and a notch in the front silhouette. The band is instead as tall as the turn needs (`blend_band`): it grows with
//! the slope mismatch at the top, up to [`MAX_BAND`], and eases in C2, so the curvature changes gradually at both of
//! its ends. [`MAX_PULL_IN`] and the other limits keep every head's turn near the tuned profiles'.
//!
//! The close-up profile (local test material) is inside the plausible space. It lands within 0.9 mm of the prototype's
//! mesh: the prototype's binned widths read up to 1% narrow, which pushed its face up to 0.9 mm outside its own outline,
//! and this construction follows the outline. Other profiles differ from the prototype where it creased, flanged or
//! spiked (T-GEOM parity waived for the lower face, recorded).
//!
//! Everything works on head-local points (origin at the head centre, +Y up, +Z forward) in `f64`.

use std::f64::consts::{FRAC_PI_2, PI};

use omuzaki_char_api::{Chin, Cranium, HeadOutline};
use omuzaki_core::glam::DVec3;

use super::cranium;
use crate::kit::curve::{Pchip, clamp, sstep};

// ------------------------------------------------------------------------------------------------ the plausible space

/// Where the outline's top (the eye line, where the taper toward the chin begins) may sit: this range times the
/// cranium's front lower half-height (`half_height_down_front`). It is raised further where the cranium would narrow
/// faster above it than [`MAX_PULL_IN`] allows.
pub const TOP_RANGE: (f64, f64) = (-0.52, -0.38);
/// How wide the outline's top may be: this range times the cranium's own front-view half-width at that height. Wider
/// bulges the face out of the cranium; narrower pinches a waist under the cheekbones.
pub const WIDTH_RANGE: (f64, f64) = (0.955, 1.0);
/// How far the face may turn in above the outline's top, as a fraction of the cranium's half-width: the cranium's width
/// [`FADE`] above the top minus the outline's. The tuned close-up profile's is 0.112.
pub const MAX_PULL_IN: f64 = 0.113;
/// How long the lower face (outline top to chin) may be: this range times `half_height_down_front`.
pub const LENGTH_RANGE: (f64, f64) = (0.40, 0.80);
/// Where the chin may sit relative to the cranium's own lowest point: this range times `half_height_down_front`. A chin
/// stretched further reads as a stalk or a beak; one pushed up flattens the cranium's bottom into a ridge.
pub const CHIN_RANGE: (f64, f64) = (-0.15, 0.02);
/// How far above the cranium's lowest point the stretch line must be, times `half_height_down_front`: a stretch that
/// starts lower draws a sliver of the bottom into a stalk.
pub const MIN_STRETCH_DEPTH: f64 = 0.40;
/// Where the stretch line may sit relative to the outline's top, metres: a line well above the top shears the face
/// where it turns into the cranium, one well below pulls a beak out of the chin.
pub const STRETCH_LINE_RANGE: (f64, f64) = (-0.02, 0.016);
/// The stretch line of a head without an explicit chin, relative to the outline's top, metres.
pub const IMPLICIT_STRETCH_LINE: f64 = 0.0;
/// The superellipse exponent's range: below it the jaw runs to a V, above it to a box. The lower end is a V-jaw's, held
/// by the chin tip ([`MIN_TIP_DEG`]) rather than the crease: with [`OutlineCurve`]'s round start the worst crease at the
/// eye line on the test grid's corners is 15.8 degrees at 1.4, 14.8 at 1.475 and 13.9 at 1.55 (limit 19), where the
/// bare superellipse creased them 27.3, 22.9 and 19.2 (#1687).
pub const EXPONENT_RANGE: (f64, f64) = (1.4, 2.4);
/// The lower exponent's range ([`OutlineCurve`]): wider than [`EXPONENT_RANGE`] at the bottom, since the curve it shapes
/// starts at the cheek line and cannot crease the eye line. Below about 1.2 the chin tip limit ([`MIN_TIP_DEG`]) binds
/// before the floor does: at 1.0 a superellipse is a straight V. The floor lets a full-cheeked face end on a pointed
/// chin (the heart preset, #1688).
pub const LOWER_EXPONENT_RANGE: (f64, f64) = (1.0, 2.4);
/// The most the built head may stray outside a pointed outline near its chin, metres ([`TipFit`]): under the
/// head-quality flange limit (3 mm), with room for the mesh's own sampling.
const TIP_MISS: f64 = 0.0025;
/// How far above the chin [`TipFit::miss`] looks, as a fraction of the lower face's length: the ratio floor acts only
/// near the tip.
const TIP_REACH: f64 = 0.3;
/// The pointiest the chin may be in the front view: the outline's own tip angle, measured as the head-quality tests
/// measure the mesh (on a circle of a quarter of the half-width round the chin), degrees. The exponent is raised until
/// the outline meets it. 5 degrees above the pointiest CC0 anime reference's (130 after rounding down).
pub const MIN_TIP_DEG: f64 = 135.0;
/// The range of `Chin::min_ratio`, the least the outline may narrow a height of the cranium. It acts only near the
/// chin's tip, where the outline closes faster than the cranium's round bottom.
pub const MIN_RATIO_RANGE: (f64, f64) = (0.6, 0.75);
/// The `min_ratio` of a head without an explicit chin.
pub const IMPLICIT_MIN_RATIO: f64 = 0.6;
/// The most the cheeks may puff forward, metres.
pub const MAX_CHEEK_PUFF: f64 = 0.010;

// ------------------------------------------------------------------------------------------------ the construction

/// The prototype's fade, metres: the narrowest blend band above the outline's top, and the reach over which the slope
/// mismatch and [`MAX_PULL_IN`] are measured.
pub const FADE: f64 = 0.03;
/// The tallest blend band above the outline's top, metres: above `top + MAX_BAND` every head is its untouched cranium.
/// Measured: a 6 cm band brings the join's crease (about 11 degrees at worst) down to the rest of the lower
/// face's, so a taller one would gain little and reshape more of the side of the head.
pub const MAX_BAND: f64 = 0.06;
/// How tall the blend band is per unit of slope mismatch, metres. The band's turn per metre of height is then about
/// the mismatch over the band, the same `1 / BAND_PER_MISMATCH` on every head between [`FADE`] and [`MAX_BAND`]. Chosen
/// so the heads held at the narrowest band and those at the tallest end with about the same crease.
const BAND_PER_MISMATCH: f64 = 0.12;
/// How far below the head centre the band must stop, metres: the width profile only covers the lower half, which the
/// band must not leave even on the smallest cranium.
const BAND_CLEARANCE: f64 = 0.01;
/// Depth below the stretch line over which the stretch eases in, metres (the prototype's).
const STRETCH_EASE: f64 = 0.03;
/// The least the chin stretch may compress the lower face (a stretch factor below 1 moves it up); below about 0.39 the
/// fastest-moving band would fold. [`CHIN_RANGE`] keeps real compressions far above it.
const MIN_STRETCH: f64 = 0.45;
/// The stretch factor the bisection starts with as its upper bound, and the largest it may grow to.
const STRETCH_START: f64 = 3.0;
const STRETCH_LIMIT: f64 = 1.0e5;
/// Bisection steps for the stretch: far below a `f32` position's precision on any bracket.
const BISECTION_STEPS: u32 = 48;
/// Bisection steps for the exponent and the tip.
const SOLVE_STEPS: u32 = 40;
/// The tip circle's radius as a fraction of the outline's half-width, for [`MIN_TIP_DEG`].
const TIP_SCALE: f64 = 0.25;
/// The width profile's sampling: meridians over the right half of the head, steps along each over the lower hemisphere
/// (every height the profile is asked about is below the head centre, and the stretch only moves points down), and the
/// height step, metres. Fine enough that the profile is smooth to a small fraction of a millimetre.
const MERIDIANS: usize = 180;
const MERIDIAN_STEPS: usize = 180;
const PROFILE_STEP: f64 = 0.001;
/// The most heights the profile holds: far more than any valid head spans, so no input makes it allocate without bound.
const MAX_PROFILE_STEPS: usize = 4096;
/// Where [`WidthProfile::smoothed`] samples, relative to the height asked about, metres: a 1 cm window.
const SMOOTHING_TAPS: [f64; 5] = [-0.004, -0.002, 0.0, 0.002, 0.004];
/// Below this cranium half-width (metres) a height is the chin's very tip, and its points are left where they are.
const TIP_WIDTH: f64 = 1e-6;
/// The most the outline may widen a height of the cranium, as a ratio of widths.
const MAX_RATIO: f64 = 2.0;
/// How softly the ratio limits engage: the width of the blend between the ratio and a limit.
const RATIO_SOFTNESS: f64 = 0.02;
/// Cheek puff placement, as fractions of the outline: the bulge's centre sits this far down the lower face and this far
/// out across the half-width, with these Gaussian widths. On the contract's default outline these are the prototype's
/// fixed numbers (0.640 m on its 0.778 m head centre, 0.098 m out, 0.024 by 0.042 m).
const PUFF_DOWN: f64 = 0.38;
const PUFF_OUT: f64 = 0.65;
const PUFF_SIGMA: (f64, f64) = (0.24, 0.28);
/// The narrowest the bulge may be, metres: a short or narrow face keeps a soft cheek rather than a lump.
const PUFF_MIN_SIGMA: (f64, f64) = (0.024, 0.042);

/// Half the width of the outline at head-local height `y` ([`OutlineCurve`]): full above the top, 0 below the chin.
pub fn half_width_at(outline: &HeadOutline, y: f64) -> f64 {
    OutlineCurve::of(outline).at(y)
}

/// The front outline of the lower face: half its width at each height, from the outline's top (the eye line, `t = 0`)
/// to the chin ([`HeadOutline::chin_y`], `t = 1`).
///
/// - **The superellipse.** `W0 * (1 - t^P)^(1/P)` with `P` the outline's `exponent`.
/// - **A round start.** Below an exponent of 2 the superellipse turns infinitely sharply as it leaves the top (its
///   slope grows like `t^(P - 1)`), which creases the face at the eye line however gently the blend band joins it to
///   the cranium. `t^P` is replaced by `((t^2 + e^2)^(P/2) - e^P)`, normalised to 1 at the chin, with `e`
///   [`ROUND_PER_EXPONENT`] times how far `P` is below 2: quadratic (so of finite curvature) within about `e` of the
///   top and the superellipse beyond it. At 2 and above `e` is 0 and the curve is the superellipse exactly (#1687).
/// - **The lower exponent.** With `exponent_lower`, the curve below the cheek line ([`CHEEK_LINE`], half way down)
///   blends from the upper curve into the superellipse of that exponent (`W0 * (1 - t^Pl)^(1/Pl)`) by a C2 step that
///   is 0 at the cheek line and 1 at the chin: the cheeks keep the upper exponent's fullness down to the cheek line,
///   the chin takes the lower one's point, and width, slope and curvature are continuous at the join (#1688). A lower
///   superellipse that started at the cheek line with the upper curve's slope could not: near an exponent of 1 it is
///   a straight line almost from its top, so it turned a corner there.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OutlineCurve {
    half_width: f64,
    top: f64,
    length: f64,
    exponent: f64,
    round: f64,
    lower: Option<f64>,
}

/// Where the lower exponent starts to take over, as the fraction `t` of the way from the outline's top to the chin:
/// half way, the height mid-face fullness is measured at.
pub const CHEEK_LINE: f64 = 0.5;
/// How round the outline's start is per unit of exponent below 2, as a fraction of the lower face's length. 0.25 was
/// enough on the default cranium; the wide crania the face-shape presets' proportions reach (#1690) need 0.35.
pub const ROUND_PER_EXPONENT: f64 = 0.35;

impl OutlineCurve {
    /// The curve `outline` asks for.
    #[must_use]
    pub fn of(outline: &HeadOutline) -> Self {
        let exponent = outline.exponent;
        Self {
            half_width: outline.half_width,
            top: outline.top,
            length: outline.top - outline.chin_y(),
            exponent,
            round: ROUND_PER_EXPONENT * (2.0 - exponent).max(0.0),
            lower: outline.exponent_lower,
        }
    }

    /// Half the width at head-local height `y`.
    #[must_use]
    pub fn at(&self, y: f64) -> f64 {
        if self.length <= 0.0 {
            return if y >= self.top { self.half_width } else { 0.0 };
        }
        self.half_width * self.fraction(clamp((self.top - y) / self.length, 0.0, 1.0))
    }

    /// The width at `t` over the width at the top.
    fn fraction(&self, t: f64) -> f64 {
        let upper = (1.0 - self.power(t)).max(0.0).powf(1.0 / self.exponent);
        match self.lower {
            Some(lower) if t > CHEEK_LINE => {
                let w = smoother(CHEEK_LINE, 1.0, t);
                (1.0 - w) * upper + w * superellipse(t, lower)
            }
            _ => upper,
        }
    }

    /// `t^P`, with the round start.
    fn power(&self, t: f64) -> f64 {
        if self.round <= 0.0 {
            return t.powf(self.exponent);
        }
        let half = 0.5 * self.exponent;
        let e2 = self.round * self.round;
        let base = e2.powf(half);
        ((t * t + e2).powf(half) - base) / ((1.0 + e2).powf(half) - base)
    }
}

/// `(1 - u^p)^(1/p)`.
fn superellipse(u: f64, p: f64) -> f64 {
    (1.0 - u.powf(p)).max(0.0).powf(1.0 / p)
}

/// The outline a head is built from: `outline` clamped into the plausible space of a stylised anime head on `cranium`.
///
/// The limits are this module's constants. In the order they are applied:
/// - the top within [`TOP_RANGE`], raised where [`MAX_PULL_IN`] demands;
/// - its width within [`WIDTH_RANGE`] of the cranium's and within [`MAX_PULL_IN`];
/// - the chin within [`CHIN_RANGE`] of the cranium's lowest point and [`LENGTH_RANGE`] below the top (the chin moves;
///   the top stays);
/// - the stretch line within [`STRETCH_LINE_RANGE`] of the top and [`MIN_STRETCH_DEPTH`] above the cranium's bottom;
/// - the exponent within [`EXPONENT_RANGE`] and the lower exponent, if set, within [`LOWER_EXPONENT_RANGE`]; the one
///   that shapes the chin (the lower one when set) raised until the front chin is no pointier than [`MIN_TIP_DEG`];
/// - `min_ratio` within [`MIN_RATIO_RANGE`] and the puff at most [`MAX_CHEEK_PUFF`].
///
/// An outline inside the space comes back with the same numbers. Every limit is continuous in the parameters, so a
/// slider dragged past a limit stops there rather than jumping.
///
/// The result always has a chin: the explicit one, or (without one) the point where the outline closes, `top - height`,
/// with the stretch line [`IMPLICIT_STRETCH_LINE`] above the top and the ratio floor [`IMPLICIT_MIN_RATIO`]. Its
/// `height` is `top - chin.y`, so its [`HeadOutline::chin_y`] is where the built head's lowest vertex lands: the head
/// part publishes its face projection from this outline. The cranium's widths are the analytic cranium's
/// ([`cranium::radius`]), so a traced head is clamped by the same numbers.
pub fn plausible(outline: &HeadOutline, cranium: &Cranium) -> HeadOutline {
    let down = cranium.half_height_down_front;
    let own = WidthProfile::new(&|dir| cranium::radius(cranium, dir), &Stretch::NONE, 0.0);
    let lowest = own.lowest;
    let pull_in = MAX_PULL_IN * cranium.half_width;
    // The lowest top at which even the widest outline turns in no more than the pull-in limit.
    let mut lowest_top = TOP_RANGE.0 * down;
    while lowest_top < TOP_RANGE.1 * down && own.at(lowest_top + FADE) - WIDTH_RANGE.1 * own.at(lowest_top) > pull_in {
        lowest_top += PROFILE_STEP;
    }
    let top = clamp(outline.top, lowest_top.min(TOP_RANGE.1 * down), TOP_RANGE.1 * down);
    let width = own.at(top);
    let widest = WIDTH_RANGE.1 * width;
    let narrowest = (WIDTH_RANGE.0 * width).max(own.at(top + FADE) - pull_in).min(widest);
    let half_width = clamp(outline.half_width, narrowest, widest);
    let deepest = (top - LENGTH_RANGE.1 * down).max(lowest + CHIN_RANGE.0 * down);
    let highest = (top - LENGTH_RANGE.0 * down).min(lowest + CHIN_RANGE.1 * down).max(deepest);
    let chin_y = clamp(outline.chin_y(), deepest, highest);
    let length = top - chin_y;
    let line = outline.chin.map_or(top + IMPLICIT_STRETCH_LINE, |c| c.stretch_top);
    let stretch_top =
        clamp(line, (top + STRETCH_LINE_RANGE.0).max(lowest + MIN_STRETCH_DEPTH * down), top + STRETCH_LINE_RANGE.1);
    let min_ratio =
        outline.chin.map_or(IMPLICIT_MIN_RATIO, |c| clamp(c.min_ratio, MIN_RATIO_RANGE.0, MIN_RATIO_RANGE.1));
    blunt(
        HeadOutline {
            half_width,
            top,
            height: length,
            exponent: clamp(outline.exponent, EXPONENT_RANGE.0, EXPONENT_RANGE.1),
            exponent_lower: outline.exponent_lower.map(|e| clamp(e, LOWER_EXPONENT_RANGE.0, LOWER_EXPONENT_RANGE.1)),
            chin: Some(Chin { y: chin_y, stretch_top, min_ratio }),
            cheek_puff: clamp(outline.cheek_puff, 0.0, MAX_CHEEK_PUFF),
        },
        cranium,
    )
}

/// The outline's front tip angle, degrees: on the circle of radius [`TIP_SCALE`] x the half-width round the chin, the
/// angle between the two points where the outline ([`OutlineCurve`]) crosses it.
fn tip_angle(outline: &HeadOutline) -> f64 {
    let curve = OutlineCurve::of(outline);
    let (chin, length) = (outline.chin_y(), outline.top - outline.chin_y());
    let radius = TIP_SCALE * outline.half_width;
    // Along the outline from the chin up: the point a fraction `u` of the length above it.
    let at = |u: f64| (curve.at(chin + u * length), u * length);
    let (mut low, mut high) = (0.0, 1.0);
    for _ in 0..SOLVE_STEPS {
        let mid = f64::midpoint(low, high);
        let (x, y) = at(mid);
        if x.hypot(y) < radius {
            low = mid;
        } else {
            high = mid;
        }
    }
    let (x, y) = at(high);
    2.0 * x.atan2(y).to_degrees()
}

/// `outline` (with a chin) with the exponent that shapes its chin (the lower one when it has one, else the exponent)
/// raised, up to the top of its range, until the tip is at least [`MIN_TIP_DEG`] and the head built on `cranium` can
/// follow the outline into the chin to within [`TIP_MISS`] ([`TipFit`]).
fn blunt(outline: HeadOutline, cranium: &Cranium) -> HeadOutline {
    let Some(fit) = TipFit::new(&outline, cranium) else {
        return outline;
    };
    let with = |p: f64| match outline.exponent_lower {
        Some(_) => HeadOutline { exponent_lower: Some(p), ..outline },
        None => HeadOutline { exponent: p, ..outline },
    };
    let fits = |o: &HeadOutline| tip_angle(o) >= MIN_TIP_DEG && fit.miss(o) <= TIP_MISS;
    if fits(&outline) {
        return outline;
    }
    let ceiling = if outline.exponent_lower.is_some() { LOWER_EXPONENT_RANGE.1 } else { EXPONENT_RANGE.1 };
    let (mut low, mut high) = (outline.exponent_lower.unwrap_or(outline.exponent), ceiling);
    for _ in 0..SOLVE_STEPS {
        let mid = f64::midpoint(low, high);
        if fits(&with(mid)) {
            high = mid;
        } else {
            low = mid;
        }
    }
    with(high)
}

/// How closely a head can follow a pointed outline into its chin. Near the tip the outline may close in faster than
/// the cranium's round bottom, and the ratio floor (`min_ratio`) then holds the head's width at that fraction of the
/// cranium's: the head is rounder there than the outline. [`TipFit::miss`] predicts by how much from the stretched
/// cranium's width profile, which the chin's exponent does not change, so it is measured once per outline.
struct TipFit {
    profile: WidthProfile,
    min_ratio: f64,
}

impl TipFit {
    /// The fit for `outline`'s chin on the analytic `cranium`, or `None` for an outline without a chin.
    fn new(outline: &HeadOutline, cranium: &Cranium) -> Option<Self> {
        let chin = outline.chin?;
        let radius = |dir: DVec3| cranium::radius(cranium, dir);
        let points: Vec<DVec3> = (0..=MERIDIANS)
            .flat_map(|j| {
                let phi = PI * fraction(j, MERIDIANS);
                (0..=MERIDIAN_STEPS).map(move |i| direction(FRAC_PI_2 * (1.0 + fraction(i, MERIDIAN_STEPS)), phi))
            })
            .map(|dir| dir * radius(dir))
            .collect();
        let stretch = Stretch::solve(&points, &chin);
        Some(Self { profile: WidthProfile::new(&radius, &stretch, outline.top), min_ratio: chin.min_ratio })
    }

    /// How far the head strays outside `outline` in the [`TIP_REACH`] above its chin, metres: at each height, the
    /// width the sideways scale gives (`side_scale`'s ratio, floor included, times the cranium's width) beyond the
    /// outline's, measured across the outline (divided by the length of its normal).
    fn miss(&self, outline: &HeadOutline) -> f64 {
        let curve = OutlineCurve::of(outline);
        let chin = outline.chin_y();
        let reach = TIP_REACH * (outline.top - chin);
        let mut worst = 0.0_f64;
        let mut y = chin + PROFILE_STEP;
        while y <= chin + reach {
            let current = self.profile.smoothed(y);
            if current > TIP_WIDTH {
                let wanted = curve.at(y);
                let ratio = soft_max(soft_min(wanted / current, MAX_RATIO), self.min_ratio);
                let slope = (curve.at(y + PROFILE_STEP) - curve.at(y - PROFILE_STEP)) / (2.0 * PROFILE_STEP);
                worst = worst.max((self.profile.at(y) * ratio - wanted) / slope.hypot(1.0));
            }
            y += PROFILE_STEP;
        }
        worst
    }
}

// ------------------------------------------------------------------------------------------------ the chin stretch

/// The vertical stretch that lands the lowest vertex on the chin: points below `line` move down by `(k - 1)` times
/// their drag (up, for `k < 1`).
#[derive(Clone, Copy, Debug)]
struct Stretch {
    line: f64,
    k: f64,
}

impl Stretch {
    /// No stretch at all.
    const NONE: Self = Self { line: 0.0, k: 1.0 };

    /// How far `p` moves for each unit of `k - 1`: its depth below the line, eased in over the first 3 cm, fully on the
    /// face and a third of that toward the back.
    fn drag(&self, p: DVec3) -> f64 {
        let below = self.line - p.y;
        if below > 0.0 { below * ease(below) * (0.35 + 0.65 * sstep(-0.04, 0.10, p.z)) } else { 0.0 }
    }

    fn apply(&self, p: DVec3) -> DVec3 {
        DVec3::new(p.x, p.y - (self.k - 1.0) * self.drag(p), p.z)
    }

    /// The factor that puts the lowest of `points` on `chin.y`, within [`MIN_STRETCH`] and [`STRETCH_LIMIT`].
    fn solve(points: &[DVec3], chin: &Chin) -> Self {
        let line = chin.stretch_top;
        let drags: Vec<f64> = points.iter().map(|p| Self { line, k: 1.0 }.drag(*p)).collect();
        let lowest = |k: f64| points.iter().zip(&drags).map(|(p, d)| p.y - (k - 1.0) * d).fold(f64::INFINITY, f64::min);
        let natural = lowest(1.0);
        let (mut low, mut high) = if natural > chin.y {
            let mut high = STRETCH_START;
            while lowest(high) > chin.y {
                high *= 2.0;
                if high > STRETCH_LIMIT {
                    return Self { line, k: 1.0 };
                }
            }
            (1.0, high)
        } else if natural < chin.y {
            if lowest(MIN_STRETCH) < chin.y {
                return Self { line, k: MIN_STRETCH };
            }
            (MIN_STRETCH, 1.0)
        } else {
            return Self { line, k: 1.0 };
        };
        // `lowest` falls as k grows: keep `low` above the chin and `high` at or below it.
        for _ in 0..BISECTION_STEPS {
            let mid = f64::midpoint(low, high);
            if lowest(mid) > chin.y {
                low = mid;
            } else {
                high = mid;
            }
        }
        Self { line, k: low }
    }
}

// ------------------------------------------------------------------------------------------------ the width profile

/// The unit direction at `theta` from the top pole and `phi` from the front toward +X (the mesh's convention).
fn direction(theta: f64, phi: f64) -> DVec3 {
    let (sin_theta, cos_theta) = theta.sin_cos();
    let (sin_phi, cos_phi) = phi.sin_cos();
    DVec3::new(sin_theta * sin_phi, cos_theta, sin_theta * cos_phi)
}

#[allow(clippy::cast_precision_loss)] // grid indices are tiny
fn fraction(k: usize, n: usize) -> f64 {
    k as f64 / n as f64
}

/// The stretched cranium's front-view half-width as a smooth function of height, from its lowest point up: the widest
/// point of each height on a fine triangulation of the surface, every [`PROFILE_STEP`], squared (so the rounded bottom
/// is a straight line rather than a square root) and joined by a monotone cubic, so nothing overshoots at the chin.
#[derive(Debug)]
struct WidthProfile {
    squared: Option<Pchip>,
    /// Height of the surface's lowest point.
    lowest: f64,
}

impl WidthProfile {
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded counts
    fn new(radius: &dyn Fn(DVec3) -> f64, stretch: &Stretch, high: f64) -> Self {
        // The stretched surface along each meridian of the right half, from the equator to the bottom pole.
        let meridians: Vec<Vec<DVec3>> = (0..=MERIDIANS)
            .map(|j| {
                let phi = PI * fraction(j, MERIDIANS);
                (0..=MERIDIAN_STEPS)
                    .map(|i| {
                        let dir = direction(FRAC_PI_2 * (1.0 + fraction(i, MERIDIAN_STEPS)), phi);
                        stretch.apply(dir * radius(dir))
                    })
                    .collect()
            })
            .collect();
        // The first height is the surface's lowest point, where the width is exactly 0: the squared width then rises
        // in a straight line from the first knot rather than bending through a knot below the surface.
        let low = meridians.iter().flatten().map(|p| p.y).fold(f64::INFINITY, f64::min);
        let count = (((high - low) / PROFILE_STEP).ceil().max(1.0) as usize).min(MAX_PROFILE_STEPS);
        // The widest x at each height is where the plane of that height cuts the triangulated grid, so on one of its
        // edges: along the meridians, across them, and the diagonals that split each cell. Meridians alone are not
        // enough. Below the head the stretch pulls the front lower than the pole, so each meridian there dips and
        // rises again, and the widest crossing would vanish in a jump as a dip's floor passed a height. The cut of a
        // surface changes continuously, so the profile moves smoothly with every parameter.
        let mut widest = vec![0.0_f64; count + 1];
        for (j, meridian) in meridians.iter().enumerate() {
            let next = meridians.get(j + 1);
            for (i, &from) in meridian.iter().enumerate() {
                let edges = [meridian.get(i + 1), next.and_then(|n| n.get(i)), next.and_then(|n| n.get(i + 1))];
                for &to in edges.into_iter().flatten() {
                    widen_crossings(&mut widest, from, to, low);
                }
            }
        }
        let knots: Vec<(f64, f64)> =
            widest.iter().enumerate().map(|(k, w)| (low + k as f64 * PROFILE_STEP, w * w)).collect();
        Self { squared: Pchip::new(&knots).ok(), lowest: low }
    }

    /// The half-width at height `y`.
    fn at(&self, y: f64) -> f64 {
        self.squared.as_ref().map_or(0.0, |s| s.eval(y).max(0.0).sqrt())
    }

    /// The half-width the outline is compared with at height `y`: the root mean square of five samples 2 mm apart
    /// round `y`, a 1 cm window like the prototype's five smoothed 2 mm bins (what its close-up profile was tuned
    /// with), without their steps. Below the lowest point the samples are mirrored back up, so the window stays full
    /// and continuous into the chin's tip, where it keeps the ratio from running to 0 or infinity.
    fn smoothed(&self, y: f64) -> f64 {
        let mean = SMOOTHING_TAPS
            .iter()
            .map(|dy| {
                let t = y + dy;
                self.squared_at(self.lowest + (t - self.lowest).abs())
            })
            .sum::<f64>()
            * fraction(1, SMOOTHING_TAPS.len());
        mean.max(0.0).sqrt()
    }

    fn squared_at(&self, y: f64) -> f64 {
        self.squared.as_ref().map_or(0.0, |s| s.eval(y).max(0.0))
    }
}

/// A smooth `max(a, b)`: within about [`RATIO_SOFTNESS`] of each other the two blend, so a limit adds no crease.
fn soft_max(a: f64, b: f64) -> f64 {
    0.5 * (a + b + ((a - b) * (a - b) + RATIO_SOFTNESS * RATIO_SOFTNESS).sqrt())
}

/// A smooth `min(a, b)`, as [`soft_max`].
fn soft_min(a: f64, b: f64) -> f64 {
    0.5 * (a + b - ((a - b) * (a - b) + RATIO_SOFTNESS * RATIO_SOFTNESS).sqrt())
}

// ------------------------------------------------------------------------------------------------ the lower face

/// How the stretch eases in below its line: from 0 at the line to full [`STRETCH_EASE`] below it. C2 (a quintic step,
/// where the prototype's is cubic), so a long stretch leaves no curvature step across the face where the easing ends.
fn ease(below: f64) -> f64 {
    smoother(0.0, STRETCH_EASE, below)
}

/// A C2 step from 0 at `a` to 1 at `b` (zero slope and curvature at both ends).
fn smoother(a: f64, b: f64, x: f64) -> f64 {
    let t = clamp((x - a) / (b - a), 0.0, 1.0);
    t * t * t * (t * (6.0 * t - 15.0) + 10.0)
}

/// Reshapes the lower face of the head-local cranium `points` (the mesh's vertices, pushed out by `radius`) to
/// `outline`, which must come from [`plausible`] (so it has a chin): stretches the chin onto `outline.chin`, scales
/// each height sideways so the front silhouette is the outline, and puffs the cheeks.
///
/// `radius` is the surface the points lie on (the cranium with any trace blended in); the width profile is measured on
/// it, not on the points, so the shape does not depend on the grid. Without a chin `points` are left as they are and
/// the result is `None`; otherwise it is the reshaping itself, which the built head's surface applies to any point of
/// the cranium so what sits on the head lands on the mesh.
#[must_use = "the reshaping is what places things on the built head"]
pub fn reshape(points: &mut [DVec3], radius: &dyn Fn(DVec3) -> f64, outline: &HeadOutline) -> Option<LowerFace> {
    let face = LowerFace::new(points, radius, outline)?;
    for p in points.iter_mut() {
        *p = face.apply(*p);
    }
    Some(face)
}

/// The lower face's reshaping of one head, solved once: the chin stretch, the width profile the sideways scale is
/// measured on, the blend band above the outline's top and the cheek puff. [`LowerFace::apply`] takes any head-local
/// point of the cranium to where the built head has it.
#[derive(Debug)]
pub struct LowerFace {
    outline: HeadOutline,
    curve: OutlineCurve,
    min_ratio: f64,
    stretch: Stretch,
    profile: WidthProfile,
    band: f64,
    puff: Puff,
}

impl LowerFace {
    /// The reshaping that takes the cranium `points` (whose lowest lands on the chin) to `outline`, or `None` for an
    /// outline without a chin. See [`reshape`].
    fn new(points: &[DVec3], radius: &dyn Fn(DVec3) -> f64, outline: &HeadOutline) -> Option<Self> {
        let chin = outline.chin?;
        let stretch = Stretch::solve(points, &chin);
        let top = outline.top;
        let profile = WidthProfile::new(radius, &stretch, top + band_reach(top) + SMOOTHING_TAPS[4] + PROFILE_STEP);
        let band = blend_band(&profile, outline);
        Some(Self {
            outline: *outline,
            curve: OutlineCurve::of(outline),
            min_ratio: chin.min_ratio,
            stretch,
            profile,
            band,
            puff: Puff::on(outline, chin.y),
        })
    }

    /// Where the head-local cranium point `p` lands on the built head: stretched, then (up to the top of the blend band)
    /// scaled sideways onto the outline and puffed.
    #[must_use]
    pub fn apply(&self, p: DVec3) -> DVec3 {
        let mut p = self.stretch.apply(p);
        if p.y <= self.outline.top + self.band {
            p.x *= side_scale(&self.profile, &self.curve, self.outline.top, self.min_ratio, self.band, p.y);
            p.z += self.puff.offset(p);
        }
        p
    }
}

/// The sideways scale at height `y` that puts the front silhouette on the outline: the outline's width over the
/// cranium's, held smoothly within `min_ratio..=2`, at full strength from the outline's top down and easing out (C2)
/// over the `band` above it. The chin's very tip, narrower than [`TIP_WIDTH`], is left as it is.
fn side_scale(profile: &WidthProfile, curve: &OutlineCurve, top: f64, min_ratio: f64, band: f64, y: f64) -> f64 {
    let current = profile.smoothed(y);
    if current <= TIP_WIDTH {
        return 1.0;
    }
    let ratio = soft_max(soft_min(curve.at(y) / current, MAX_RATIO), min_ratio);
    1.0 + (ratio - 1.0) * smoother(0.0, band, top + band - y)
}

/// The tallest band the outline's top allows: [`MAX_BAND`], or less where that would reach within [`BAND_CLEARANCE`]
/// of the head centre (a top near the centre on a small cranium).
fn band_reach(top: f64) -> f64 {
    MAX_BAND.min(-top - BAND_CLEARANCE)
}

/// How tall the band above the outline's top is over which the face eases into the cranium, metres.
///
/// The band turns the side of the face from the cranium's slope to the outline's vertical start, against the cranium's
/// own narrowing. Its slope mismatch is how fast the cranium closes in on the outline across the first [`FADE`]: the
/// cranium's width there minus the outline's, per metre of height. A band [`BAND_PER_MISMATCH`] times that tall turns
/// every head equally gently; a cranium that already runs nearly straight into its outline keeps the prototype's narrow
/// fade. Continuous in every parameter, as the width profile is, so a blend between two face shapes does not jump.
fn blend_band(profile: &WidthProfile, outline: &HeadOutline) -> f64 {
    let mismatch = (profile.smoothed(outline.top + FADE) - outline.half_width) / FADE;
    clamp(BAND_PER_MISMATCH * mismatch, FADE, band_reach(outline.top))
}

/// Raises `row[k]` (the widest `x` at height `low + k` profile steps) to where the grid edge from `from` to `to` crosses
/// each height it spans; heights 0 (the lowest point) and beyond the row are left alone.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)] // bounded indices
fn widen_crossings(row: &mut [f64], from: DVec3, to: DVec3, low: f64) {
    let rise = to.y - from.y;
    let first = ((from.y.min(to.y) - low) / PROFILE_STEP).ceil().max(1.0);
    let last = ((from.y.max(to.y) - low) / PROFILE_STEP).floor().min((row.len() - 1) as f64);
    let mut step = first;
    while step <= last {
        let along = if rise.abs() < 1e-15 { 0.0 } else { (low + step * PROFILE_STEP - from.y) / rise };
        let slot = &mut row[step as usize];
        *slot = slot.max(from.x + (to.x - from.x) * along);
        step += 1.0;
    }
}

/// The chubby cheeks: a Gaussian bulge either side of the mouth that pushes the front of the face forward.
#[derive(Debug)]
struct Puff {
    amount: f64,
    /// Height and distance from the midline of the bulge's centre.
    centre: (f64, f64),
    sigma: (f64, f64),
}

impl Puff {
    /// The bulge of `outline`, whose chin is at `chin`: placed and sized relative to the lower face.
    fn on(outline: &HeadOutline, chin: f64) -> Self {
        let length = (outline.top - chin).max(0.0);
        Self {
            amount: outline.cheek_puff,
            centre: (outline.top - PUFF_DOWN * length, PUFF_OUT * outline.half_width),
            sigma: (
                (PUFF_SIGMA.0 * length).max(PUFF_MIN_SIGMA.0),
                (PUFF_SIGMA.1 * outline.half_width).max(PUFF_MIN_SIGMA.1),
            ),
        }
    }

    /// How far forward the bulge pushes `p`: fading in over the front of the face, nothing at the sides or the back.
    fn offset(&self, p: DVec3) -> f64 {
        if self.amount <= 0.0 || self.sigma.0 <= 0.0 || self.sigma.1 <= 0.0 {
            return 0.0;
        }
        let falloff = (-((p.y - self.centre.0) / self.sigma.0).powi(2)).exp()
            * (-((p.x.abs() - self.centre.1) / self.sigma.1).powi(2)).exp();
        self.amount * falloff * sstep(0.02, 0.12, p.z)
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // sample counters
mod tests {
    use omuzaki_core::rng::Rng;

    use super::*;

    fn outline() -> HeadOutline {
        HeadOutline {
            half_width: 0.13,
            top: -0.09,
            height: 0.1,
            exponent: 2.1,
            exponent_lower: None,
            chin: None,
            cheek_puff: 0.0,
        }
    }

    /// An outline inside the plausible space of the default cranium (synthetic numbers).
    fn inside() -> HeadOutline {
        HeadOutline {
            half_width: 0.155,
            top: -0.095,
            height: 0.105,
            exponent: 2.0,
            exponent_lower: None,
            chin: Some(Chin { y: -0.2, stretch_top: -0.085, min_ratio: 0.65 }),
            cheek_puff: 0.005,
        }
    }

    #[test]
    fn the_superellipse_is_full_width_above_its_top_and_closes_at_the_chin() {
        let o = outline();
        assert!((half_width_at(&o, o.top) - o.half_width).abs() < 1e-12);
        assert!((half_width_at(&o, 0.3) - o.half_width).abs() < 1e-12, "above the top it stays full");
        assert!(half_width_at(&o, o.top - o.height).abs() < 1e-12, "without a chin it closes at top - height");
        let chinned = HeadOutline { chin: Some(Chin { y: -0.25, ..Chin::default() }), ..o };
        assert!(half_width_at(&chinned, -0.25).abs() < 1e-12, "with one, at the chin");
        assert!(half_width_at(&chinned, o.top - o.height) > 0.05, "and not before");
        assert!(half_width_at(&o, -0.5).abs() < 1e-12, "and stays closed below");
    }

    #[test]
    fn an_exponent_of_two_is_an_ellipse() {
        let o = HeadOutline { exponent: 2.0, ..outline() };
        let t = 0.6;
        let expected = o.half_width * (1.0_f64 - t * t).sqrt();
        assert!((half_width_at(&o, o.top - t * o.height) - expected).abs() < 1e-12);
    }

    #[test]
    fn an_outline_inside_the_space_comes_back_unchanged_with_its_height_set_by_the_chin() {
        let o = inside();
        let p = plausible(&o, &Cranium::default());
        assert_eq!(p, HeadOutline { height: o.top - o.chin.map_or(0.0, |c| c.y), ..o });
    }

    #[test]
    fn a_chinless_outline_gets_a_chin_where_it_closes() {
        let o = HeadOutline { chin: None, ..inside() };
        let p = plausible(&o, &Cranium::default());
        let chin = p.chin.unwrap_or_default();
        assert!((chin.y - (o.top - o.height)).abs() < 1e-12, "{chin:?}");
        assert!((chin.stretch_top - (p.top + IMPLICIT_STRETCH_LINE)).abs() < 1e-12);
        assert!((chin.min_ratio - IMPLICIT_MIN_RATIO).abs() < 1e-12);
        assert!((p.chin_y() - chin.y).abs() < 1e-12, "chin_y is where the head ends");
    }

    /// A random outline anywhere `HeadOutline::check` accepts.
    fn any(rng: &mut Rng) -> HeadOutline {
        let mut u = |lo: f64, hi: f64| lo + (hi - lo) * f64::from(rng.next_f32());
        let y = u(-0.5, -0.001);
        let chin = Chin { y, stretch_top: u(y + 0.001, 0.2), min_ratio: u(0.2, 1.0) };
        HeadOutline {
            half_width: u(0.05, 0.4),
            top: u(-0.3, 0.2),
            height: u(0.02, 0.4),
            exponent: u(1.0, 4.0),
            exponent_lower: None,
            chin: (u(0.0, 1.0) < 0.5).then_some(chin),
            cheek_puff: u(0.0, 0.05),
        }
    }

    /// The pointiest face the plausible space allows: the lowest exponent, the longest face, the highest eye line.
    #[test]
    fn a_lower_exponent_passes_through_plausible_clamped_to_its_range() {
        let c = Cranium::default();
        let set = |e: Option<f64>| plausible(&HeadOutline { exponent_lower: e, ..inside() }, &c).exponent_lower;
        assert_eq!(set(None), None);
        assert_eq!(set(Some(2.0)), Some(2.0), "an in-range value comes back unchanged");
        assert_eq!(set(Some(1.3)), Some(1.3), "the lower exponent's range reaches below the exponent's");
        assert_eq!(set(Some(9.0)), Some(LOWER_EXPONENT_RANGE.1), "above the range stops at the ceiling");
        // Below the floor the value stops at the floor, or above it where the chin tip limit binds first.
        let floor = set(Some(0.5)).unwrap();
        assert!(floor >= LOWER_EXPONENT_RANGE.0 && floor < EXPONENT_RANGE.0, "{floor}");
        assert!(
            (tip_angle(&plausible(&HeadOutline { exponent_lower: Some(0.5), ..inside() }, &c)) - MIN_TIP_DEG) > -0.01
        );
    }

    #[test]
    fn the_lowest_exponent_is_kept_and_its_chin_is_a_visible_point_but_no_spike() {
        let c = Cranium::default();
        let down = c.half_height_down_front;
        let given = HeadOutline {
            half_width: 0.2,
            top: 0.0,
            height: 0.4,
            exponent: 1.0,
            exponent_lower: None,
            chin: Some(Chin { y: -0.5, stretch_top: 0.0, min_ratio: 0.2 }),
            cheek_puff: 0.0,
        };
        let p = plausible(&given, &c);
        assert!((p.exponent - EXPONENT_RANGE.0).abs() < 1e-9, "the exponent stops at the floor: {}", p.exponent);
        assert!((p.top - TOP_RANGE.1 * down).abs() < 1e-9, "the eye line is at its highest");
        let tip = tip_angle(&p);
        assert!((MIN_TIP_DEG..160.0).contains(&tip), "a visible point that is no spike: {tip} degrees");
        // Just below the floor is the floor, not a jump: the clamp is continuous.
        let below = plausible(&HeadOutline { exponent: 1.35, ..given }, &c);
        assert!((below.exponent - p.exponent).abs() < 1e-9 && (below.half_width - p.half_width).abs() < 1e-9);
    }

    #[test]
    fn any_valid_outline_lands_inside_every_limit() {
        let c = Cranium::default();
        let down = c.half_height_down_front;
        let own = WidthProfile::new(&|dir| cranium::radius(&c, dir), &Stretch::NONE, 0.0);
        let mut rng = Rng::new(703);
        for _ in 0..200 {
            let p = plausible(&any(&mut rng), &c);
            let chin = p.chin.unwrap_or_default();
            let within = |v: f64, (lo, hi): (f64, f64)| v >= lo - 1e-9 && v <= hi + 1e-9;
            assert!(within(p.top / down, TOP_RANGE), "{p:?}");
            assert!(within(p.half_width / own.at(p.top), WIDTH_RANGE), "{p:?}");
            assert!(own.at(p.top + FADE) - p.half_width <= MAX_PULL_IN * c.half_width + 1e-9, "{p:?}");
            assert!(within((p.top - chin.y) / down, LENGTH_RANGE), "{p:?}");
            assert!(within((chin.y - own.lowest) / down, CHIN_RANGE), "{p:?}");
            assert!(within(chin.stretch_top - p.top, STRETCH_LINE_RANGE), "{p:?}");
            assert!(chin.stretch_top - own.lowest >= MIN_STRETCH_DEPTH * down - 1e-9, "{p:?}");
            assert!(within(p.exponent, EXPONENT_RANGE), "{p:?}");
            assert!(tip_angle(&p) >= MIN_TIP_DEG - 1e-6, "{p:?}");
            assert!(within(chin.min_ratio, MIN_RATIO_RANGE), "{p:?}");
            assert!(within(p.cheek_puff, (0.0, MAX_CHEEK_PUFF)), "{p:?}");
        }
    }

    #[test]
    fn the_clamps_are_continuous_a_slider_stops_at_a_limit_rather_than_jumping() {
        let c = Cranium::default();
        let mut rng = Rng::new(704);
        for _ in 0..40 {
            let a = any(&mut rng);
            let b = HeadOutline { chin: a.chin, ..any(&mut rng) };
            let mut previous = plausible(&a, &c);
            for k in 1..=32 {
                let next = plausible(&a.lerp(&b, f64::from(k) / 32.0), &c);
                let step = (next.half_width - previous.half_width)
                    .abs()
                    .max((next.top - previous.top).abs())
                    .max((next.chin_y() - previous.chin_y()).abs())
                    .max((next.exponent - previous.exponent).abs() / 10.0);
                assert!(step < 0.02, "a 1/32 step of the slider moved the outline by {step}: {previous:?} -> {next:?}");
                previous = next;
            }
        }
    }

    /// The default cranium's points on a sphere grid of `rings` x `segments`, head-local.
    fn cranium_points(rings: usize, segments: usize) -> Vec<DVec3> {
        let c = Cranium::default();
        let mut out = Vec::new();
        for i in 0..=rings {
            for j in 0..=segments {
                let d =
                    direction(std::f64::consts::PI * fraction(i, rings), std::f64::consts::TAU * fraction(j, segments));
                out.push(d * cranium::radius(&c, d));
            }
        }
        out
    }

    #[test]
    fn the_stretch_lands_the_lowest_point_on_the_chin_and_leaves_the_top_alone() {
        let points = cranium_points(48, 64);
        let natural = points.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
        for chin_y in [natural - 0.03, natural + 0.003] {
            let chin = Chin { y: chin_y, stretch_top: -0.09, min_ratio: 0.6 };
            let stretch = Stretch::solve(&points, &chin);
            let moved: Vec<DVec3> = points.iter().map(|p| stretch.apply(*p)).collect();
            let lowest = moved.iter().map(|p| p.y).fold(f64::INFINITY, f64::min);
            assert!((lowest - chin_y).abs() < 1e-9, "lowest {lowest}, chin {chin_y}");
            for (p, q) in moved.iter().zip(&points) {
                assert_eq!((p.x, p.z), (q.x, q.z), "only heights move");
                if q.y >= chin.stretch_top {
                    assert_eq!(p.y, q.y, "above the stretch line nothing moves");
                }
            }
        }
    }

    #[test]
    fn the_front_silhouette_lands_on_the_outline() {
        let c = Cranium::default();
        let o = plausible(&inside(), &c);
        let mut points = cranium_points(192, 256);
        assert!(reshape(&mut points, &|dir| cranium::radius(&c, dir), &o).is_some(), "a plausible outline has a chin");
        // The widest point in thin height bands below the top, against the outline there.
        for k in 0..18 {
            let y = o.top - 0.005 - f64::from(k) * 0.005;
            let widest = points.iter().filter(|p| (p.y - y).abs() < 0.0015).map(|p| p.x).fold(0.0, f64::max);
            let wanted = half_width_at(&o, y);
            assert!((widest - wanted).abs() < 0.0035, "y {y:.3}: {widest:.4} against {wanted:.4}");
        }
    }

    #[test]
    fn the_blend_band_grows_with_the_slope_mismatch_between_the_fade_and_its_limit() {
        let c = Cranium::default();
        let o = plausible(&inside(), &c);
        let profile = WidthProfile::new(&|dir| cranium::radius(&c, dir), &Stretch::NONE, o.top + MAX_BAND + 2.0 * FADE);
        // From an outline as wide as the cranium a fade above its top (no mismatch) to one far narrower.
        let wide = profile.smoothed(o.top + FADE);
        let bands: Vec<f64> = (0..=40)
            .map(|k| blend_band(&profile, &HeadOutline { half_width: wide * (1.0 - 0.01 * f64::from(k)), ..o }))
            .collect();
        assert!(bands.windows(2).all(|b| b[1] >= b[0]), "a narrower outline never gets a shorter band: {bands:?}");
        assert!((bands[0] - FADE).abs() < 1e-12, "no mismatch keeps the prototype's fade");
        assert!((bands[40] - MAX_BAND).abs() < 1e-12, "a large one stops at the limit");
        // In between, the band is proportional to the mismatch.
        let wanted = 0.045;
        let band = blend_band(&profile, &HeadOutline { half_width: wide - wanted / BAND_PER_MISMATCH * FADE, ..o });
        assert!((band - wanted).abs() < 1e-9, "{band}");
    }

    #[test]
    fn the_band_stays_below_the_head_centre() {
        assert!((band_reach(-0.2) - MAX_BAND).abs() < 1e-12);
        assert!((band_reach(-0.04) - (0.04 - BAND_CLEARANCE)).abs() < 1e-12);
    }

    #[test]
    fn the_reshape_leaves_the_cranium_above_the_band_alone_and_reaches_past_the_old_fade() {
        let c = Cranium::default();
        let o = plausible(&inside(), &c);
        let before = cranium_points(96, 128);
        let mut after = before.clone();
        assert!(reshape(&mut after, &|dir| cranium::radius(&c, dir), &o).is_some(), "a plausible outline has a chin");
        for (p, q) in after.iter().zip(&before) {
            if q.y > o.top + MAX_BAND {
                assert_eq!(p, q, "above the tallest band the cranium is untouched");
            }
        }
        let eased = after.iter().zip(&before).any(|(p, q)| q.y > o.top + FADE + 0.01 && (p.x - q.x).abs() > 1e-3);
        assert!(eased, "the band reaches above the prototype's 3 cm fade on the default cranium");
    }

    /// The largest change in the front silhouette's turn between neighbouring 5 mm steps, degrees, from 2 cm below the
    /// outline's top to 1 cm above its blend band: the join's crease, in one dimension.
    fn eye_line_crease(cranium: &Cranium, outline: &HeadOutline) -> f64 {
        let o = plausible(outline, cranium);
        let chin = o.chin.unwrap_or_default();
        let radius = |dir: DVec3| cranium::radius(cranium, dir);
        let stretch = Stretch::solve(&cranium_points(96, 128), &chin);
        let profile =
            WidthProfile::new(&radius, &stretch, o.top + band_reach(o.top) + SMOOTHING_TAPS[4] + PROFILE_STEP);
        let band = blend_band(&profile, &o);
        // At most 2 cm below the top, the tallest band and 1 cm above it: 18 steps.
        let points: Vec<(f64, f64)> = (0..40)
            .map(|k| o.top - 0.02 + 0.005 * f64::from(k))
            .take_while(|y| *y <= o.top + band + 0.01)
            .map(|y| {
                (profile.smoothed(y) * side_scale(&profile, &OutlineCurve::of(&o), o.top, chin.min_ratio, band, y), y)
            })
            .collect();
        let headings: Vec<f64> = points.windows(2).map(|w| (w[1].0 - w[0].0).atan2(w[1].1 - w[0].1)).collect();
        let turns: Vec<f64> = headings.windows(2).map(|h| h[1] - h[0]).collect();
        turns.windows(2).map(|t| (t[1] - t[0]).abs().to_degrees()).fold(0.0, f64::max)
    }

    /// The join measures 4.3 to 6.4 degrees on these heads; the prototype's 3 cm smoothstep fade measured 7.4 to 13.0
    /// on the same.
    #[test]
    fn the_side_of_the_face_turns_gently_through_the_eye_line() {
        let c = Cranium::default();
        let narrow = Cranium {
            half_width: 0.9 * c.half_width,
            half_height_down_front: 1.1 * c.half_height_down_front,
            ..c.clone()
        };
        let outlines = [
            inside(),
            // The lowest, narrowest top the space allows (the largest mismatch) and the highest, widest.
            HeadOutline { half_width: 0.05, top: -0.3, ..inside() },
            HeadOutline { half_width: 0.4, top: 0.2, ..inside() },
        ];
        for cranium in [&c, &narrow] {
            for o in &outlines {
                let crease = eye_line_crease(cranium, o);
                assert!(crease < 7.0, "{crease} deg for {o:?} on {cranium:?}");
            }
        }
    }

    #[test]
    fn the_width_profile_is_smooth_into_the_chin() {
        let c = Cranium::default();
        let profile = WidthProfile::new(&|dir| cranium::radius(&c, dir), &Stretch { line: -0.08, k: 1.4 }, 0.0);
        // The width grows steadily from the chin up. Above the tip, where the 1 cm window no longer reaches below the
        // lowest point (and the ratio floor rules anyway), its second differences every 0.5 mm stay small: no bumps
        // where meridians come and go.
        let w: Vec<f64> = (0..160).map(|k| profile.smoothed(profile.lowest + 0.0005 * f64::from(k))).collect();
        assert!(w.windows(2).all(|t| t[1] >= t[0] - 1e-9), "the width grows from the chin up");
        for (k, t) in w.windows(3).enumerate().skip(10) {
            let bend = (t[0] - 2.0 * t[1] + t[2]).abs();
            assert!(bend < 2e-4, "bump of {bend:e} m at {:.1} mm", 0.5 + 0.5 * k as f64);
        }
    }

    #[test]
    fn the_width_profile_moves_smoothly_with_the_stretch() {
        // As the chin is pulled down, the width a few millimetres above the lowest point changes steadily: no step
        // much larger than its neighbours, as there was when meridians alone were cut and a dip's floor passed a height.
        let c = Cranium::default();
        let radius = |dir: DVec3| cranium::radius(&c, dir);
        for above in [0.001, 0.002, 0.003, 0.005] {
            let widths: Vec<f64> = (0..=200)
                .map(|k| {
                    let stretch = Stretch { line: -0.09, k: 1.3 + 0.001 * f64::from(k) };
                    let profile = WidthProfile::new(&radius, &stretch, -0.1);
                    profile.smoothed(profile.lowest + above)
                })
                .collect();
            let mut steps: Vec<f64> = widths.windows(2).map(|w| (w[1] - w[0]).abs()).collect();
            let largest = steps.iter().copied().fold(0.0, f64::max);
            steps.sort_by(f64::total_cmp);
            let median = steps[steps.len() / 2];
            assert!(largest <= 3.0 * median + 1e-7, "{above} m up: a step of {largest:e} against a median {median:e}");
        }
    }

    #[test]
    fn the_soft_limits_meet_the_hard_ones_away_from_the_corner_and_are_smooth_through_it() {
        assert!((soft_max(1.5, 0.6) - 1.5).abs() < 1e-3 && (soft_max(0.1, 0.6) - 0.6).abs() < 1e-3);
        assert!((soft_min(3.0, 2.0) - 2.0).abs() < 1e-3 && (soft_min(1.0, 2.0) - 1.0).abs() < 1e-3);
        let slope = |x: f64| (soft_max(x + 1e-6, 0.6) - soft_max(x - 1e-6, 0.6)) / 2e-6;
        assert!((slope(0.6 - 1e-4) - slope(0.6 + 1e-4)).abs() < 0.01, "no kink at the limit");
        // The stretch's easing is C2: its slope and curvature are 0 at both ends.
        for x in [0.0, STRETCH_EASE] {
            let h = 1e-5;
            let d1 = (ease(x + h) - ease(x - h)) / (2.0 * h);
            let d2 = (ease(x + h) - 2.0 * ease(x) + ease(x - h)) / (h * h);
            // Against the step's own curvature scale, 1 / STRETCH_EASE^2 (about 1 100 per metre squared).
            assert!(d1.abs() < 1e-3 && d2.abs() < 10.0, "at {x}: {d1} {d2}");
        }
    }

    #[test]
    fn a_puff_moves_only_the_front_of_the_cheeks_forward() {
        let o = HeadOutline { cheek_puff: 0.01, ..plausible(&inside(), &Cranium::default()) };
        let puff = Puff::on(&o, o.chin_y());
        let (y, x) = puff.centre;
        assert!(puff.offset(DVec3::new(x, y, 0.15)) > 0.009, "the cheek bulges");
        assert_eq!(puff.offset(DVec3::new(x, y, -0.1)), 0.0, "the back does not");
        assert!(puff.offset(DVec3::new(x, y + 0.2, 0.15)) < 1e-9, "nor the forehead");
    }
}
