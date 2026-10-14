//! The standard template: an anime-standard figure, 6 heads tall, and the conventions the [`mini`](super::mini) and
//! [`tall`](super::tall) templates share with it.
//!
//! **A ratio table, in head units.** Every number below is a multiple of [`HEAD`], the head part's chin-to-crown height
//! (the project's one head frame), so a table reads as a figure-drawing proportion chart: heights are heads above the
//! sole, widths are heads from the centre line. [`h`] turns them into metres at compile time. The three templates differ
//! only in these tables; the builder, the sliders and the body shape act on all of them the same way.
//!
//! **Where the numbers come from.** Each class was fitted to a measured reference, never to a traced likeness (the
//! measurements and sources are in the PR; `tests/templates.rs` holds the bands):
//! - *The real adult* (tall): the US Army's ANSUR II survey, public domain, 4,082 men and 1,986 women. Fractions of the
//!   stature: shoulder tip 0.820, crotch 0.481, hip (trochanter) 0.515, knee (tibiale) 0.267, wrist 0.484, ankle 0.041,
//!   hand 0.110, foot 0.153. The classical canon (Vitruvius 8 heads, a real adult 7.5) agrees.
//! - *The anime model-sheet convention* (standard, and tall's widths): seven CC0 `VRoid` sample bodies of 6.5 to 7.9 heads,
//!   measured joint by joint. The shoulder joint sits 0.33 to 0.40 heads under the chin (not the canon's half head: the
//!   head is wider), the hip joint at 0.64 to 0.67, the knee at 0.38 to 0.40 and the ankle at 0.08 to 0.10 of the chin
//!   height, the arm (shoulder joint to wrist) 0.29 to 0.32 of it, the shoulder joints 0.87 to 0.93 heads apart on the
//!   female bodies and 1.2 on the male, the hip joints 0.62 to 0.72. Longer legs and shorter arms than the survey.
//! - *The chibi* (mini): the project's own 3 head concept art: crotch at a third of
//!   the height, shoulder joint a quarter of a head under the chin, knee at 0.31 of the chin height, short arms.
//!
//! The standard template is the middle one: a young adult's neutral build, shoulders about as wide as the hips and a
//! waist at four fifths of the chest. The body shape (CB-04) moves it towards a body type.

use omuzaki_char_api::{BodyParams, BodyShape, Cranium, HandParams, HeadParams, Neck, Proportions};
use omuzaki_core::glam::DVec3;

use super::layout::{
    self, BodyLayout, BodyTable, Extents, HandTable, Joints, ON_AXIS, RegionCurves, ThumbTable, Zones,
};
use super::soft_sd;
use super::{Template, TemplateDefaults};
use crate::kit::KitError;
use crate::kit::curve::{clamp, sstep};

/// One head in metres: the chin-to-crown height of the default cranium, the unit of every template's tables.
pub const HEAD: f64 = 0.383;

/// `heads` head heights in metres.
pub(super) const fn h(heads: f64) -> f64 {
    heads * HEAD
}

/// A point of a joint table: `x` across, `y` above the sole, `z` forward, all in heads.
pub(super) const fn at(x: f64, y: f64, z: f64) -> DVec3 {
    DVec3::new(h(x), h(y), h(z))
}

/// A knot of a section curve: `(coordinate, value)`, both in heads.
pub(super) const fn k(coordinate: f64, value: f64) -> (f64, f64) {
    (h(coordinate), h(value))
}

/// How far the fingers curl towards the palm at `u` past the hand's origin, for a hand whose middle fingertip is `reach`
/// from that origin: the soft-sd mitten's curve (`0.0075` over a `0.05` reach) at any size.
pub(super) fn curl(u: f64, reach: f64) -> f64 {
    0.15 * reach * clamp((u - 0.16 * reach) / (0.8 * reach), 0.0, 1.2).powi(2)
}

/// The soft-sd mitten's length, wrist to middle fingertip: what the thumb and the finger spacing scale from.
const MITTEN_LENGTH: f64 = 0.072;
/// Where a hand's origin lies past the wrist, as a fraction of its length (the mitten's).
const HAND_SHIFT: f64 = 0.30;

/// `v` scaled by `s` (glam's operators are not `const`).
const fn scaled(v: DVec3, s: f64) -> DVec3 {
    DVec3::new(v.x * s, v.y * s, v.z * s)
}

/// A hand `length` metres from the wrist to the middle fingertip, as soft-sd's mitten scaled up to `length`, with the
/// palm making `palm` of that length (the mitten's is `0.72`, a natural hand's `0.55`: longer fingers) and `curl` for
/// the fingers. The hand frame, the finger spacing and the thumb are the mitten's, so every hand opens the same way.
pub(super) const fn hand(length: f64, palm: f64, curl: fn(f64) -> f64) -> HandTable {
    let from = soft_sd::TABLE.hand;
    let s = length / MITTEN_LENGTH;
    let knuckle = palm * length - HAND_SHIFT * length;
    let finger = (1.0 - palm) * length;
    HandTable {
        shift: HAND_SHIFT * length,
        palm_hint: from.palm_hint,
        thumb_hint: from.thumb_hint,
        knuckle,
        finger_across: [
            from.finger_across[0] * s,
            from.finger_across[1] * s,
            from.finger_across[2] * s,
            from.finger_across[3] * s,
        ],
        // The mitten's fingers, relative to its middle finger: index, middle, ring, little.
        finger_length: [finger * 0.95, finger, finger * 0.925, finger * 0.775],
        joint_split: from.joint_split,
        curl,
        thumb: ThumbTable {
            base: scaled(from.thumb.base, s),
            root: scaled(from.thumb.root, s),
            direction: from.thumb.direction,
            length: from.thumb.length * s,
            split: from.thumb.split,
        },
    }
}

/// The hip zone of a torso: `1` up to `from`, falling to `0` at `to` (the prototype's `weightHips`).
pub(super) fn hip_zone(y: f64, from: f64, to: f64) -> f64 {
    1.0 - sstep(from, to, y)
}

/// The numbers a new character of a template starts from: the template's layout is the default sliders, so what changes
/// is the head, which rides on the head base, and the neck, which has to meet the body's.
pub(super) fn defaults(head_base_y: f64, neck: Neck) -> TemplateDefaults {
    // The default cranium sits `lift` above soft-sd's head base; every template puts it that far above its own.
    let lift = Cranium::default().centre[1] - soft_sd::TABLE.joints.head_base.y;
    let mut cranium = Cranium::default();
    cranium.centre[1] = head_base_y + lift;
    TemplateDefaults {
        head: HeadParams { cranium, neck, ..HeadParams::default() },
        body: BodyParams::default(),
        ..TemplateDefaults::default()
    }
}

/// Total height over head height: crown to sole.
const HEADS_TALL: f64 = 6.0;

/// The x of the leg's centre line.
const LEG_X: f64 = 0.31;

/// Length of the arm's axis (shoulder point, elbow, wrist): where the arm curves end.
const ARM_END: f64 = 1.6875;
/// Where the elbow lies along it.
const ARM_ELBOW: f64 = 0.9451;

/// The hip zone of the torso: full below the hip joint, gone a third of a head above the hips.
fn weight_hips(y: f64) -> f64 {
    hip_zone(y, h(3.30), h(3.65))
}

/// The fingers' curl, for a hand 0.6 heads long.
fn finger_curl(u: f64) -> f64 {
    curl(u, 0.7 * h(0.60))
}

/// The left joints in the A-pose: arms hang at 30 and 26 degrees from the body, legs straight down with the ankles a
/// little closer than the hips.
const JOINTS: Joints = Joints {
    hips: at(0.0, 3.45, 0.0),
    spine: at(0.0, 3.81, 0.0),
    chest: at(0.0, 4.26, 0.0),
    upper_chest: at(0.0, 4.53, 0.0),
    neck: at(0.0, 4.80, 0.0),
    head_base: at(0.0, 4.94, 0.0),
    shoulder: at(0.20, 4.71, 0.0),
    upper_arm: at(0.54, 4.62, 0.0),
    lower_arm: at(0.99, 3.8406, -0.03),
    hand: at(1.3144, 3.1755, 0.03),
    upper_leg: at(LEG_X, 3.25, 0.0),
    lower_leg: at(LEG_X - 0.04, 1.90, 0.02),
    foot: at(LEG_X - 0.08, 0.36, 0.0),
    toes: at(LEG_X - 0.08, 0.05, 0.42),
};

/// Where the sections start and end beyond the bones.
const EXTENTS: Extents = Extents {
    torso_bottom: at(0.0, -0.47, 0.0), // the crotch, 2.98
    torso_top: at(0.0, 0.06, 0.0),     // 4.86
    neck_top: at(0.0, 0.10, 0.0),      // inside the chin (5.00)
    arm_start: at(-0.02, 0.04, 0.0),
    leg_top: at(0.0, 0.12, 0.0),
    leg_bottom: at(0.0, -0.05, 0.0),
    heel: h(-0.21),
    toe_tip: h(0.57),
};

/// The torso, from the crotch to the base of the neck, against height.
const TORSO: RegionCurves = RegionCurves {
    origin: h(2.98),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(2.98, 0.000), k(3.45, 0.000), k(4.25, 0.020), k(4.62, 0.000), k(4.86, -0.030)]),
    rx: &[
        k(2.98, 0.530),
        k(3.04, 0.536),
        k(3.10, 0.545),
        k(3.20, 0.555),
        k(3.40, 0.520),
        k(3.60, 0.465),
        k(3.80, 0.435),
        k(3.95, 0.430),
        k(4.15, 0.445),
        k(4.35, 0.475),
        k(4.50, 0.515),
        k(4.62, 0.525),
        k(4.72, 0.500),
        k(4.79, 0.430),
        k(4.83, 0.330),
        k(4.86, 0.215),
    ],
    ry: &[
        k(2.98, 0.320),
        k(3.10, 0.350),
        k(3.20, 0.360),
        k(3.40, 0.350),
        k(3.60, 0.320),
        k(3.80, 0.290),
        k(3.95, 0.280),
        k(4.15, 0.290),
        k(4.35, 0.315),
        k(4.50, 0.325),
        k(4.62, 0.310),
        k(4.72, 0.290),
        k(4.79, 0.260),
        k(4.83, 0.230),
        k(4.86, 0.200),
    ],
};

/// The neck: as wide as the torso ends, narrowing to the chin.
const NECK: RegionCurves = RegionCurves {
    origin: h(4.86),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(4.86, -0.030), k(5.04, -0.070)]),
    rx: &[k(4.86, 0.215), k(4.95, 0.168), k(5.04, 0.150)],
    ry: &[k(4.86, 0.200), k(4.95, 0.172), k(5.04, 0.155)],
};

/// The bare arm, against arc length from the shoulder point.
const ARM: RegionCurves = RegionCurves {
    origin: 0.0,
    direction: 1.0,
    across: ON_AXIS,
    through: Some(ON_AXIS),
    rx: &[
        k(0.00, 0.160),
        k(0.48, 0.170),
        k(0.84, 0.150),
        k(ARM_ELBOW, 0.132),
        k(1.15, 0.130),
        k(1.38, 0.105),
        k(1.62, 0.082),
        k(ARM_END, 0.075),
    ],
    ry: &[
        k(0.00, 0.150),
        k(0.48, 0.160),
        k(0.84, 0.142),
        k(ARM_ELBOW, 0.124),
        k(1.15, 0.120),
        k(1.38, 0.098),
        k(1.62, 0.078),
        k(ARM_END, 0.071),
    ],
};

/// The leg, against height, running down from above the hip joint; the centre's x is relative to the joint line.
const LEG: RegionCurves = RegionCurves {
    origin: h(3.37),
    direction: -1.0,
    across: &[k(3.37, -0.03), k(2.90, -0.01), k(2.40, 0.0), k(0.31, 0.0)],
    through: Some(&[k(3.37, 0.0), k(2.80, 0.02), k(2.05, 0.0), k(1.30, -0.03), k(0.65, -0.015), k(0.31, 0.0)]),
    rx: &[
        k(3.37, 0.150),
        k(3.20, 0.205),
        k(3.05, 0.245),
        k(2.80, 0.250),
        k(2.40, 0.210),
        k(2.15, 0.168),
        k(1.90, 0.146),
        k(1.65, 0.150),
        k(1.30, 0.168),
        k(0.90, 0.135),
        k(0.62, 0.105),
        k(0.42, 0.088),
        k(0.31, 0.085),
    ],
    ry: &[
        k(3.37, 0.170),
        k(3.20, 0.230),
        k(3.05, 0.270),
        k(2.80, 0.280),
        k(2.40, 0.245),
        k(2.15, 0.182),
        k(1.90, 0.155),
        k(1.65, 0.165),
        k(1.30, 0.190),
        k(0.90, 0.145),
        k(0.62, 0.110),
        k(0.42, 0.092),
        k(0.31, 0.090),
    ],
};

/// The foot, against `z` from the ankle, heel to toe: 0.78 heads long, the sole on the floor.
const FOOT: RegionCurves = RegionCurves {
    origin: h(-0.21),
    direction: 1.0,
    across: ON_AXIS,
    through: None,
    rx: &[
        k(-0.21, 0.110),
        k(-0.10, 0.140),
        k(0.08, 0.150),
        k(0.30, 0.165),
        k(0.44, 0.155),
        k(0.54, 0.095),
        k(0.57, 0.030),
    ],
    ry: &[
        k(-0.21, 0.140),
        k(-0.10, 0.180),
        k(0.08, 0.175),
        k(0.30, 0.110),
        k(0.44, 0.085),
        k(0.54, 0.058),
        k(0.57, 0.026),
    ],
};

/// The standard body table.
pub const TABLE: BodyTable = BodyTable {
    joints: JOINTS,
    extents: EXTENTS,
    torso: TORSO,
    neck: NECK,
    arm: ARM,
    leg: LEG,
    foot: FOOT,
    zones: Zones { hip: weight_hips, shoulder: [h(4.12), h(4.42), h(4.58), h(4.74)], waist: h(3.90), bust: h(4.40) },
    hand: hand(h(0.60), 0.55, finger_curl),
};

/// The standard template (CB-01): an anime-standard figure, 6 heads tall.
#[derive(Debug)]
pub struct Standard;

impl Template for Standard {
    fn id(&self) -> &'static str {
        "standard"
    }

    fn summary(&self) -> &'static str {
        "An anime-standard figure 6 heads tall: a young adult's build, long legs, natural hands."
    }

    fn heads_tall(&self) -> f64 {
        HEADS_TALL
    }

    /// Derived in `docs/research/2026-10-template-stature.md`.
    fn default_stature_m(&self) -> f64 {
        1.660
    }

    fn try_layout(
        &self,
        proportions: &Proportions,
        shape: &BodyShape,
        hand: &HandParams,
    ) -> Result<BodyLayout, KitError> {
        layout::build(&TABLE, proportions, shape, hand)
    }

    fn defaults(&self) -> TemplateDefaults {
        defaults(JOINTS.head_base.y, Neck { radius_top: h(0.15), radius_bottom: h(0.215), height: h(0.36), drop: 0.0 })
    }
}

crate::register_template!(Standard);
