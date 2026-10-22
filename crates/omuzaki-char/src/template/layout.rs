//! Joint layout and section curves: what a body template is made of ([`BodyTable`]), what it produces ([`BodyLayout`])
//! and the builder between them ([`build`]).
//!
//! A template is **data**: the left half of a skeleton (A-pose joint positions), the section curves of five regions, a
//! hand, and a few zones the sliders act on (see `soft_sd` for the first). [`build`] applies a character's
//! [`Proportions`], [`BodyShape`] and [`HandParams`] to that table and returns a [`BodyLayout`]: every humanoid joint
//! the body owns (all 55 VRM bones except `head`, `leftEye`, `rightEye` and `jaw`, which the head part publishes) and
//! the section curves of each [`BodyRegion`], which is a [`BodyFit`]. Left and right are built once and mirrored, so
//! symmetry holds exactly.
//!
//! **Section curves** are four [`Profile`]s (PCHIP, like the prototype's `curve`) over arc length along the region's
//! axis, a polyline through the region's joints:
//! - `across` and `through` move the section's centre off the axis, `rx` and `ry` are its half-extents;
//! - the section's frame is [`section_basis`]: `x` is world +X projected perpendicular to the axis, `y` is world +Z (+Y
//!   for the feet, where `radii.y` is the foot's half-height and the centre sits that far above the sole), so a vertical
//!   region reproduces the prototype's `[cx, y, cz]` centre with `rx`, `rz` radii exactly;
//! - a template gives its curves in its own coordinates (the prototype's heights), and the builder moves each knot to
//!   where its part of the axis went, so stretching a region keeps girth and the features slide with the joints.
//!
//! **Sliders.** `neck_length` and `torso_length` stretch their joint offsets along y; `shoulder_width` and `hip_width`
//! scale the x offsets of the arm and leg roots and the torso's x radii at those heights; `arm_length` and
//! `leg_length` scale segment lengths (the hips ride up on the legs so the soles stay at y = 0); `hand_size` and
//! `foot_size` scale the hand and foot sub-skeletons. Values outside `0.5..=2` are clamped and NaN reads as 1.
//!
//! **Body shape.** The five [`BodyShape`] axes act first, through [`shape`](super::shape): `height` lengthens the body
//! from the sole to the head base, `shoulder_hip` and `build` space the arm and leg roots, and every axis grows or
//! shrinks the sections as a [`Gain`] (a smooth factor on a section's radii) at landmarks the builder reads from the
//! template, so the template's own curves are never edited. Every axis at 0 is the identity, exactly.

use std::collections::BTreeMap;

use omuzaki_anim_api::{HUMANOID_COUNT, HumanoidBone};
use omuzaki_char_api::{BodyFit, BodyRegion, BodyShape, HandParams, JointRef, Proportions, Section};
use omuzaki_core::glam::{DVec2, DVec3, Vec3};

mod junction;

use self::junction::{Join, Outline};
use super::shape::{Gain, Marks, Shape};
use crate::kit::KitError;
use crate::kit::curve::{Pchip, clamp, fraction, lerp, sstep};

/// Half-width, in metres, of the window over which a region's axis turns a corner (the elbow): the section's axis
/// blends smoothly across it, so garments do not see a kink.
const AXIS_BLEND: f64 = 0.02;
/// The shortest segment of a region's axis. Shorter is a table mistake, not a template.
const MIN_SEGMENT: f64 = 1e-9;
/// Intervals along a region that the body shape's gain is sampled at: close enough that a bump a tenth of the region
/// wide (a deltoid, a calf) is drawn by several knots.
const GAIN_STEPS: usize = 96;
/// The same for a region joined to the torso: the blend between the torso's outline and the limb's own is a few
/// millimetres across, which the coarser steps would round off.
const JOIN_STEPS: usize = 4 * GAIN_STEPS;
/// How far, in metres, the template's thighs reach out of its pelvis (0.7 mm at soft-sd's): the most [`tuck`] leaves.
const TEMPLATE_POKE: f64 = 0.0008;
/// How far, in metres, [`trim_outer`] lets the thigh reach out of the pelvis: less than [`TEMPLATE_POKE`], so the curve
/// through the samples (which may bow out between them) stays within it.
const TRIM_POKE: f64 = 0.5 * TEMPLATE_POKE;
/// Over how much of the torso's length above its bottom [`trim_outer`] fades in.
const TRIM_FADE: f64 = 0.1;
/// How far below the torso's bottom, as a fraction of its length, [`tuck`] still holds a region to the torso's bottom
/// section.
const TUCK_MARGIN: f64 = 0.08;
/// Points around a section that [`tuck`] checks against the torso.
const TUCK_ANGLES: usize = 32;

// ------------------------------------------------------------------------------------------------ curves and regions

/// A scalar curve along a region's axis: a monotone cubic (PCHIP) through knots, so it never overshoots its data.
#[derive(Clone, Debug, PartialEq)]
pub struct Profile {
    knots: Vec<(f64, f64)>,
    curve: Pchip,
}

impl Profile {
    /// The curve through `(arc length, value)` knots, in any order. One knot is a constant.
    ///
    /// # Errors
    /// [`KitError`] for no knots, a NaN or infinite value, or two knots at the same arc length.
    pub fn new(knots: &[(f64, f64)]) -> Result<Self, KitError> {
        let curve = Pchip::new(knots)?;
        let mut sorted = knots.to_vec();
        sorted.sort_by(|a, b| a.0.total_cmp(&b.0));
        Ok(Self { knots: sorted, curve })
    }

    /// The value at arc length `s`; the end values hold outside the knots.
    pub fn eval(&self, s: f64) -> f64 {
        self.curve.eval(s)
    }

    /// The knots, sorted by arc length.
    pub fn knots(&self) -> &[(f64, f64)] {
        &self.knots
    }
}

/// A polyline with arc-length lookup: the centre line of a region.
#[derive(Clone, Debug, PartialEq)]
struct Path {
    points: Vec<DVec3>,
    /// `cum[i]` is the arc length from the start to `points[i]`.
    cum: Vec<f64>,
    length: f64,
}

impl Path {
    fn new(points: Vec<DVec3>) -> Result<Self, KitError> {
        if points.len() < 2 {
            return Err(KitError::TooFew { what: "points of a region's axis", need: 2, got: points.len() });
        }
        if points.iter().any(|p| !p.is_finite()) {
            return Err(KitError::NonFinite { what: "a region's axis" });
        }
        let mut cum = vec![0.0];
        let mut length = 0.0;
        for pair in points.windows(2) {
            let step = pair[0].distance(pair[1]);
            if step < MIN_SEGMENT {
                return Err(KitError::Degenerate { what: "a segment of a region's axis" });
            }
            length += step;
            cum.push(length);
        }
        Ok(Self { points, cum, length })
    }

    #[must_use]
    fn mirrored(&self) -> Self {
        Self { points: self.points.iter().map(|p| DVec3::new(-p.x, p.y, p.z)).collect(), ..self.clone() }
    }

    /// The segment containing arc length `s` (the first or last for `s` outside).
    fn segment(&self, s: f64) -> usize {
        self.cum.partition_point(|&c| c <= s).saturating_sub(1).min(self.points.len() - 2)
    }

    fn direction(&self, segment: usize) -> DVec3 {
        (self.points[segment + 1] - self.points[segment]) / (self.cum[segment + 1] - self.cum[segment])
    }

    fn point(&self, s: f64) -> DVec3 {
        let i = self.segment(s);
        let u = (s - self.cum[i]) / (self.cum[i + 1] - self.cum[i]);
        self.points[i].lerp(self.points[i + 1], u)
    }

    /// Half the blend window at interior vertex `v`: the full [`AXIS_BLEND`], less where the segments are short.
    fn blend_half(&self, v: usize) -> f64 {
        let before = self.cum[v] - self.cum[v - 1];
        let after = self.cum[v + 1] - self.cum[v];
        AXIS_BLEND.min(0.5 * before).min(0.5 * after)
    }

    /// The unit direction at `s`: the segment's, turned smoothly across a corner within its blend window.
    fn axis(&self, s: f64) -> DVec3 {
        let i = self.segment(s);
        let turn = |from: usize, v: usize| {
            let half = self.blend_half(v);
            let w = sstep(self.cum[v] - half, self.cum[v] + half, s);
            self.direction(from).lerp(self.direction(from + 1), w).try_normalize().unwrap_or(self.direction(from + 1))
        };
        if i >= 1 && s < self.cum[i] + self.blend_half(i) {
            return turn(i - 1, i);
        }
        if i + 2 < self.points.len() && s > self.cum[i + 1] - self.blend_half(i + 1) {
            return turn(i, i + 1);
        }
        self.direction(i)
    }
}

/// The directions a section's two radii point along: `x` is world +X projected perpendicular to `axis` (across the
/// body), `y` is `front` projected perpendicular to both (front to back). Unit vectors, at right angles to each other
/// and to the axis.
fn frame(axis: DVec3, front: DVec3) -> (DVec3, DVec3) {
    let across = (DVec3::X - axis * axis.x).try_normalize().unwrap_or(DVec3::X);
    let through = (front - axis * front.dot(axis) - across * front.dot(across)).try_normalize().unwrap_or(front);
    (across, through)
}

/// The directions of `radii.x` and `radii.y` for a section of `region` whose axis is `axis`: what a garment needs to
/// turn a [`Section`] into an ellipse. `x` lies across the body (world +X projected perpendicular to the axis, for
/// both sides), `y` points front to back (+Z projected), and for the feet, whose `radii.y` is height, up (+Y).
pub fn section_basis(region: BodyRegion, axis: Vec3) -> (Vec3, Vec3) {
    let (across, through) = frame(axis.as_dvec3(), front_of(region));
    (across.as_vec3(), through.as_vec3())
}

/// The direction `radii.y` leans towards, before projection: up for the feet, forward for everything else.
fn front_of(region: BodyRegion) -> DVec3 {
    match region {
        BodyRegion::FootL | BodyRegion::FootR => DVec3::Y,
        _ => DVec3::Z,
    }
}

/// A cross-section at full precision (the `f64` form of `Section`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct RegionSection {
    /// Centre of the ellipse, bind space.
    pub centre: DVec3,
    /// The region's axis here (unit), pointing from the region's start to its end.
    pub axis: DVec3,
    /// Half-extents along [`section_basis`]'s `x` and `y`.
    pub radii: DVec2,
}

impl From<RegionSection> for Section {
    fn from(s: RegionSection) -> Self {
        Self { centre: s.centre.as_vec3(), axis: s.axis.as_vec3(), radii: s.radii.as_vec2() }
    }
}

/// The body shape's gain along a region, as three profiles over arc length (see [`Gain`]).
#[derive(Clone, Debug, PartialEq)]
struct Gains {
    outer: Profile,
    inner: Profile,
    front: Profile,
    back: Profile,
}

/// The section curves of one region: a centre line and four profiles along it, and the body shape's gain on them.
#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    path: Path,
    /// `+1` for a region built as given, `-1` for its mirror image (the across offset is along world +X for both).
    lateral: f64,
    front: DVec3,
    across: Profile,
    through: Profile,
    rx: Profile,
    ry: Profile,
    /// The body shape's gain, or `None` where the shape leaves the region as the template made it (every gain is `1`).
    gains: Option<Gains>,
}

impl Region {
    /// The length of the region's axis in metres.
    pub fn length(&self) -> f64 {
        self.path.length
    }

    /// The point of the region's centre line (the polyline through its joints, before the section curves move the centre
    /// off it) a fraction `t` of the way along (clamped to `0..=1`).
    pub fn path_point(&self, t: f64) -> DVec3 {
        self.path.point(clamp(t, 0.0, 1.0) * self.path.length)
    }

    /// The section a fraction `t` of the way along the axis (clamped to `0..=1`).
    pub fn section(&self, t: f64) -> RegionSection {
        let s = clamp(t, 0.0, 1.0) * self.path.length;
        let axis = self.path.axis(s);
        let (across, through) = frame(axis, self.front);
        let (mut rx, mut ry) = (self.rx.eval(s), self.ry.eval(s));
        let (mut side, mut lean) = (0.0, 0.0);
        if let Some(g) = &self.gains {
            // On each axis the section grows by the mean of its two factors and its centre moves by half their
            // difference, so a bust, a calf or a thigh can bulge one way and leave the other edge where it was.
            let (outer, inner, front, back) = (g.outer.eval(s), g.inner.eval(s), g.front.eval(s), g.back.eval(s));
            side = rx * (outer - inner) / 2.0;
            lean = ry * (front - back) / 2.0;
            rx *= f64::midpoint(outer, inner);
            ry *= f64::midpoint(front, back);
        }
        let centre = self.path.point(s)
            + across * (self.lateral * (self.across.eval(s) + side))
            + through * (self.through.eval(s) + lean);
        RegionSection { centre, axis, radii: DVec2::new(rx, ry) }
    }

    /// The same region on the other side of the body (x negated).
    #[must_use]
    pub fn mirrored(&self) -> Self {
        Self { path: self.path.mirrored(), lateral: -self.lateral, ..self.clone() }
    }
}

// ------------------------------------------------------------------------------------------------ the layout

/// Where a built body's joints sit and how wide it is along each region: a template's answer to a set of sliders.
///
/// It is the body's [`BodyFit`]: garments and the skeleton read it, and nothing else about the template.
#[derive(Clone, Debug)]
pub struct BodyLayout {
    /// Bind-space joint positions, indexed by [`HumanoidBone::index`]; `None` for the bones the body does not own.
    joints: [Option<DVec3>; HUMANOID_COUNT],
    head_base: Option<DVec3>,
    regions: BTreeMap<BodyRegion, Region>,
    hand: Option<HandTable>,
}

impl BodyLayout {
    /// A layout with no joints and no regions: every lookup reads `None`.
    pub fn empty() -> Self {
        Self { joints: [None; HUMANOID_COUNT], head_base: None, regions: BTreeMap::new(), hand: None }
    }

    /// The position of a body joint at full precision. `None` for `head`, the eyes and the jaw (the head part's).
    pub fn joint64(&self, bone: HumanoidBone) -> Option<DVec3> {
        self.joints[bone.index()]
    }

    /// The hand table the layout was built from (the template's own hand: its size, finger and thumb numbers), which
    /// the hand meshes are built on so they reach the joints placed from it. `None` for an empty layout.
    pub fn hand_table(&self) -> Option<&HandTable> {
        self.hand.as_ref()
    }

    /// Every joint the body owns, parents before children.
    pub fn joints(&self) -> impl Iterator<Item = (HumanoidBone, DVec3)> + '_ {
        HumanoidBone::ALL.into_iter().filter_map(|b| self.joint64(b).map(|p| (b, p)))
    }

    /// Where the head part's `head` joint belongs: the end of the neck bone, which moves with the neck, torso and leg
    /// sliders. The head part places its own skeleton and mesh from here.
    pub fn head_base(&self) -> Option<DVec3> {
        self.head_base
    }

    /// The curves of `region`.
    pub fn region(&self, region: BodyRegion) -> Option<&Region> {
        self.regions.get(&region)
    }

    /// The section of `region` a fraction `t` along its axis, at full precision.
    pub fn section64(&self, region: BodyRegion, t: f64) -> Option<RegionSection> {
        self.region(region).map(|r| r.section(t))
    }

    fn put(&mut self, bone: HumanoidBone, p: DVec3) {
        self.joints[bone.index()] = Some(p);
    }

    /// Sets a left bone and, mirrored in x, its right counterpart.
    fn put_pair(&mut self, left: HumanoidBone, p: DVec3) {
        self.put(left, p);
        self.put(left.mirrored(), DVec3::new(-p.x, p.y, p.z));
    }
}

impl BodyFit for BodyLayout {
    fn section(&self, region: BodyRegion, t: f32) -> Option<Section> {
        t.is_finite().then(|| self.section64(region, f64::from(t))).flatten().map(Section::from)
    }

    fn joint(&self, joint: &JointRef) -> Option<Vec3> {
        match joint {
            JointRef::Humanoid(bone) => self.joint64(*bone).map(DVec3::as_vec3),
            JointRef::Named(_) => None,
        }
    }
}

// ------------------------------------------------------------------------------------------------ template tables

/// Knots of a curve in a template's own coordinates: `(coordinate along the region's axis, value)`.
pub type Knots = &'static [(f64, f64)];

/// A curve that is zero everywhere: a section centred on its axis.
pub const ON_AXIS: Knots = &[(0.0, 0.0)];

/// The section curves of one region, as a template states them.
#[derive(Copy, Clone, Debug)]
pub struct RegionCurves {
    /// The coordinate (the prototype's height `y` for vertical regions, arc length for arms, `z` for feet) at which the
    /// region starts.
    pub origin: f64,
    /// `1` if the coordinate grows along the axis, `-1` if it falls (the legs run down from the hip).
    pub direction: f64,
    /// Where the centre sits across the body, relative to the axis (the prototype's `cx` minus the leg's x).
    pub across: Knots,
    /// Where the centre sits front to back, relative to the axis (the prototype's `cz`); `None` puts the centre half a
    /// height above the sole, so the section rests on the floor (feet).
    pub through: Option<Knots>,
    /// Half-extent across the body (the prototype's `rx`).
    pub rx: Knots,
    /// Half-extent front to back (the prototype's `rz`), or the half-height of a foot.
    pub ry: Knots,
}

/// The joints of the left half of a body in the template's A-pose (`x >= 0`), and the centre line, at the template's
/// default proportions. The right half is the mirror image.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Joints {
    /// `hips`.
    pub hips: DVec3,
    /// `spine`.
    pub spine: DVec3,
    /// `chest`.
    pub chest: DVec3,
    /// `upperChest`.
    pub upper_chest: DVec3,
    /// `neck`.
    pub neck: DVec3,
    /// Where the head part's `head` joint sits: the end of the neck bone.
    pub head_base: DVec3,
    /// `leftShoulder` (the collar bone's root).
    pub shoulder: DVec3,
    /// `leftUpperArm`.
    pub upper_arm: DVec3,
    /// `leftLowerArm`.
    pub lower_arm: DVec3,
    /// `leftHand` (the wrist).
    pub hand: DVec3,
    /// `leftUpperLeg`.
    pub upper_leg: DVec3,
    /// `leftLowerLeg`.
    pub lower_leg: DVec3,
    /// `leftFoot` (the ankle).
    pub foot: DVec3,
    /// `leftToes`.
    pub toes: DVec3,
}

/// Where each region's axis starts and ends beyond its joints, as offsets from them (the sections run a little past
/// the bones: the pelvis under the hips, the neck's top inside the chin).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Extents {
    /// Torso start, from `hips`.
    pub torso_bottom: DVec3,
    /// Torso end (and neck start), from `neck`.
    pub torso_top: DVec3,
    /// Neck end, from the head base.
    pub neck_top: DVec3,
    /// Arm start, from `leftUpperArm` (the sleeve begins just inside the shoulder).
    pub arm_start: DVec3,
    /// Leg start, from `leftUpperLeg`.
    pub leg_top: DVec3,
    /// Leg end, from `leftFoot`.
    pub leg_bottom: DVec3,
    /// Heel, as a z offset from `leftFoot` (negative).
    pub heel: f64,
    /// Toe tip, as a z offset from `leftFoot`.
    pub toe_tip: f64,
}

/// Heights along the torso, in the template's own coordinates, where sliders and shape axes act.
#[derive(Copy, Clone, Debug)]
pub struct Zones {
    /// `1` in the hip zone, falling to `0` above it (the prototype's `weightHips`): `hip_width` scales the torso's x
    /// radii by this much.
    pub hip: fn(f64) -> f64,
    /// `[rise start, rise end, fall start, fall end]` of the shoulder zone, which `shoulder_width` scales.
    pub shoulder: [f64; 4],
    /// The waist, where `curves` pinches.
    pub waist: f64,
    /// The bust, where `curves` adds depth.
    pub bust: f64,
}

impl Zones {
    /// `1` across the shoulders, `0` below and above them.
    fn shoulder_weight(&self, y: f64) -> f64 {
        let [rise_from, rise_to, fall_from, fall_to] = self.shoulder;
        sstep(rise_from, rise_to, y) * (1.0 - sstep(fall_from, fall_to, y))
    }
}

/// The left hand's layout, in hand-frame coordinates: `(along the forearm, towards the thumb, towards the palm)`.
#[derive(Copy, Clone, Debug)]
pub struct HandTable {
    /// How far past the wrist joint the hand's origin lies, along the forearm (the prototype's `HAND_SHIFT`).
    pub shift: f64,
    /// The way the palm faces before it is projected perpendicular to the forearm (towards the thigh).
    pub palm_hint: DVec3,
    /// The way the thumb side faces before projection (forward).
    pub thumb_hint: DVec3,
    /// From the hand's origin to the knuckles.
    pub knuckle: f64,
    /// Where each finger lies towards the thumb (index, middle, ring, little).
    pub finger_across: [f64; 4],
    /// Each finger's length from knuckle to tip.
    pub finger_length: [f64; 4],
    /// Where along a finger its proximal, intermediate and distal joints sit, as fractions of its length.
    pub joint_split: [f64; 3],
    /// How far the fingers curl towards the palm at distance `u` from the hand's origin.
    pub curl: fn(f64) -> f64,
    /// The thumb.
    pub thumb: ThumbTable,
}

/// The left thumb, in hand-frame coordinates measured from the hand's origin.
#[derive(Copy, Clone, Debug)]
pub struct ThumbTable {
    /// The metacarpal joint, at the base of the palm.
    pub base: DVec3,
    /// The proximal joint, where the free thumb begins.
    pub root: DVec3,
    /// The direction of the free thumb (it is normalised).
    pub direction: DVec3,
    /// The free thumb's length, root to tip.
    pub length: f64,
    /// Where the distal joint sits along it, as a fraction.
    pub split: f64,
}

/// Everything a template is: joints, extents, curves, zones and the hand.
#[derive(Copy, Clone, Debug)]
pub struct BodyTable {
    /// The left half of the skeleton.
    pub joints: Joints,
    /// Where regions start and end beyond their joints.
    pub extents: Extents,
    /// The torso's curves (start at the pelvis, end where the neck begins).
    pub torso: RegionCurves,
    /// The neck's curves (start where the torso ends, end inside the head).
    pub neck: RegionCurves,
    /// The left arm's curves (start at the shoulder, end at the wrist).
    pub arm: RegionCurves,
    /// The left leg's curves (start above the hip, end at the ankle).
    pub leg: RegionCurves,
    /// The left foot's curves (start at the heel, end at the toe tip).
    pub foot: RegionCurves,
    /// Where the sliders act.
    pub zones: Zones,
    /// The left hand.
    pub hand: HandTable,
}

// ------------------------------------------------------------------------------------------------ the builder

/// The sliders, sanitised, and the body shape, combined into the factors the builder applies.
struct Factors {
    neck: f64,
    torso: f64,
    arm: f64,
    leg: f64,
    /// The width sliders (not the shape's `shoulder_hip`, which the shape's gains and spacings carry).
    shoulder: f64,
    hip: f64,
    hand: f64,
    foot: f64,
    /// The body's length factor from `height`.
    length: f64,
    /// The shape's factors on the arm roots' and the legs' distance from the body's axis.
    arm_spacing: f64,
    leg_spacing: f64,
    /// How much further out the arms hang than the template's, so they clear a trunk wider than the shoulders put them
    /// and arms thicker than the template's (`1` or more; [`ArmHang`]).
    arm_swing: f64,
    /// How far, in metres, the arms' roots sit outside the shoulders when the spacing factor holds them out from a
    /// narrower trunk: the start of the arm's sections is brought in by it.
    arm_tuck: f64,
}

impl Factors {
    fn new(p: &Proportions, shape: &Shape, marks: &Marks, hang: &ArmHang) -> Self {
        let slider = |v: f64| if v.is_finite() { clamp(v, 0.5, 2.0) } else { 1.0 };
        let length = shape.length();
        let (shoulder, hip) = (slider(p.shoulder_width), slider(p.hip_width));
        let arm_spacing = shape.arm_spacing(marks);
        Self {
            neck: slider(p.neck_length) * length,
            torso: slider(p.torso_length) * length,
            arm: slider(p.arm_length) * length,
            leg: slider(p.leg_length) * length,
            shoulder,
            hip,
            hand: slider(p.hand_size),
            foot: slider(p.foot_size),
            length,
            arm_spacing,
            leg_spacing: shape.leg_spacing(marks),
            arm_tuck: shape.arm_slack(marks).max(0.0) * hang.root * shoulder,
            arm_swing: hang.swing(
                shape.hang_width(marks) * hip,
                shape.arm_girth(marks),
                shoulder * arm_spacing,
                length * slider(p.arm_length),
            ),
        }
    }
}

/// The steepest a swung arm segment gets from the vertical: `sin` of 58 degrees.
const MAX_SWING_SIN: f64 = 0.85;

/// An arm segment `d` turned outward in the front plane until its sideways reach is `swing` times what it was (at most
/// [`MAX_SWING_SIN`] of its length), keeping its length. `swing` of `1` returns it as it is.
fn swung(d: DVec3, swing: f64) -> DVec3 {
    let along = d.x.hypot(d.y);
    if swing <= 1.0 || d.x <= 0.0 || along < MIN_SEGMENT {
        return d;
    }
    let sin = (d.x / along * swing).min(MAX_SWING_SIN);
    DVec3::new(along * sin, along * (1.0 - sin * sin).sqrt() * d.y.signum(), d.z)
}

/// The joints with the factors applied. Segment vectors scale, so each joint keeps its place in its limb.
fn place(n: &Joints, f: &Factors) -> Joints {
    let stacked = |base: DVec3, from: DVec3, to: DVec3, k: f64| base + (to - from) * k;
    // Legs, from the floor up: the ankle rises with the foot and the body's length, the hips ride up on the legs (and
    // their small offset from the hip joint is part of the body's length), so soles stay at y = 0.
    let leg_x = f.hip * f.leg_spacing;
    // The hip root sits at the spaced offset and the rest of the leg hangs from it by the template's own offsets (scaled
    // with the leg), so a leg that leans, as a short template's does, keeps its bone lengths whatever the spacing.
    let upper_x = n.upper_leg.x * leg_x;
    let lower_x = upper_x + (n.lower_leg.x - n.upper_leg.x) * f.leg;
    let foot = DVec3::new(lower_x + (n.foot.x - n.lower_leg.x) * f.leg, n.foot.y * f.foot * f.length, n.foot.z);
    // The toes belong to the foot, not to the ankle's height: they stay where the foot's own size puts them.
    let toes =
        DVec3::new(foot.x + (n.toes.x - n.foot.x) * f.foot, n.toes.y * f.foot, foot.z + (n.toes.z - n.foot.z) * f.foot);
    let lower_leg = stacked(foot, n.foot, n.lower_leg, f.leg);
    let upper_leg = stacked(lower_leg, n.lower_leg, n.upper_leg, f.leg);
    let hips = DVec3::new(n.hips.x, upper_leg.y + (n.hips.y - n.upper_leg.y) * f.length, n.hips.z);
    // The torso stretches along y from the hips; widths stay.
    let above = |p: DVec3, x: f64| DVec3::new(x, hips.y + (p.y - n.hips.y) * f.torso, hips.z + (p.z - n.hips.z));
    let neck = above(n.neck, n.neck.x);
    let arm_x = f.shoulder * f.arm_spacing;
    let upper_arm = above(n.upper_arm, n.upper_arm.x * arm_x);
    let lower_arm = upper_arm + swung(n.lower_arm - n.upper_arm, f.arm_swing) * f.arm;
    Joints {
        hips,
        spine: above(n.spine, n.spine.x),
        chest: above(n.chest, n.chest.x),
        upper_chest: above(n.upper_chest, n.upper_chest.x),
        neck,
        head_base: neck + (n.head_base - n.neck) * DVec3::new(1.0, f.neck, 1.0),
        shoulder: above(n.shoulder, n.shoulder.x * arm_x),
        upper_arm,
        lower_arm,
        hand: lower_arm + swung(n.hand - n.lower_arm, f.arm_swing) * f.arm,
        upper_leg,
        lower_leg,
        foot,
        toes,
    }
}

/// The corner points of each (left) region's axis.
struct Axes {
    torso: Path,
    neck: Path,
    arm: Path,
    leg: Path,
    foot: Path,
}

impl Axes {
    /// The axes through `j`, with the foot-sized extents scaled by `foot` and the arm's start `tuck`ed that far in
    /// towards the body (so an arm held out from a narrow shoulder still starts inside it).
    fn of(j: &Joints, e: &Extents, foot: f64, tuck: f64) -> Result<Self, KitError> {
        let torso_top = j.neck + e.torso_top;
        Ok(Self {
            torso: Path::new(vec![
                j.hips + e.torso_bottom,
                j.hips,
                j.spine,
                j.chest,
                j.upper_chest,
                j.neck,
                torso_top,
            ])?,
            // Two points: with a short neck the head base can lie below the torso's top, and the axis must not fold back.
            neck: Path::new(vec![torso_top, j.head_base + e.neck_top])?,
            arm: Path::new(vec![j.upper_arm + e.arm_start - DVec3::X * tuck, j.lower_arm, j.hand])?,
            leg: Path::new(vec![
                j.upper_leg + e.leg_top,
                j.upper_leg,
                j.lower_leg,
                j.foot,
                j.foot + e.leg_bottom * foot,
            ])?,
            // The sole is the floor: y = 0 whatever the legs do.
            foot: Path::new(vec![
                DVec3::new(j.foot.x, 0.0, j.foot.z + e.heel * foot),
                DVec3::new(j.foot.x, 0.0, j.foot.z + e.toe_tip * foot),
            ])?,
        })
    }
}

/// What the template says about how its arm hangs beside the body, in metres, half way down the upper arm, at the elbow
/// and at the hand: the
/// sideways distance from the arm's root, the trunk's half-width at that height, the arm's own half-width, and the gap
/// between arm and trunk.
struct ArmHang {
    /// Where the arm's root is, from the body's axis.
    root: f64,
    points: [HangPoint; 3],
}

#[derive(Copy, Clone)]
struct HangPoint {
    reach: f64,
    trunk: f64,
    arm: f64,
    gap: f64,
}

/// The share of the template's gap between arm and trunk that a swung arm keeps.
const KEEP_GAP: f64 = 0.15;

impl ArmHang {
    fn of(table: &BodyTable, native: &Axes) -> Result<Self, KitError> {
        // Below the torso the trunk's width holds at the pelvis's: a good stand-in for the thighs the hand hangs by.
        let trunk = Pchip::new(table.torso.rx)?;
        let arm = Pchip::new(table.arm.rx)?;
        let j = &table.joints;
        let point = |joint: DVec3, arc: f64| {
            let (trunk, arm) = (trunk.eval(joint.y), arm.eval(arc));
            HangPoint { reach: joint.x - j.upper_arm.x, trunk, arm, gap: (joint.x - arm - trunk).max(0.0) }
        };
        // Half way down the upper arm, the elbow and the hand: the trunk can be widest at any of their heights.
        let (half_way, elbow) = (j.upper_arm.lerp(j.lower_arm, 0.5), native.arm.cum[1]);
        Ok(Self {
            root: j.upper_arm.x,
            points: [point(half_way, 0.5 * elbow), point(j.lower_arm, elbow), point(j.hand, native.arm.cum[2])],
        })
    }

    /// The factor on the arm's sideways reach that keeps it clear of the trunk (`1` or more). `trunk` and `arm` are
    /// the factors on the trunk's and the arm's half-widths, `root` the factor on where the arm's root sits, and
    /// `length` the factor on the arm's length.
    fn swing(&self, trunk: f64, arm: f64, root: f64, length: f64) -> f64 {
        self.points
            .iter()
            .map(|p| (trunk * p.trunk + arm * p.arm + KEEP_GAP * p.gap - self.root * root) / (p.reach * length))
            .fold(1.0, f64::max)
    }
}

impl BodyTable {
    /// Where the landmarks the body shape hangs on (waist, bust, shoulder line, elbow, knee, ...) sit, as fractions of
    /// each region's length.
    ///
    /// # Errors
    /// [`KitError`] if the table does not describe a valid layout (as [`build`]).
    pub fn marks(&self) -> Result<Marks, KitError> {
        marks(self, &Axes::of(&self.joints, &self.extents, 1.0, 0.0)?)
    }
}

/// Where the landmarks the body shape hangs on sit in `table`, as fractions of each region's length.
fn marks(table: &BodyTable, native: &Axes) -> Result<Marks, KitError> {
    let z = &table.zones;
    // Both curves are keyed by the template's height at the torso's bottom, which is where the legs leave it.
    let pelvis = Pchip::new(table.torso.rx)?.eval(table.torso.origin);
    let thigh = Pchip::new(table.leg.rx)?.eval(table.torso.origin);
    let on_torso = |y: f64| table.torso.direction * (y - table.torso.origin) / native.torso.length;
    let at = |path: &Path, corner: usize| path.cum[corner] / path.length;
    Ok(Marks {
        waist: on_torso(z.waist),
        bust: on_torso(z.bust),
        shoulder: on_torso(f64::midpoint(z.shoulder[1], z.shoulder[2])),
        arm_root: on_torso(table.joints.upper_arm.y),
        elbow: at(&native.arm, 1),
        hip_joint: at(&native.leg, 1),
        // The legs are vertical in a template's own frame, so the torso's bottom is as far down them as its height says.
        crotch: clamp(
            (native.leg.points[0].y - native.torso.points[0].y)
                / (native.leg.points[0].y - native.leg.points[native.leg.points.len() - 1].y),
            0.0,
            1.0,
        ),
        pelvis_ratio: pelvis / thigh.max(f64::EPSILON),
        knee: at(&native.leg, 2),
        ankle: at(&native.leg, 3),
    })
}

/// How the sliders and the body shape change one region: what [`region`] applies to the template's curves.
struct Look<'a> {
    /// Factor on the template's radii (the foot-size slider scales the foot's section, centre height included).
    size: f64,
    /// Factor on the template's across offsets: the legs' offsets spread with the legs.
    across: f64,
    /// The gain at a point of the region, given its template coordinate and its fraction along the template's axis;
    /// `None` leaves the radii as the template made them.
    gain: Option<&'a dyn Fn(f64, f64) -> Gain>,
    /// The torso, shaped, which the gain must not let this region poke out of: the legs, which run up inside it.
    inside: Option<&'a Region>,
    /// The torso's outline, which this region follows where it leaves the torso (the hip).
    join: Option<Join<'a>>,
}

/// Maps arc lengths along a template's axis to the same places on the placed axis, piecewise linearly between the
/// corners (outside, with slope 1).
struct Warp {
    from: Vec<f64>,
    to: Vec<f64>,
}

impl Warp {
    fn apply(&self, s: f64) -> f64 {
        let last = self.from.len() - 1;
        if s <= self.from[0] {
            return self.to[0] + (s - self.from[0]);
        }
        if s >= self.from[last] {
            return self.to[last] + (s - self.from[last]);
        }
        let i = self.from.partition_point(|&c| c <= s) - 1;
        lerp(self.to[i], self.to[i + 1], (s - self.from[i]) / (self.from[i + 1] - self.from[i]))
    }
}

/// The body shape's gain along a region: sampled at `steps` + 1 fractions of the template's axis, each moved to
/// where that part of the axis went.
fn gain_samples(
    spec: &RegionCurves,
    native: &Path,
    warp: &Warp,
    steps: usize,
    gain: &dyn Fn(f64, f64) -> Gain,
) -> Vec<(f64, Gain)> {
    (0..=steps)
        .map(|k| {
            let v = fraction(k, steps);
            let along = v * native.length;
            (warp.apply(along), gain(spec.origin + spec.direction * along, v))
        })
        .collect()
}

/// The gain profiles through `samples`, or `None` if the gain is `1` everywhere, which keeps an unshaped region exactly
/// the template's.
fn gains_of(samples: &[(f64, Gain)]) -> Result<Option<Gains>, KitError> {
    if samples.iter().all(|&(_, g)| g == Gain::ONE) {
        return Ok(None);
    }
    let profile =
        |pick: fn(&Gain) -> f64| Profile::new(&samples.iter().map(|(s, g)| (*s, pick(g))).collect::<Vec<_>>());
    Ok(Some(Gains {
        outer: profile(|g| g.outer)?,
        inner: profile(|g| g.inner)?,
        front: profile(|g| g.front)?,
        back: profile(|g| g.back)?,
    }))
}

/// A region that runs up inside the torso (a leg, into the pelvis), and what it takes to keep it there.
struct Inside<'a> {
    base: &'a Region,
    torso: &'a Region,
    /// Heights of the torso's bottom and top sections.
    bottom: f64,
    top: f64,
}

impl<'a> Inside<'a> {
    fn new(base: &'a Region, torso: &'a Region) -> Self {
        Self { base, torso, bottom: torso.section(0.0).centre.y, top: torso.section(1.0).centre.y }
    }

    /// The centre of `base`'s section at `s` and its radii.
    fn frame(&self, s: f64) -> (DVec2, DVec2) {
        let (b, p) = (self.base, self.base.path.point(s));
        (DVec2::new(p.x + b.across.eval(s), p.z + b.through.eval(s)), DVec2::new(b.rx.eval(s), b.ry.eval(s)))
    }

    /// The region's section at `s` under `g`: its edges (outer, inner, front, back) as coordinates.
    fn edges(&self, s: f64, g: Gain) -> [f64; 4] {
        let (c, r) = self.frame(s);
        [c.x + r.x * g.outer, c.x - r.x * g.inner, c.y + r.y * g.front, c.y - r.y * g.back]
    }

    /// The torso's section at the height of `base`'s at `s`, if the torso reaches it. A little below the torso's bottom
    /// counts as its bottom: the leg there is leaving it, and its gain, between the knots on either side, must not jump.
    fn torso_at(&self, s: f64) -> Option<RegionSection> {
        let at = self.height_at(s);
        (-TUCK_MARGIN..=1.0).contains(&at).then(|| self.torso.section(at.max(0.0)))
    }

    /// Where the section at `s` is along the torso, from `0` at its bottom to `1` at its top (negative below it).
    fn height_at(&self, s: f64) -> f64 {
        (self.base.path.point(s).y - self.bottom) / (self.top - self.bottom)
    }

    /// How far the section with `edges` reaches into the torso's ellipse `t`: `1` at its edge, over `1` outside it.
    fn reach(edges: [f64; 4], t: &RegionSection) -> f64 {
        let (cx, cz) = (f64::midpoint(edges[0], edges[1]), f64::midpoint(edges[2], edges[3]));
        let (rx, rz) = ((edges[0] - edges[1]) / 2.0, (edges[2] - edges[3]) / 2.0);
        (0..TUCK_ANGLES)
            .map(|k| {
                let a = fraction(k, TUCK_ANGLES) * std::f64::consts::TAU;
                let (dx, dz) =
                    ((cx + rx * a.cos() - t.centre.x) / t.radii.x, (cz + rz * a.sin() - t.centre.z) / t.radii.y);
                dx * dx + dz * dz
            })
            .fold(0.0, f64::max)
    }

    /// The most [`Inside::reach`] may be: the edge, and the sliver the template's own thighs poke out of the pelvis by.
    fn allowed(t: &RegionSection) -> f64 {
        (1.0 + TEMPLATE_POKE / t.radii.max_element()).powi(2)
    }

    /// The factor that brings the section at `s` under `g` inside the torso (to where the template's reached), if it
    /// is not.
    fn fit(&self, s: f64, g: Gain) -> f64 {
        let Some(t) = self.torso_at(s) else { return 1.0 };
        let (now, allowed) = (Self::reach(self.edges(s, g), &t), Self::allowed(&t));
        // Half way through the margin below the torso's bottom the hold fades out, so it does not end in a step where the
        // torso ends.
        let hold = sstep(-TUCK_MARGIN, -0.5 * TUCK_MARGIN, self.height_at(s));
        if now > allowed { 1.0 - hold * (1.0 - (allowed / now).sqrt()) } else { 1.0 }
    }
}

/// Scales the gains in `samples` where they would put the region outside the torso, in the horizontal section at the same
/// height (across the body and front to back), towards the torso's axis: sideways and front-to-back positions and
/// radii alike, so a leg narrows and flattens as the torso does and stays inside it. The region may reach the torso's
/// edge by [`TEMPLATE_POKE`] (its thighs reach that far out of its pelvis), and where it already fits nothing changes. Each
/// knot also answers for the half step either side of it, where the interpolated gain and the torso's curve could part.
fn tuck(samples: &mut [(f64, Gain)], inside: &Inside<'_>) {
    let before = samples.to_vec();
    for (i, (s, g)) in samples.iter_mut().enumerate() {
        let mut k = inside.fit(*s, *g);
        for (other_s, other) in [i.checked_sub(1), Some(i + 1)].into_iter().flatten().filter_map(|j| before.get(j)) {
            let mid = Gain {
                outer: f64::midpoint(g.outer, other.outer),
                inner: f64::midpoint(g.inner, other.inner),
                front: f64::midpoint(g.front, other.front),
                back: f64::midpoint(g.back, other.back),
            };
            k = k.min(inside.fit(f64::midpoint(*s, *other_s), mid));
        }
        if k < 1.0 {
            // Scale the section about the torso's centre, then read the gains that make it so.
            let at = (inside.base.path.point(*s).y - inside.bottom) / (inside.top - inside.bottom);
            let t = inside.torso.section(at.clamp(0.0, 1.0));
            let [outer, inner, front, back] = inside.edges(*s, *g);
            let (c, r) = inside.frame(*s);
            let pull = |edge: f64, about: f64| about + k * (edge - about);
            g.outer = (pull(outer, t.centre.x) - c.x) / r.x;
            g.inner = (c.x - pull(inner, t.centre.x)) / r.x;
            g.front = (pull(front, t.centre.z) - c.y) / r.y;
            g.back = (c.y - pull(back, t.centre.z)) / r.y;
        }
    }
}

/// Draws the outer edge back, where a join has put it out of the torso, as far as keeps the section inside it: the least
/// that does, so the edge stays on the torso's outline wherever the section fits there (and where it does not, it is a
/// sliver, the section's front and back curving away from the outline faster than the torso's).
fn trim_outer(samples: &mut [(f64, Gain)], inside: &Inside<'_>) {
    const ITERATIONS: usize = 40;
    for (s, g) in samples {
        let Some(t) = inside.torso_at(*s) else { continue };
        // Off at the torso's bottom and below, where the edge is the outline's own and no torso is left to hold it in,
        // fading in above it: a cut would step the edge.
        let held = sstep(0.0, TRIM_FADE, inside.height_at(*s));
        if held <= 0.0 {
            continue;
        }
        // Within what [`tuck`] allows, with room for the curve between samples to bow out.
        let allowed = (1.0 + TRIM_POKE / t.radii.max_element()).powi(2);
        let edges = inside.edges(*s, *g);
        if Inside::reach(edges, &t) <= allowed {
            continue;
        }
        // The outer edge somewhere between the inner edge (a section with no width fits) and where it is.
        let (mut fits, mut pokes) = (edges[1], edges[0]);
        for _ in 0..ITERATIONS {
            let mid = f64::midpoint(fits, pokes);
            if Inside::reach([mid, edges[1], edges[2], edges[3]], &t) <= allowed {
                fits = mid;
            } else {
                pokes = mid;
            }
        }
        let (c, r) = inside.frame(*s);
        g.outer += held * ((fits - c.x) / r.x - g.outer);
    }
}

/// One region: the template's curves moved onto the placed axis, with the sliders' and the body shape's changes applied.
fn region(spec: &RegionCurves, native: &Path, placed: Path, front: DVec3, look: &Look<'_>) -> Result<Region, KitError> {
    let warp = Warp { from: native.cum.clone(), to: placed.cum.clone() };
    // Each knot keeps its place on the axis: its coordinate becomes an arc length, moved to where that arc went.
    let profile = |knots: Knots, scale: f64| {
        let moved: Vec<(f64, f64)> =
            knots.iter().map(|&(u, v)| (warp.apply(spec.direction * (u - spec.origin)), v * scale)).collect();
        Profile::new(&moved)
    };
    let (rx, ry) = (profile(spec.rx, look.size)?, profile(spec.ry, look.size)?);
    let through = match spec.through {
        Some(knots) => profile(knots, 1.0)?,
        None => ry.clone(),
    };
    let mut region = Region {
        across: profile(spec.across, look.across)?,
        path: placed,
        lateral: 1.0,
        front,
        through,
        rx,
        ry,
        gains: None,
    };
    if let Some(gain) = look.gain {
        let steps = if look.join.is_some() { JOIN_STEPS } else { GAIN_STEPS };
        let mut samples = gain_samples(spec, native, &warp, steps, gain);
        let shaped: Vec<f64> = samples.iter().map(|(_, g)| g.outer).collect();
        let inside = look.inside.map(|torso| Inside::new(&region, torso));
        if let Some(inside) = &inside {
            tuck(&mut samples, inside);
        }
        if let Some(join) = look.join {
            join.apply(&mut samples, &shaped, &region);
            // The join put the outer edge on the torso's outline, where the section's front and back may still be out.
            if let Some(inside) = &inside {
                trim_outer(&mut samples, inside);
            }
        }
        region.gains = gains_of(&samples)?;
    }
    Ok(region)
}

/// The hand frame at the wrist: unit vectors along the forearm, towards the palm and towards the thumb side (the
/// prototype's `handFrame`: `A`, `N`, `T`).
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct HandFrame {
    /// Along the forearm, elbow to wrist.
    pub axis: DVec3,
    /// Out of the palm: the palm hint projected perpendicular to the axis.
    pub palm: DVec3,
    /// Towards the thumb side: the thumb hint projected perpendicular to both.
    pub thumb: DVec3,
}

impl HandFrame {
    /// The frame for a forearm pointing along `forearm` and the table's hints.
    pub fn of(forearm: DVec3, palm_hint: DVec3, thumb_hint: DVec3) -> Self {
        let reject = |v: DVec3, onto: DVec3| v - onto * v.dot(onto);
        let axis = forearm.try_normalize().unwrap_or(DVec3::NEG_Y);
        let palm = reject(palm_hint, axis).try_normalize().unwrap_or(DVec3::NEG_X);
        let thumb = reject(reject(thumb_hint, axis), palm).try_normalize().unwrap_or(DVec3::Z);
        Self { axis, palm, thumb }
    }
}

/// The thumb and finger bones of the left hand: the thumb, then index, middle, ring and little.
const THUMB_BONES: [HumanoidBone; 3] =
    [HumanoidBone::LeftThumbMetacarpal, HumanoidBone::LeftThumbProximal, HumanoidBone::LeftThumbDistal];
const FINGER_BONES: [[HumanoidBone; 3]; 4] = [
    [HumanoidBone::LeftIndexProximal, HumanoidBone::LeftIndexIntermediate, HumanoidBone::LeftIndexDistal],
    [HumanoidBone::LeftMiddleProximal, HumanoidBone::LeftMiddleIntermediate, HumanoidBone::LeftMiddleDistal],
    [HumanoidBone::LeftRingProximal, HumanoidBone::LeftRingIntermediate, HumanoidBone::LeftRingDistal],
    [HumanoidBone::LeftLittleProximal, HumanoidBone::LeftLittleIntermediate, HumanoidBone::LeftLittleDistal],
];

/// The 15 joints of the left hand at `wrist`, for a forearm from `elbow`: the table's hand with `HandParams` applied,
/// all scaled by `size` about the wrist. `finger_thickness` and `taper` shape the mesh only.
fn place_hand(t: &HandTable, h: &HandParams, size: f64, wrist: DVec3, elbow: DVec3) -> Vec<(HumanoidBone, DVec3)> {
    let param = |v: f64| if v.is_finite() { clamp(v, 0.5, 2.0) } else { 1.0 };
    let spread = if h.thumb_spread_deg.is_finite() { clamp(h.thumb_spread_deg, -30.0, 30.0) } else { 0.0 };
    let (palm_length, palm_width) = (param(h.palm_length), param(h.palm_width));
    let (finger_length, thumb_length) = (param(h.finger_length), param(h.thumb_length));
    let frame = HandFrame::of(wrist - elbow, t.palm_hint, t.thumb_hint);
    let world = |c: DVec3| wrist + (frame.axis * c.x + frame.thumb * c.y + frame.palm * c.z) * size;
    // A point of the palm, given from the hand's origin: the palm's length and width scale where it sits.
    let on_palm = |p: DVec3| DVec3::new((t.shift + p.x) * palm_length, p.y * palm_width, p.z);

    let mut joints = Vec::with_capacity(THUMB_BONES.len() + 3 * FINGER_BONES.len());
    for (bones, (&across, &length)) in FINGER_BONES.iter().zip(t.finger_across.iter().zip(&t.finger_length)) {
        let knuckle = on_palm(DVec3::new(t.knuckle, across, 0.0));
        for (bone, split) in bones.iter().zip(t.joint_split) {
            let along = length * finger_length * split;
            let curl = (t.curl)(t.knuckle + along);
            joints.push((*bone, world(DVec3::new(knuckle.x + along, knuckle.y, curl))));
        }
    }
    // The thumb opens in the plane of the palm: its joints turn about the metacarpal joint, from the fingers towards
    // the thumb side for a positive spread.
    let (base, root) = (on_palm(t.thumb.base), on_palm(t.thumb.root));
    let tip_ward = t.thumb.direction.normalize_or_zero() * (t.thumb.length * t.thumb.split * thumb_length);
    let (sin, cos) = spread.to_radians().sin_cos();
    let turn = |p: DVec3| {
        let v = p - base;
        base + DVec3::new(v.x * cos - v.y * sin, v.x * sin + v.y * cos, v.z)
    };
    for (bone, p) in THUMB_BONES.into_iter().zip([base, turn(root), turn(root + tip_ward)]) {
        joints.push((bone, world(p)));
    }
    joints
}

/// How the sliders and the body shape change each region's radii: a [`Gain`] at a point of a region, from the point's
/// template coordinate `u` and its fraction `v` along the template's axis.
struct Shaper<'a> {
    f: &'a Factors,
    shape: &'a Shape,
    marks: &'a Marks,
    zones: &'a Zones,
}

impl Shaper<'_> {
    fn torso(&self, y: f64, v: f64) -> Gain {
        // The width sliders act on their zones; the shape's gain (which carries its own `shoulder_hip`) multiplies.
        let sliders = (1.0 + (self.f.shoulder - 1.0) * self.zones.shoulder_weight(y))
            * (1.0 + (self.f.hip - 1.0) * (self.zones.hip)(y));
        self.shape.torso(v, self.marks) * Gain { outer: sliders, inner: sliders, front: 1.0, back: 1.0 }
    }
}

/// Builds the layout of `table` for these numbers: the loft of its curves ([`build_loft`]), with the leg
/// blended into the torso's outline where it leaves it (see [`junction`]), so the body's outline is C1 across the
/// hip.
///
/// # Errors
/// [`KitError`] if the table does not describe a valid layout: a curve with two knots at the same place, a region axis
/// with a zero-length segment, a non-finite number.
pub fn build(
    table: &BodyTable,
    proportions: &Proportions,
    shape: &BodyShape,
    hand: &HandParams,
) -> Result<BodyLayout, KitError> {
    build_stage(table, proportions, shape, hand, true)
}

/// The loft alone, before the blend at the hip: for soft-sd, the prototype's body exactly (what the
/// parity tests hold the port to).
///
/// # Errors
/// As [`build`].
pub fn build_loft(
    table: &BodyTable,
    proportions: &Proportions,
    shape: &BodyShape,
    hand: &HandParams,
) -> Result<BodyLayout, KitError> {
    build_stage(table, proportions, shape, hand, false)
}

fn build_stage(
    table: &BodyTable,
    proportions: &Proportions,
    shape: &BodyShape,
    hand: &HandParams,
    blend: bool,
) -> Result<BodyLayout, KitError> {
    let shape = Shape::new(shape);
    let native = Axes::of(&table.joints, &table.extents, 1.0, 0.0)?;
    let marks = marks(table, &native)?;
    let f = Factors::new(proportions, &shape, &marks, &ArmHang::of(table, &native)?);
    let j = place(&table.joints, &f);
    let placed = Axes::of(&j, &table.extents, f.foot, f.arm_tuck)?;
    let shaper = Shaper { f: &f, shape: &shape, marks: &marks, zones: &table.zones };

    let mut layout = BodyLayout::empty();
    for (bone, p) in [
        (HumanoidBone::Hips, j.hips),
        (HumanoidBone::Spine, j.spine),
        (HumanoidBone::Chest, j.chest),
        (HumanoidBone::UpperChest, j.upper_chest),
        (HumanoidBone::Neck, j.neck),
    ] {
        layout.put(bone, p);
    }
    for (bone, p) in [
        (HumanoidBone::LeftShoulder, j.shoulder),
        (HumanoidBone::LeftUpperArm, j.upper_arm),
        (HumanoidBone::LeftLowerArm, j.lower_arm),
        (HumanoidBone::LeftHand, j.hand),
        (HumanoidBone::LeftUpperLeg, j.upper_leg),
        (HumanoidBone::LeftLowerLeg, j.lower_leg),
        (HumanoidBone::LeftFoot, j.foot),
        (HumanoidBone::LeftToes, j.toes),
    ] {
        layout.put_pair(bone, p);
    }
    for (bone, p) in place_hand(&table.hand, hand, f.hand, j.hand, j.lower_arm) {
        layout.put_pair(bone, p);
    }
    layout.head_base = Some(j.head_base);
    layout.hand = Some(table.hand);

    let plain = Look { size: 1.0, across: 1.0, gain: None, inside: None, join: None };
    let torso_gain = |y: f64, v: f64| shaper.torso(y, v);
    let neck_gain = |_: f64, v: f64| shape.neck(v);
    let arm_gain = |_: f64, v: f64| shape.arm(v, &marks);
    let leg_gain = |_: f64, v: f64| shape.leg(v, &marks);
    let torso =
        region(&table.torso, &native.torso, placed.torso, DVec3::Z, &Look { gain: Some(&torso_gain), ..plain })?;
    let neck = region(&table.neck, &native.neck, placed.neck, DVec3::Z, &Look { gain: Some(&neck_gain), ..plain })?;
    let outline = blend.then(|| Outline::of(&torso));
    let arm = region(&table.arm, &native.arm, placed.arm, DVec3::Z, &Look { gain: Some(&arm_gain), ..plain })?;
    let leg_look = Look {
        across: f.leg_spacing,
        gain: Some(&leg_gain),
        inside: Some(&torso),
        join: outline.as_ref().map(|outline| Join { outline }),
        ..plain
    };
    let leg = region(&table.leg, &native.leg, placed.leg, DVec3::Z, &leg_look)?;
    let foot = region(&table.foot, &native.foot, placed.foot, DVec3::Y, &Look { size: f.foot, ..plain })?;
    layout.regions.insert(BodyRegion::Torso, torso);
    layout.regions.insert(BodyRegion::Neck, neck);
    for (left, right, r) in [
        (BodyRegion::ArmL, BodyRegion::ArmR, arm),
        (BodyRegion::LegL, BodyRegion::LegR, leg),
        (BodyRegion::FootL, BodyRegion::FootR, foot),
    ] {
        layout.regions.insert(right, r.mirrored());
        layout.regions.insert(left, r);
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::soft_sd::TABLE;

    #[test]
    fn an_unshaped_template_carries_no_gains_and_a_shaped_one_does() {
        let (p, h) = (Proportions::default(), HandParams::default());
        let plain = build_loft(&TABLE, &p, &BodyShape::default(), &h).unwrap();
        for region in [BodyRegion::Torso, BodyRegion::Neck, BodyRegion::ArmL, BodyRegion::LegL, BodyRegion::LegR] {
            assert!(plain.region(region).unwrap().gains.is_none(), "{region:?} is exactly the template's");
        }
        let tall = build(&TABLE, &p, &BodyShape { height: 1.0, ..BodyShape::default() }, &h).unwrap();
        assert!(tall.region(BodyRegion::Torso).unwrap().gains.is_none(), "height moves joints, not girth");
        let broad = build(&TABLE, &p, &BodyShape { build: 1.0, ..BodyShape::default() }, &h).unwrap();
        assert!(broad.region(BodyRegion::Torso).unwrap().gains.is_some());
    }

    #[test]
    fn a_swung_segment_keeps_its_length_reaches_further_out_and_swing_one_is_the_identity() {
        let d = DVec3::new(0.055, -0.09, 0.01);
        assert_eq!(swung(d, 1.0), d);
        assert_eq!(swung(DVec3::new(-0.05, -0.1, 0.0), 2.0), DVec3::new(-0.05, -0.1, 0.0), "only outward arms swing");
        let out = swung(d, 1.5);
        assert!((out.truncate().length() - d.truncate().length()).abs() < 1e-12 && (out.z - d.z).abs() < 1e-15);
        assert!((out.x / d.x - 1.5).abs() < 1e-12 && out.y < 0.0, "{out}");
        // However hard it is swung it never goes past the steepest the rule allows.
        let flat = swung(d, 100.0);
        assert!((flat.x / flat.truncate().length() - MAX_SWING_SIN).abs() < 1e-12);
    }

    fn elbow_path() -> Path {
        Path::new(vec![DVec3::ZERO, DVec3::new(0.06, -0.1, 0.0), DVec3::new(0.11, -0.2, 0.0)]).unwrap()
    }

    #[test]
    fn a_profile_is_a_monotone_curve_through_its_knots_and_rejects_bad_knots() {
        let p = Profile::new(&[(0.2, 3.0), (0.0, 1.0), (0.1, 2.0)]).unwrap();
        assert_eq!(p.knots(), [(0.0, 1.0), (0.1, 2.0), (0.2, 3.0)]);
        assert!((p.eval(0.1) - 2.0).abs() < 1e-12);
        assert_eq!((p.eval(-1.0), p.eval(9.0)), (1.0, 3.0), "the ends hold");
        assert!(matches!(Profile::new(&[(0.0, 1.0), (0.0, 2.0)]), Err(KitError::DuplicateKnot { .. })));
        assert!(matches!(Profile::new(&[]), Err(KitError::TooFew { .. })));
    }

    #[test]
    fn a_path_needs_two_distinct_finite_points() {
        assert!(matches!(Path::new(vec![DVec3::ZERO]), Err(KitError::TooFew { .. })));
        assert!(matches!(Path::new(vec![DVec3::ZERO, DVec3::ZERO]), Err(KitError::Degenerate { .. })));
        assert!(matches!(Path::new(vec![DVec3::ZERO, DVec3::NAN]), Err(KitError::NonFinite { .. })));
    }

    #[test]
    fn a_corner_turns_smoothly_inside_its_window_and_the_centre_stays_on_the_polyline() {
        let path = elbow_path();
        let corner = path.cum[1];
        let before = path.axis(corner - 2.0 * AXIS_BLEND);
        let after = path.axis(corner + 2.0 * AXIS_BLEND);
        assert!(before.dot(after) < 0.999, "outside the window the segments keep their own directions");
        let step = 1e-6;
        let jump = path.axis(corner - step).distance(path.axis(corner + step));
        assert!(jump < 1e-4, "the axis is continuous across the corner: {jump}");
        assert!((path.axis(corner).length() - 1.0).abs() < 1e-12);
        assert!(path.point(corner).distance(path.points[1]) < 1e-12);
    }

    #[test]
    fn the_warp_moves_corners_to_corners_and_extends_with_slope_one() {
        let warp = Warp { from: vec![0.0, 1.0, 3.0], to: vec![0.0, 2.0, 3.0] };
        assert_eq!([warp.apply(0.0), warp.apply(1.0), warp.apply(3.0)], [0.0, 2.0, 3.0]);
        assert!((warp.apply(0.5) - 1.0).abs() < 1e-12 && (warp.apply(2.0) - 2.5).abs() < 1e-12);
        assert!((warp.apply(-0.25) + 0.25).abs() < 1e-12 && (warp.apply(3.5) - 3.5).abs() < 1e-12);
    }

    #[test]
    fn the_section_frame_is_orthonormal_and_the_same_rule_serves_both_sides() {
        let axis = DVec3::new(0.4, -0.9, 0.1).normalize();
        let (across, through) = frame(axis, DVec3::Z);
        assert!(across.dot(axis).abs() < 1e-12 && through.dot(axis).abs() < 1e-12 && across.dot(through).abs() < 1e-12);
        let (m_across, m_through) = frame(DVec3::new(-axis.x, axis.y, axis.z), DVec3::Z);
        assert!(m_through.distance(DVec3::new(-through.x, through.y, through.z)) < 1e-12, "front stays front");
        assert!(m_across.distance(DVec3::new(across.x, -across.y, -across.z)) < 1e-12, "across stays along +X");
    }
}
