//! The body-shape space (CB-04): [`BodyShape`]'s five axes as smooth deformations of any template, and the presets.
//!
//! A shape never touches a template's tables. It answers two questions for [`layout::build`](super::layout::build):
//! *where do the joints go* ([`Shape::length`], [`Shape::arm_spacing`], [`Shape::leg_spacing`]) and *how does each
//! section grow or shrink* ([`Shape::torso`], [`Shape::neck`], [`Shape::arm`], [`Shape::leg`], each a [`Gain`] at a
//! point along the region). Both are written in **template-normalised coordinates**: a point on a region is a fraction
//! `0..=1` of the region's own length, and the landmarks a response hangs on (waist, bust, shoulder line, elbow, knee,
//! ankle) are fractions too, read from the template by [`Marks`]. So one definition serves a 2.5-head chibi and an
//! 8-head figure, and the features slide with the joints when proportions change.
//!
//! # The axes
//!
//! Every response is a smooth bump on a landmark, scaled by `swell` of the axis value, so `0` is exactly the template
//! and `-1` and `+1` need not be mirror images (a body has more room to get fuller than to get thinner). Gains are
//! factors, and the axes **multiply**: the order they are applied in does not matter, and a corner of the cube is the
//! product of its single-axis bodies. Only where several pile onto the same place does `limit` bend the product over,
//! so a corner stays a body, not a balloon.
//!
//! | Axis | What it does at `+1` (the figures are the soft-sd template's) |
//! |---|---|
//! | `height` | the whole body from the sole to the head base is 12% longer (legs, torso, neck, arms); the head keeps its size, so the crown rises by about 7% |
//! | `build` | girth: belly and hips +26%, shoulders +10%, neck +8%, thigh +26%, calf +15%, ankle +5%, upper arm +26%, forearm +16%, wrist +5%; the belly leans forward |
//! | `curves` | front: waist -30%, hips +22% (flaring down the upper thigh); side: bust +24% forward, buttocks +26% back, waist -20% front and -12% back. At `-1` the waist fills (+20%) and the hips (-14%), bust (-24%) and buttocks (-22%) flatten |
//! | `musculature` | shoulder line +14%, pectorals +18% forward, back +12%, neck +24%, waist -6%; arm: deltoid +35%, biceps +19%, elbow as it was, forearm +26%, wrist as it was; leg: thigh +26% and calf +24% (outward only), calf +42% to the back, knee as it was. At `-1` the limbs slim by up to a fifth and the belly fills |
//! | `shoulder_hip` | shoulder-line width +18% and hip width -18% (the mean stays); the arms ride out with the shoulders and the legs close in with the hips |
//!
//! # Limbs follow the trunk
//!
//! A shape moves the roots of the limbs with the trunk, so no combination puts a limb inside the body or another limb:
//! the arms' roots ride out with the shoulder line ([`Shape::arm_spacing`]), the legs' offsets with their own girth
//! ([`Shape::leg_spacing`]; swellings that only reach outward leave the gap between the thighs as it was), and
//! [`layout::build`](super::layout::build) swings the arms out when the trunk below the shoulders (a broad or hip-heavy
//! body) or their own girth would put them inside it, and tucks the legs in where they run up inside a narrowing
//! torso (a waist). The clearance tests check the whole cube.
//!
//! # Presets
//!
//! [`presets`] reads the library data in `library/body-shapes/*.toml`: five numbers each, so a preset is only ever a
//! point in the same space ([`BodyShape::lerp`] blends two).

use std::ops::Mul;
use std::sync::OnceLock;

use omuzaki_char_api::{BodyShape, BodyShapeId};
use omuzaki_face_api::Validate;
use serde::Deserialize;

use crate::kit::curve::{clamp, fraction, lerp, sstep};

// ------------------------------------------------------------------------------------------------ the numbers

/// `height = 1` makes the body this much longer (`-1`: shorter): the documented 12% of `BodyShape::height`.
const HEIGHT_RANGE: f64 = 0.12;
/// `shoulder_hip = 1`: the shoulder line widens and the hips narrow by this fraction (and the reverse at `-1`).
const BALANCE_RANGE: f64 = 0.18;

/// Girth at `build = +1` and `-1`: the fraction a full-weight region (belly, hips, thigh, upper arm) changes by.
const BUILD: (f64, f64) = (0.26, 0.20);
/// How much of the girth change each part takes: shoulders are bone, the neck and the ends of limbs likewise.
const BUILD_SHOULDER_SHARE: f64 = 0.4;
const BUILD_NECK_SHARE: f64 = 0.3;

/// `curves`: waist narrowing (width, front, back), hip fullness (width), bust projection (front), buttocks projection
/// (back). Each is the fraction a feature changes by at `+1` and at `-1` (a negative `down` is a fuller waist). At `+1`
/// they give a waist about 0.62 of the hips' width in front (a stylised hourglass: drawing guides put the idealised
/// waist-to-hip ratio between 0.5 and 0.75) and hips about 1.1 times the chest.
const WAIST_WIDTH: (f64, f64) = (0.18, 0.14);
const WAIST_FRONT: (f64, f64) = (0.12, 0.10);
const WAIST_BACK: (f64, f64) = (0.08, 0.08);
const HIP_WIDTH: (f64, f64) = (0.10, 0.08);
const BUST_FRONT: (f64, f64) = (0.24, 0.20);
const SEAT_BACK: (f64, f64) = (0.20, 0.16);

/// `musculature` on the trunk: shoulder-line width, pectorals (front), back, waist tightening (a soft body's belly
/// fills), buttocks, neck.
const SHOULDER_LINE: (f64, f64) = (0.14, 0.12);
const PECS_FRONT: (f64, f64) = (0.18, 0.10);
const BACK_MASS: (f64, f64) = (0.12, 0.08);
const TIGHT_WAIST: (f64, f64) = (0.04, 0.10);
const GLUTES_BACK: (f64, f64) = (0.12, 0.10);
const NECK: (f64, f64) = (0.24, 0.14);

/// The reach of a muscle beyond the template at `musculature = +1`, as fractions of a section's half-width on each side:
/// (outer, inner, front, back). At `-1` every muscle gives back `SOFT_MUSCLE` of that, so the limbs slim and the
/// template's own shape shows through.
type Reach = (f64, f64, f64, f64);
/// Deltoid, a cap over the shoulder, outward and a little forward and back.
const DELTOID: Reach = (0.42, 0.04, 0.20, 0.16);
/// Biceps in front and a little inward, triceps behind.
const UPPER_ARM: Reach = (0.05, 0.10, 0.28, 0.22);
/// The forearm's extensors and brachioradialis, outward and forward.
const FOREARM: Reach = (0.24, 0.05, 0.14, 0.08);
/// Quadriceps in front and outward, hamstrings behind; the inside of the thigh stays (the gap between the thighs).
const THIGH: Reach = (0.26, 0.0, 0.20, 0.12);
/// Calf behind, with some to the inside and the outside; the front stays near the shin.
const CALF: Reach = (0.10, 0.08, 0.05, 0.32);
const SOFT_MUSCLE: f64 = 0.5;
/// How far down the thigh, from the crotch to the knee, the pelvis's flare (`curves`) is gone: a quick end keeps the
/// thighs their own, a slow one runs the hips into them.
const THIGH_FLARE_FADE: f64 = 0.5;
/// How much of the thighs' outward swell the pelvis takes, so a muscular thigh does not stand out of it.
const PELVIS_FOLLOWS_THIGH: f64 = 0.3;

/// Where along the torso the neck's gain takes over from the body's: the shoulders slope down into the neck.
const TORSO_NECK_FADE: f64 = 0.80;
/// The share of the neck's own gain the torso's top (the neck's base) already has: the trapezius.
const NECK_BASE_SHARE: f64 = 0.6;
/// How far down the neck its own gain is fully in force, as a fraction of the neck's length.
const NECK_RAMP: f64 = 0.35;
/// The range of the factors on where the limbs' roots sit across the body ([`Shape::arm_spacing`],
/// [`Shape::leg_spacing`]). The proportion sliders scale the same offsets by `0.5..=2`, and a lateral root bone must stay
/// within `0.4..=2.8` times its default length at every setting of both (`conformance::LATERAL_ROOT_MAX_RATIO`); the
/// factors here keep to `0.85..=1.35` of that.
const SPACING: (f64, f64) = (0.85, 1.35);
/// The most the legs' girth grows across the body: what `build = 1` alone gives.
const LEG_GIRTH_MAX: f64 = 1.26;
/// How much further apart than their girth the legs stand at the ends of `build`, so thighs read as two (a gap of at
/// least 8% of a thigh's width at every corner of the cube), a heavy body's and a slim one's alike.
const LEG_GAP: f64 = 0.09;
/// The samples along a region that the `Shape` queries for the widest part use.
const SAMPLES: usize = 48;

// ------------------------------------------------------------------------------------------------ gain and landmarks

/// What a shape does to one cross-section: factors on the distances from its centre to its four sides. `1` everywhere
/// is no change.
///
/// The sides are separate so a feature can bulge one way only: a bust forward, a calf backward, a quadriceps outward
/// (which leaves the gap between the thighs as it was). The section's radius grows by the mean of the two factors on an
/// axis and its centre moves by half their difference (see `Region::section`). For a region that is its own mirror
/// image (the torso, the neck) the two sides across are equal.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Gain {
    /// Factor on the distance from the centre to the outer edge (away from the body's axis).
    pub outer: f64,
    /// Factor on the distance from the centre to the inner edge (towards the body's axis).
    pub inner: f64,
    /// Factor on the distance from the centre to the front edge.
    pub front: f64,
    /// Factor on the distance from the centre to the back edge.
    pub back: f64,
}

impl Gain {
    /// No change.
    pub const ONE: Self = Self { outer: 1.0, inner: 1.0, front: 1.0, back: 1.0 };

    /// A change of `outer`, `inner`, `front` and `back`, each a fraction (`0.1` is 10% more).
    fn by(outer: f64, inner: f64, front: f64, back: f64) -> Self {
        Self { outer: 1.0 + outer, inner: 1.0 + inner, front: 1.0 + front, back: 1.0 + back }
    }

    /// Factors `x` on both sides across, `front` forward and `back` backward.
    fn sym(x: f64, front: f64, back: f64) -> Self {
        Self { outer: x, inner: x, front, back }
    }

    /// The same change `r` in every direction.
    fn round(r: f64) -> Self {
        Self::by(r, r, r, r)
    }

    /// Factors `lerp`ed from `self` at `t = 0` to `other` at `t = 1`.
    #[must_use]
    fn mix(self, other: Self, t: f64) -> Self {
        Self {
            outer: lerp(self.outer, other.outer, t),
            inner: lerp(self.inner, other.inner, t),
            front: lerp(self.front, other.front, t),
            back: lerp(self.back, other.back, t),
        }
    }

    /// The same gain with every factor kept within [`limit`]'s range, however many axes pile up.
    #[must_use]
    fn limited(self) -> Self {
        Self { outer: limit(self.outer), inner: limit(self.inner), front: limit(self.front), back: limit(self.back) }
    }
}

impl Mul for Gain {
    type Output = Self;

    fn mul(self, o: Self) -> Self {
        Self {
            outer: self.outer * o.outer,
            inner: self.inner * o.inner,
            front: self.front * o.front,
            back: self.back * o.back,
        }
    }
}

/// Where a template's landmarks sit, as fractions of each region's length. [`layout::build`](super::layout::build)
/// reads them from the template's joints and zones, so a shape needs no template-specific numbers.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Marks {
    /// The torso's waist (its narrowest point).
    pub waist: f64,
    /// The torso at the bust.
    pub bust: f64,
    /// The middle of the shoulder zone.
    pub shoulder: f64,
    /// The torso at the height of the arm's root joint.
    pub arm_root: f64,
    /// The arm's elbow.
    pub elbow: f64,
    /// The leg's hip joint.
    pub hip_joint: f64,
    /// The leg's section level with the torso's bottom: where the legs leave the pelvis.
    pub crotch: f64,
    /// The torso's half-width at its bottom over a leg's half-width there: how much more a given change of the pelvis's
    /// width is than the same change of a thigh's, so a hip flare carries on down the leg.
    pub pelvis_ratio: f64,
    /// The leg's knee.
    pub knee: f64,
    /// The leg's ankle.
    pub ankle: f64,
}

// ------------------------------------------------------------------------------------------------ smooth pieces

/// `1` at `centre`, falling smoothly to `0` at `half` either side.
fn bump(v: f64, centre: f64, half: f64) -> f64 {
    1.0 - sstep(0.0, half, (v - centre).abs())
}

/// How much of a feature an axis at `a` (`-1..=1`) switches on: `up` at `+1`, `-down` at `-1`, `0` at `0`.
///
/// A quadratic through those three points, so it is smooth through `0` and unequal ends are fine; it is monotone while
/// `up` and `down` are within a factor of three of each other, which every table above keeps to.
fn swell(a: f64, (up, down): (f64, f64)) -> f64 {
    let (linear, curved) = (f64::midpoint(up, down), (up - down) / 2.0);
    a * (linear + curved * a)
}

/// A factor within `0.65..=1.35` is left exactly as it is; beyond that it bends over smoothly (the slope is `1` where it
/// starts) towards `0.45` and `1.5`. A single axis never leaves the exact range, so one axis is always itself; where
/// several pile onto the same place (every axis fattening the hips) the body stays a body, and no section can shrink to
/// nothing.
fn limit(g: f64) -> f64 {
    const LOW: (f64, f64) = (0.65, 0.45);
    const HIGH: (f64, f64) = (1.35, 1.5);
    if g > HIGH.0 {
        let room = HIGH.1 - HIGH.0;
        HIGH.0 + room * ((g - HIGH.0) / room).tanh()
    } else if g < LOW.0 {
        let room = LOW.0 - LOW.1;
        LOW.0 - room * ((LOW.0 - g) / room).tanh()
    } else {
        g
    }
}

/// The largest value of `f` over `0..=1`, sampled.
fn sampled_max(f: impl Fn(f64) -> f64) -> f64 {
    (0..=SAMPLES).map(|k| f(fraction(k, SAMPLES))).fold(f64::MIN, f64::max)
}

// ------------------------------------------------------------------------------------------------ the shape

/// A [`BodyShape`] with its axes made safe (clamped to `-1..=1`, NaN read as `0`) and ready to answer the builder.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Shape {
    height: f64,
    build: f64,
    curves: f64,
    muscle: f64,
    balance: f64,
}

impl Shape {
    /// The shape's five axes, sanitised.
    pub fn new(shape: &BodyShape) -> Self {
        let axis = |v: f64| if v.is_finite() { clamp(v, -1.0, 1.0) } else { 0.0 };
        Self {
            height: axis(shape.height),
            build: axis(shape.build),
            curves: axis(shape.curves),
            muscle: axis(shape.musculature),
            balance: axis(shape.shoulder_hip),
        }
    }

    /// The factor on every length of the body from the sole to the head base.
    pub fn length(&self) -> f64 {
        1.0 + HEIGHT_RANGE * self.height
    }

    /// Hip-zone width factor from `shoulder_hip`.
    fn hip_balance(&self) -> f64 {
        1.0 - BALANCE_RANGE * self.balance
    }

    /// Shoulder-zone width factor from `shoulder_hip`.
    fn shoulder_balance(&self) -> f64 {
        1.0 + BALANCE_RANGE * self.balance
    }

    /// The factor on the arms' x offset from the body's axis: the torso's width at the shoulder line, so the arms stay
    /// where the shoulders put them, within `SPACING` (an arm a little off a very narrow or very wide shoulder is
    /// covered by its own girth).
    pub fn arm_spacing(&self, marks: &Marks) -> f64 {
        self.torso(marks.arm_root, marks).outer.clamp(SPACING.0, SPACING.1)
    }

    /// How much further out than the shoulder line the arms' roots sit, as a fraction of their offset: where
    /// [`Shape::arm_spacing`] holds them out from a shoulder narrower than it allows (`0` otherwise).
    pub fn arm_slack(&self, marks: &Marks) -> f64 {
        self.arm_spacing(marks) - self.torso(marks.arm_root, marks).outer
    }

    /// The factor on the trunk's width at the widest below the arm roots (the pelvis and belly an arm hangs beside).
    pub fn hang_width(&self, marks: &Marks) -> f64 {
        sampled_max(|k| self.torso(k * marks.arm_root, marks).outer)
    }

    /// The factor on the arm's half-width at its thickest.
    pub fn arm_girth(&self, marks: &Marks) -> f64 {
        sampled_max(|k| self.arm(k, marks).outer)
    }

    /// The factor on the legs' x offset from the body's axis: the widest the legs' own girth gets (at most
    /// `LEG_GIRTH_MAX`), and a little more where they grow. A leg's gap to the midline scales with it (and was open in the template), so legs never
    /// overlap; swellings that only reach outward (muscle, hip flare) leave it alone.
    pub fn leg_spacing(&self, marks: &Marks) -> f64 {
        let girth = sampled_max(|v| self.leg_girth(v, marks));
        (girth * (1.0 + LEG_GAP * self.build.abs())).max(SPACING.0)
    }

    /// The torso's gain a fraction `v` of the way from the pelvis to the neck.
    ///
    /// Each axis is blended into its own share of the neck's gain towards the top, so the torso ends exactly where the
    /// neck begins ([`Shape::neck`]) and the axes still multiply.
    pub fn torso(&self, v: f64, marks: &Marks) -> Gain {
        let to_neck = sstep(TORSO_NECK_FADE, 1.0, v);
        let (build_base, muscle_base) = self.neck_bases();
        (self.trunk_build(v, marks).mix(build_base, to_neck)
            * self.trunk_muscle(v, marks).mix(muscle_base, to_neck)
            * self.trunk_curves(v, marks).mix(Gain::ONE, to_neck)
            * self.trunk_balance(v, marks).mix(Gain::ONE, to_neck))
        .limited()
    }

    /// The neck's gain a fraction `v` of the way from its base to the head. At `v = 0` it equals the torso's at `v = 1`,
    /// so the two regions always meet.
    pub fn neck(&self, v: f64) -> Gain {
        let along = sstep(0.0, NECK_RAMP, v);
        let (build_base, muscle_base) = self.neck_bases();
        (build_base.mix(self.neck_build(), along) * muscle_base.mix(self.neck_muscle(), along)).limited()
    }

    /// The arm's gain a fraction `v` of the way from the shoulder to the wrist.
    pub fn arm(&self, v: f64, marks: &Marks) -> Gain {
        let e = marks.elbow;
        // Girth is flesh around the upper arm and bone at the wrist.
        let flesh = 1.0 - 0.8 * sstep(0.7 * e, 1.0, v);
        let build = Gain::round(swell(self.build, BUILD) * flesh);
        // A deltoid cap, the biceps and triceps, then the forearm. The lobes overlap, so the arm tapers through the
        // elbow instead of pinching there, and each reaches the sides it does on a body, not all round.
        let forearm = 1.0 - e;
        let muscle = self.muscle_gain(&[
            (DELTOID, bump(v, 0.16 * e, 0.45 * e)),
            (UPPER_ARM, bump(v, 0.55 * e, 0.60 * e)),
            (FOREARM, bump(v, e + 0.28 * forearm, 0.55 * forearm)),
        ]);
        (build * muscle).limited()
    }

    /// The gain of the muscles `lobes` (each a [`Reach`] and its weight at this point) at the current `musculature`.
    fn muscle_gain(&self, lobes: &[(Reach, f64)]) -> Gain {
        let s = swell(self.muscle, (1.0, SOFT_MUSCLE));
        let sum = |side: fn(&Reach) -> f64| s * lobes.iter().map(|(reach, weight)| side(reach) * weight).sum::<f64>();
        Gain::by(sum(|r| r.0), sum(|r| r.1), sum(|r| r.2), sum(|r| r.3))
    }

    /// The leg's gain a fraction `v` of the way from the hip to the ankle.
    ///
    /// Girth and the hip balance act on both sides, and the legs' spacing grows with them ([`Shape::leg_spacing`]);
    /// muscle and the hips' flare swell outward and front and back only, so they never push into the other leg.
    pub fn leg(&self, v: f64, marks: &Marks) -> Gain {
        let (hip, knee, ankle) = (marks.hip_joint, marks.knee, marks.ankle);
        let girth = self.leg_girth(v, marks);
        let flesh = 1.0 + swell(self.build, BUILD) * leg_flesh(v, marks);
        // Nothing swells above the crotch: up there the leg is inside the pelvis and must stay inside it.
        let below = sstep(0.5 * marks.crotch, marks.crotch, v);
        // A thigh that runs on into the calf through a knee that tapers gently, not pinches. Everything reaches outward,
        // forward and back (the thigh's inside stays as it was), so the legs never push into each other.
        let (thigh, shin) = (knee - hip, ankle - knee);
        let muscle = self.muscle_gain(&[
            (THIGH, below * bump(v, hip + 0.40 * thigh, 0.62 * thigh)),
            (CALF, bump(v, knee + 0.34 * shin, 0.80 * shin)),
        ]);
        // The pelvis's flare (`curves`) carries on down the thigh, fading out at the knee.
        let flare = swell(self.curves, HIP_WIDTH)
            * hip_line(0.0, marks)
            * marks.pelvis_ratio
            * (1.0 - sstep(marks.crotch, marks.crotch + THIGH_FLARE_FADE * (knee - marks.crotch), v));
        let curves = Gain::by(below * flare, 0.0, 0.0, 0.0);
        let total = (Gain::sym(girth, flesh, flesh) * muscle * curves).limited();
        // The inside is the girth (what `leg_spacing` makes room for) and the calf's small swell inward.
        Gain { inner: girth * muscle.inner, ..total }
    }

    /// The legs' girth across the body at `v`: `build` (full at the thigh, less at the knee and calf, little at the
    /// ankle) and the hips' balance (the ankles take half of it), so legs are as wide as the pelvis above them.
    fn leg_girth(&self, v: f64, marks: &Marks) -> f64 {
        let hip_ratio = 1.0 + (self.hip_balance() - 1.0) * (1.0 - 0.5 * sstep(marks.hip_joint, marks.ankle, v));
        limit((1.0 + swell(self.build, BUILD) * leg_flesh(v, marks)) * hip_ratio).min(LEG_GIRTH_MAX)
    }

    /// Belly, hips and chest fill out or slim down with `build`; shoulders change least.
    fn trunk_build(&self, v: f64, marks: &Marks) -> Gain {
        let k = swell(self.build, BUILD);
        let shoulder_line = sstep(marks.waist, marks.shoulder, v);
        let across = k * (1.0 - (1.0 - BUILD_SHOULDER_SHARE) * shoulder_line);
        let depth = k * (1.0 - 0.3 * shoulder_line);
        // The belly leans forward more than the back backs out.
        Gain::by(across, across, depth * 1.15, depth * 0.85)
    }

    /// Shoulder line, pectorals, back and glutes with `musculature`; a soft body's waist fills out instead.
    fn trunk_muscle(&self, v: f64, marks: &Marks) -> Gain {
        let m = self.muscle;
        let seat = seat(v, marks);
        let tight = 1.0 - swell(m, TIGHT_WAIST) * bump(v, marks.waist, 0.17);
        // The pelvis keeps up with the thighs, so the legs never poke out of it.
        let thighs = PELVIS_FOLLOWS_THIGH * THIGH.0 * swell(m, (1.0, SOFT_MUSCLE)).max(0.0) * pelvis(v, marks);
        Gain::sym(
            (1.0 + swell(m, SHOULDER_LINE) * shoulders(v, marks) + thighs) * tight,
            (1.0 + swell(m, PECS_FRONT) * bump(v, marks.bust, 0.18)) * tight,
            (1.0 + swell(m, BACK_MASS) * bump(v, marks.shoulder, 0.22) + swell(m, GLUTES_BACK) * seat) * tight,
        )
    }

    /// Waist, hips, bust and buttocks with `curves`, each in the plane it is seen in.
    fn trunk_curves(&self, v: f64, marks: &Marks) -> Gain {
        let c = self.curves;
        let waist = bump(v, marks.waist, 0.20);
        let seat = seat(v, marks);
        let bust = bump(v, marks.bust, 0.19);
        let across = swell(c, HIP_WIDTH) * hip_line(v, marks) - swell(c, WAIST_WIDTH) * waist;
        Gain::by(
            across,
            across,
            swell(c, BUST_FRONT) * bust - swell(c, WAIST_FRONT) * waist,
            swell(c, SEAT_BACK) * seat - swell(c, WAIST_BACK) * waist,
        )
    }

    /// `shoulder_hip` trades the widths of the shoulder line and the hips.
    fn trunk_balance(&self, v: f64, marks: &Marks) -> Gain {
        let width = (1.0 + (self.shoulder_balance() - 1.0) * shoulders(v, marks))
            * (1.0 + (self.hip_balance() - 1.0) * pelvis(v, marks));
        Gain::sym(width, 1.0, 1.0)
    }

    /// The neck's own gain from `build`: a little thicker with a heavier build.
    fn neck_build(&self) -> Gain {
        Gain::round(BUILD_NECK_SHARE * swell(self.build, BUILD))
    }

    /// The neck's own gain from `musculature`: a much thicker neck.
    fn neck_muscle(&self) -> Gain {
        Gain::round(swell(self.muscle, NECK))
    }

    /// What the torso's top and the neck's base share, per axis: part of the neck's gain, the trapezius.
    fn neck_bases(&self) -> (Gain, Gain) {
        (Gain::ONE.mix(self.neck_build(), NECK_BASE_SHARE), Gain::ONE.mix(self.neck_muscle(), NECK_BASE_SHARE))
    }
}

/// How much of the girth change the leg takes at `v`: all of it at the thigh, less at the knee, little at the ankle.
fn leg_flesh(v: f64, marks: &Marks) -> f64 {
    1.0 - 0.3 * sstep(0.5 * marks.knee, marks.knee, v) - 0.5 * sstep(marks.knee, marks.ankle, v)
}

/// How far `curves` flares the pelvis at `v` along the torso: strongest at the crotch's level, where the hips run on
/// into the thighs, and gone by the waist.
fn hip_line(v: f64, marks: &Marks) -> f64 {
    bump(v, 0.0, 1.15 * marks.waist)
}

/// How far the buttocks stand out at `v`: strongest just above the crotch's level, gone by the waist.
fn seat(v: f64, marks: &Marks) -> f64 {
    bump(v, 0.2 * marks.waist, 0.9 * marks.waist)
}

/// The pelvis's share of a change at `v`: `1` over the hips and up past the waist, where the legs end inside the
/// torso, then easing off up the ribs (a smoother edge than the template's hip zone, which the width sliders use).
fn pelvis(v: f64, marks: &Marks) -> f64 {
    1.0 - sstep(1.25 * marks.waist, 1.9 * marks.waist, v)
}

/// The shoulder line's share of a change at `v`: building up from the waist to the shoulders, easing off to the neck.
fn shoulders(v: f64, marks: &Marks) -> f64 {
    sstep(marks.waist, marks.shoulder, v) * (1.0 - sstep(marks.shoulder, 1.0, v))
}

// ------------------------------------------------------------------------------------------------ presets

/// Why the preset library could not be read: a file in `library/body-shapes/` is malformed. The library is compiled
/// in, so this is a bug in the data, which the crate's own tests catch.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("body-shape preset `{file}`: {reason}")]
pub struct PresetError {
    /// The library file.
    pub file: &'static str,
    /// What is wrong with it.
    pub reason: String,
}

/// One preset of the library: a named point in the body-shape space.
#[derive(Clone, Debug, PartialEq)]
pub struct Preset {
    /// The preset's id (`curvy`), also the file's name.
    pub id: BodyShapeId,
    /// One line for pickers.
    pub summary: String,
    /// The five numbers, with [`BodyShape::preset`] set to `id`.
    pub shape: BodyShape,
}

/// The file's contents: five numbers and a line of text.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFile {
    id: BodyShapeId,
    summary: String,
    height: f64,
    build: f64,
    curves: f64,
    musculature: f64,
    shoulder_hip: f64,
}

/// Every library file, in the order the presets are listed.
const LIBRARY: [(&str, &str); 10] = [
    ("average", include_str!("../../library/body-shapes/average.toml")),
    ("slim", include_str!("../../library/body-shapes/slim.toml")),
    ("broad", include_str!("../../library/body-shapes/broad.toml")),
    ("curvy", include_str!("../../library/body-shapes/curvy.toml")),
    ("athletic", include_str!("../../library/body-shapes/athletic.toml")),
    ("stocky", include_str!("../../library/body-shapes/stocky.toml")),
    ("pear", include_str!("../../library/body-shapes/pear.toml")),
    ("hourglass", include_str!("../../library/body-shapes/hourglass.toml")),
    ("lean-tall", include_str!("../../library/body-shapes/lean-tall.toml")),
    ("petite", include_str!("../../library/body-shapes/petite.toml")),
];

fn parse(file: &'static str, source: &str) -> Result<Preset, PresetError> {
    let fail = |reason: String| PresetError { file, reason };
    let p: PresetFile = toml::from_str(source).map_err(|e| fail(e.to_string()))?;
    if p.id.as_str() != file {
        return Err(fail(format!("its id is `{}`, not the file's name", p.id.as_str())));
    }
    let shape = BodyShape {
        height: p.height,
        build: p.build,
        curves: p.curves,
        musculature: p.musculature,
        shoulder_hip: p.shoulder_hip,
        preset: Some(p.id.clone()),
    };
    shape.validate().map_err(|e| fail(e.to_string()))?;
    Ok(Preset { id: p.id, summary: p.summary, shape })
}

/// The preset library: `average slim broad curvy athletic stocky pear hourglass lean-tall petite`, parsed once.
///
/// # Errors
/// [`PresetError`] if a library file is malformed (the crate's tests keep that from shipping).
pub fn presets() -> Result<&'static [Preset], PresetError> {
    static PRESETS: OnceLock<Result<Vec<Preset>, PresetError>> = OnceLock::new();
    PRESETS
        .get_or_init(|| LIBRARY.iter().map(|(file, source)| parse(file, source)).collect())
        .as_ref()
        .map(Vec::as_slice)
        .map_err(Clone::clone)
}

/// The preset called `id`, if the library has one.
///
/// # Errors
/// [`PresetError`] as [`presets`].
pub fn preset(id: &str) -> Result<Option<&'static Preset>, PresetError> {
    Ok(presets()?.iter().find(|p| p.id.as_str() == id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn swell_hits_its_three_points_and_is_monotone_for_unequal_ends() {
        for (up, down) in [(0.26, 0.20), (0.30, 0.10), (0.1, 0.3)] {
            let (low, zero, high) = (swell(-1.0, (up, down)), swell(0.0, (up, down)), swell(1.0, (up, down)));
            assert!((low + down).abs() < 1e-12 && zero == 0.0 && (high - up).abs() < 1e-12, "{up} {down}");
            let steps: Vec<f64> = (-20..=20).map(|k| swell(f64::from(k) / 20.0, (up, down))).collect();
            assert!(steps.windows(2).all(|w| w[0] <= w[1] + 1e-15), "{up} {down}");
        }
    }

    #[test]
    fn limit_leaves_the_exact_range_alone_and_bends_over_smoothly_outside_it() {
        for g in [0.65, 0.8, 1.0, 1.2, 1.35] {
            assert_eq!(limit(g), g);
        }
        let steps: Vec<f64> = (0..=400).map(|k| limit(f64::from(k) / 100.0)).collect();
        assert!(steps.windows(2).all(|w| w[0] <= w[1]), "monotone");
        assert!(steps.iter().all(|&g| g > 0.45 && g <= 1.5), "bounded: {:?}", (steps[0], steps[400]));
        // The slope is 1 where the bend starts, so the factor has no kink.
        let slope = |g: f64| (limit(g + 1e-6) - limit(g - 1e-6)) / 2e-6;
        assert!((slope(1.35 + 1e-4) - 1.0).abs() < 1e-3 && (slope(0.65 - 1e-4) - 1.0).abs() < 1e-3);
    }

    #[test]
    fn gains_multiply_and_mix() {
        let a = Gain::by(0.1, 0.2, 0.3, 0.4);
        let b = Gain::round(0.5);
        assert_eq!(a * b, Gain { outer: 1.1 * 1.5, inner: 1.2 * 1.5, front: 1.3 * 1.5, back: 1.4 * 1.5 });
        assert_eq!(a.mix(b, 0.0), a);
        assert_eq!(a.mix(b, 1.0), b);
        assert_eq!(a * Gain::ONE, a);
    }
}
