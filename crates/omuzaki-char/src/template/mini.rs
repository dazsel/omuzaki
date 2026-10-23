//! The mini template: a chibi figure 3 heads tall, one head more than [`soft_sd`](super::soft_sd).
//!
//! The same conventions as the [standard](super::standard) template (a ratio table in head units; see there for the
//! sources), with the numbers of the project's own 3 head concept art: the crotch at a third of the height, the shoulder
//! joint a quarter of a head under the chin, short arms hanging a little further out from the body (34 degrees), a small
//! torso on short, soft limbs and feet half a head long. Mitten hands, as in soft-sd.

use omuzaki_char_api::{BodyShape, HandParams, Neck, Proportions};

use super::layout::{self, BodyLayout, BodyTable, Extents, Joints, ON_AXIS, RegionCurves, Zones};
use super::standard::{at, curl, defaults, h, hand, hip_zone, k};
use super::{Template, TemplateDefaults};
use crate::kit::KitError;

/// Total height over head height: crown to sole.
const HEADS_TALL: f64 = 3.0;

/// The x of the leg's centre line.
const LEG_X: f64 = 0.165;

/// Length of the arm's axis (shoulder point, elbow, wrist): where the arm curves end.
const ARM_END: f64 = 0.6463;
/// Where the elbow lies along it.
const ARM_ELBOW: f64 = 0.3647;

/// The hip zone of the torso: full below the hip joint, gone a third of a head above it.
fn weight_hips(y: f64) -> f64 {
    hip_zone(y, h(1.12), h(1.24))
}

/// The fingers' curl, for a mitten 0.3 heads long.
fn finger_curl(u: f64) -> f64 {
    curl(u, 0.7 * h(0.30))
}

/// The left joints in the A-pose: arms hang at 34 and 30 degrees from the body, legs straight down.
const JOINTS: Joints = Joints {
    hips: at(0.0, 1.16, 0.0),
    spine: at(0.0, 1.34, 0.0),
    chest: at(0.0, 1.57, 0.0),
    upper_chest: at(0.0, 1.71, 0.0),
    neck: at(0.0, 1.85, 0.0),
    head_base: at(0.0, 1.94, 0.0),
    shoulder: at(0.10, 1.79, 0.0),
    upper_arm: at(0.29, 1.74, 0.0),
    lower_arm: at(0.4689, 1.4747, -0.015),
    hand: at(0.6089, 1.2322, 0.015),
    upper_leg: at(LEG_X, 1.10, 0.0),
    lower_leg: at(LEG_X - 0.005, 0.62, 0.01),
    foot: at(LEG_X - 0.01, 0.22, 0.0),
    toes: at(LEG_X - 0.01, 0.03, 0.30),
};

/// Where the sections start and end beyond the bones.
const EXTENTS: Extents = Extents {
    torso_bottom: at(0.0, -0.19, 0.0), // the crotch, 0.97
    torso_top: at(0.0, 0.065, 0.0),    // 1.915
    neck_top: at(0.0, 0.10, 0.0),      // inside the chin (2.00)
    arm_start: at(-0.02, 0.04, 0.0),
    leg_top: at(0.0, 0.07, 0.0),
    leg_bottom: at(0.0, -0.03, 0.0),
    heel: h(-0.13),
    toe_tip: h(0.37),
};

/// The torso, from the crotch to the base of the neck, against height.
const TORSO: RegionCurves = RegionCurves {
    origin: h(0.97),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(0.97, 0.000), k(1.34, 0.003), k(1.66, 0.005), k(1.92, -0.030)]),
    rx: &[
        k(0.97, 0.255),
        k(1.03, 0.268),
        k(1.10, 0.278),
        k(1.18, 0.268),
        k(1.26, 0.245),
        k(1.34, 0.238),
        k(1.44, 0.252),
        k(1.56, 0.275),
        k(1.66, 0.285),
        k(1.74, 0.275),
        k(1.80, 0.240),
        k(1.86, 0.180),
        k(1.92, 0.115),
    ],
    ry: &[
        k(0.97, 0.220),
        k(1.03, 0.240),
        k(1.10, 0.250),
        k(1.18, 0.235),
        k(1.26, 0.210),
        k(1.34, 0.200),
        k(1.44, 0.210),
        k(1.56, 0.225),
        k(1.66, 0.220),
        k(1.74, 0.210),
        k(1.80, 0.190),
        k(1.86, 0.150),
        k(1.92, 0.115),
    ],
};

/// The neck: short, as wide as the torso ends, narrowing to the chin.
const NECK: RegionCurves = RegionCurves {
    origin: h(1.915),
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[k(1.92, -0.030), k(2.04, -0.045)]),
    rx: &[k(1.92, 0.115), k(1.98, 0.098), k(2.04, 0.092)],
    ry: &[k(1.92, 0.115), k(1.98, 0.100), k(2.04, 0.092)],
};

/// The bare arm, against arc length from the shoulder point.
const ARM: RegionCurves = RegionCurves {
    origin: 0.0,
    direction: 1.0,
    across: ON_AXIS,
    through: Some(ON_AXIS),
    rx: &[k(0.00, 0.070), k(0.20, 0.074), k(ARM_ELBOW, 0.060), k(0.50, 0.052), k(0.62, 0.043), k(ARM_END, 0.041)],
    ry: &[k(0.00, 0.066), k(0.20, 0.070), k(ARM_ELBOW, 0.058), k(0.50, 0.048), k(0.64, 0.040), k(ARM_END, 0.039)],
};

/// The leg, against height, running down from above the hip joint; the centre's x is relative to the joint line.
const LEG: RegionCurves = RegionCurves {
    origin: h(1.17),
    direction: -1.0,
    across: &[k(1.17, -0.020), k(1.00, -0.008), k(0.62, 0.000), k(0.19, 0.000)],
    through: Some(&[k(1.17, 0.000), k(0.95, 0.008), k(0.62, 0.000), k(0.40, -0.012), k(0.19, 0.000)]),
    rx: &[
        k(1.17, 0.060),
        k(1.10, 0.090),
        k(1.00, 0.108),
        k(0.85, 0.106),
        k(0.72, 0.095),
        k(0.62, 0.088),
        k(0.50, 0.094),
        k(0.40, 0.098),
        k(0.30, 0.075),
        k(0.22, 0.062),
        k(0.19, 0.060),
    ],
    ry: &[
        k(1.17, 0.070),
        k(1.10, 0.100),
        k(1.00, 0.125),
        k(0.85, 0.125),
        k(0.72, 0.110),
        k(0.62, 0.100),
        k(0.50, 0.105),
        k(0.40, 0.112),
        k(0.30, 0.088),
        k(0.22, 0.070),
        k(0.19, 0.068),
    ],
};

/// The foot, against `z` from the ankle, heel to toe: half a head long, the sole on the floor.
const FOOT: RegionCurves = RegionCurves {
    origin: h(-0.13),
    direction: 1.0,
    across: ON_AXIS,
    through: None,
    rx: &[
        k(-0.13, 0.060),
        k(-0.06, 0.074),
        k(0.06, 0.078),
        k(0.18, 0.085),
        k(0.26, 0.080),
        k(0.33, 0.048),
        k(0.37, 0.015),
    ],
    ry: &[
        k(-0.13, 0.080),
        k(-0.06, 0.105),
        k(0.06, 0.100),
        k(0.18, 0.062),
        k(0.26, 0.048),
        k(0.33, 0.033),
        k(0.37, 0.016),
    ],
};

/// The mini body table.
pub const TABLE: BodyTable = BodyTable {
    joints: JOINTS,
    extents: EXTENTS,
    torso: TORSO,
    neck: NECK,
    arm: ARM,
    leg: LEG,
    foot: FOOT,
    zones: Zones { hip: weight_hips, shoulder: [h(1.45), h(1.62), h(1.72), h(1.82)], waist: h(1.34), bust: h(1.60) },
    hand: hand(h(0.30), 0.72, finger_curl),
};

/// The mini template (CB-01): a chibi figure 3 heads tall.
#[derive(Debug)]
pub struct Mini;

impl Template for Mini {
    fn id(&self) -> &'static str {
        "mini"
    }

    fn summary(&self) -> &'static str {
        "A chibi figure 3 heads tall: a small torso, short soft limbs, mitten hands."
    }

    fn heads_tall(&self) -> f64 {
        HEADS_TALL
    }

    /// Soft-sd's head size times this template's head count: the SD templates share one stylised head (a project
    /// decision, `docs/research/2026-10-template-stature.md`).
    fn default_stature_m(&self) -> f64 {
        h(HEADS_TALL)
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
        defaults(Neck { radius_top: h(0.092), radius_bottom: h(0.115), height: h(0.25), drop: 0.016 })
    }
}

crate::register_template!(Mini);
