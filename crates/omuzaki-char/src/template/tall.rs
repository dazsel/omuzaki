//! The tall template: a long-limbed figure 7.5 heads tall, a real adult's proportions in the anime convention.
//!
//! The same conventions as the [standard](super::standard) template (a ratio table in head units; see there for the
//! sources). The heights follow ANSUR II scaled to 7.5 heads (the arm hanging, the wrist is at the crotch and the elbow
//! at 0.63 of the height), pushed a little towards the anime convention: crotch at 0.50 of the height, the shoulder joint
//! 0.40 heads under the chin, the arm 0.35 of the chin height. Shoulders are wider than the hips (a neutral build with a
//! lean towards the broad), 1.2 heads between the shoulder joints as on the CC0 `VRoid` male bodies. The head stays the
//! project's one head, so the body is the long part.

use omuzaki_char_api::{BodyShape, HandParams, Neck, Proportions};

use super::layout::{self, BodyLayout, BodyTable, Extents, Joints, ON_AXIS, RegionCurves, Zones};
use super::standard::{at, curl, defaults, h, hand, hip_zone, k};
use super::{Template, TemplateDefaults};
use crate::kit::KitError;

/// Total height over head height: crown to sole.
const HEADS_TALL: f64 = 7.5;

/// The x of the leg's centre line.
const LEG_X: f64 = 0.31;

/// Length of the arm's axis (shoulder point, elbow, wrist): where the arm curves end.
const ARM_END: f64 = 2.3085;
/// Where the elbow lies along it.
const ARM_ELBOW: f64 = 1.2853;

/// The hip zone of the torso: full below the hip joint, gone half a head above it.
fn weight_hips(y: f64) -> f64 {
    hip_zone(y, h(4.17), h(4.55))
}

/// The fingers' curl, for a hand 0.74 heads long.
fn finger_curl(u: f64) -> f64 {
    curl(u, 0.7 * h(0.74))
}

/// The left joints in the A-pose: arms hang at 28 and 24 degrees from the body, legs straight down with the ankles a
/// little closer than the hips.
const JOINTS: Joints = Joints {
    hips: at(0.0, 4.28, 0.0),
    spine: at(0.0, 4.82, 0.0),
    chest: at(0.0, 5.49, 0.0),
    upper_chest: at(0.0, 5.89, 0.0),
    neck: at(0.0, 6.29, 0.0),
    head_base: at(0.0, 6.44, 0.0),
    shoulder: at(0.20, 6.22, 0.0),
    upper_arm: at(0.60, 6.10, 0.0),
    lower_arm: at(1.1821, 5.0051, -0.04),
    hand: at(1.597, 4.0733, 0.04),
    upper_leg: at(LEG_X, 4.12, 0.0),
    lower_leg: at(LEG_X - 0.04, 2.34, 0.02),
    foot: at(LEG_X - 0.08, 0.40, 0.0),
    toes: at(LEG_X - 0.08, 0.055, 0.53),
};

/// Where the sections start and end beyond the bones.
const EXTENTS: Extents = Extents {
    torso_bottom: at(0.0, -0.50, 0.0), // the crotch, 3.72
    torso_top: at(0.0, 0.06, 0.0),     // 6.35
    neck_top: at(0.0, 0.10, 0.0),      // inside the chin (6.50)
    arm_start: at(-0.02, 0.04, 0.0),
    leg_top: at(0.0, 0.12, 0.0),
    leg_bottom: at(0.0, -0.06, 0.0),
    heel: h(-0.26),
    toe_tip: h(0.72),
};

/// The torso, from the crotch to the base of the neck, against height.
const TORSO: RegionCurves = RegionCurves {
    origin: h(3.78),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(3.78, 0.000), k(4.28, 0.000), k(5.40, 0.020), k(6.00, 0.000), k(6.35, -0.030)]),
    rx: &[
        k(3.78, 0.522),
        k(3.83, 0.534),
        k(3.88, 0.550),
        k(3.98, 0.560),
        k(4.20, 0.515),
        k(4.45, 0.465),
        k(4.70, 0.430),
        k(4.95, 0.435),
        k(5.20, 0.460),
        k(5.45, 0.510),
        k(5.70, 0.560),
        k(5.98, 0.585),
        k(6.10, 0.580),
        k(6.18, 0.555),
        k(6.26, 0.470),
        k(6.31, 0.335),
        k(6.35, 0.220),
    ],
    ry: &[
        k(3.78, 0.310),
        k(3.88, 0.340),
        k(3.98, 0.355),
        k(4.20, 0.345),
        k(4.45, 0.310),
        k(4.70, 0.280),
        k(4.95, 0.280),
        k(5.20, 0.295),
        k(5.45, 0.320),
        k(5.70, 0.335),
        k(5.98, 0.320),
        k(6.10, 0.300),
        k(6.18, 0.280),
        k(6.26, 0.245),
        k(6.31, 0.210),
        k(6.35, 0.190),
    ],
};

/// The neck: as wide as the torso ends, narrowing to the chin.
const NECK: RegionCurves = RegionCurves {
    origin: h(6.35),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(6.35, -0.030), k(6.54, -0.070)]),
    rx: &[k(6.35, 0.220), k(6.44, 0.175), k(6.54, 0.160)],
    ry: &[k(6.35, 0.190), k(6.44, 0.172), k(6.54, 0.160)],
};

/// The bare arm, against arc length from the shoulder point.
const ARM: RegionCurves = RegionCurves {
    origin: 0.0,
    direction: 1.0,
    across: ON_AXIS,
    through: Some(ON_AXIS),
    rx: &[
        k(0.00, 0.170),
        k(0.58, 0.180),
        k(1.00, 0.158),
        k(ARM_ELBOW, 0.140),
        k(1.58, 0.136),
        k(1.86, 0.108),
        k(2.15, 0.088),
        k(ARM_END, 0.080),
    ],
    ry: &[
        k(0.00, 0.160),
        k(0.58, 0.170),
        k(1.00, 0.148),
        k(ARM_ELBOW, 0.132),
        k(1.58, 0.126),
        k(1.86, 0.100),
        k(2.15, 0.082),
        k(ARM_END, 0.075),
    ],
};

/// The leg, against height, running down from above the hip joint; the centre's x is relative to the joint line.
const LEG: RegionCurves = RegionCurves {
    origin: h(4.24),
    direction: -1.0,
    across: &[k(4.24, -0.03), k(3.57, -0.01), k(3.15, 0.0), k(0.34, 0.0)],
    through: Some(&[k(4.24, 0.0), k(3.57, 0.02), k(2.55, 0.0), k(1.65, -0.03), k(0.90, -0.015), k(0.34, 0.0)]),
    rx: &[
        k(4.24, 0.155),
        k(4.05, 0.210),
        k(3.87, 0.240),
        k(3.57, 0.250),
        k(3.15, 0.220),
        k(2.78, 0.178),
        k(2.52, 0.158),
        k(2.34, 0.150),
        k(2.05, 0.158),
        k(1.60, 0.175),
        k(1.10, 0.142),
        k(0.78, 0.108),
        k(0.52, 0.090),
        k(0.34, 0.086),
    ],
    ry: &[
        k(4.24, 0.175),
        k(4.05, 0.235),
        k(3.87, 0.272),
        k(3.57, 0.290),
        k(3.15, 0.255),
        k(2.78, 0.192),
        k(2.52, 0.168),
        k(2.34, 0.160),
        k(2.05, 0.172),
        k(1.60, 0.198),
        k(1.10, 0.152),
        k(0.78, 0.116),
        k(0.52, 0.096),
        k(0.34, 0.092),
    ],
};

/// The foot, against `z` from the ankle, heel to toe: 0.98 heads long, the sole on the floor.
const FOOT: RegionCurves = RegionCurves {
    origin: h(-0.26),
    direction: 1.0,
    across: ON_AXIS,
    through: None,
    rx: &[
        k(-0.26, 0.120),
        k(-0.12, 0.150),
        k(0.10, 0.165),
        k(0.38, 0.180),
        k(0.55, 0.168),
        k(0.67, 0.102),
        k(0.72, 0.032),
    ],
    ry: &[
        k(-0.26, 0.150),
        k(-0.12, 0.190),
        k(0.10, 0.185),
        k(0.38, 0.120),
        k(0.55, 0.090),
        k(0.67, 0.060),
        k(0.72, 0.027),
    ],
};

/// The tall body table.
pub const TABLE: BodyTable = BodyTable {
    joints: JOINTS,
    extents: EXTENTS,
    torso: TORSO,
    neck: NECK,
    arm: ARM,
    leg: LEG,
    foot: FOOT,
    zones: Zones { hip: weight_hips, shoulder: [h(5.35), h(5.70), h(5.98), h(6.20)], waist: h(4.70), bust: h(5.65) },
    hand: hand(h(0.74), 0.55, finger_curl),
};

/// The tall template (CB-01): a long-limbed figure 7.5 heads tall.
#[derive(Debug)]
pub struct Tall;

impl Template for Tall {
    fn id(&self) -> &'static str {
        "tall"
    }

    fn summary(&self) -> &'static str {
        "A tall figure 7.5 heads tall: long legs and arms, a long neck, natural hands."
    }

    fn heads_tall(&self) -> f64 {
        HEADS_TALL
    }

    /// Derived in `docs/research/2026-10-template-stature.md`.
    fn default_stature_m(&self) -> f64 {
        1.714
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
        defaults(JOINTS.head_base.y, Neck { radius_top: h(0.158), radius_bottom: h(0.22), height: h(0.38), drop: 0.0 })
    }
}

crate::register_template!(Tall);
