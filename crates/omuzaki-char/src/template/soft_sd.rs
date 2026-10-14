//! The soft-sd template: the default chibi body, about 2.5 heads tall, in the head frame's metres.
//!
//! Its numbers are the project's own prototype skeleton and section curves (`skeleton.js` `J`, `uniform-dims.js`
//! `TORSO`, `LEG`, `ARM` and `weightHips`, `body.js` `handFrame`): generic chibi proportions that the product ships as
//! its default body. Comments give the prototype's names. Nothing of a particular character is here: the outfit curves
//! (`SKIRT`, `BOOT`) are garment parameters, and the face, hair and outfit of any character live in its own project.
//!
//! What the prototype did not need is new and generic: the VRM bones it lacked (`upperChest`, shoulders, fingers), the
//! bare arm and neck radii (a sleeve and a collar covered them) and the foot curves.

use omuzaki_char_api::{BodyShape, HandParams, Proportions};
use omuzaki_core::glam::DVec3;

use super::layout::{
    self, BodyLayout, BodyTable, Extents, HandTable, Joints, ON_AXIS, RegionCurves, ThumbTable, Zones,
};
use super::{Template, TemplateDefaults};
use crate::kit::KitError;
use crate::kit::curve::{clamp, sstep};

/// Total height over head height: crown to sole is 0.966 m for the default cranium, whose chin-to-crown height is
/// 0.383 m, or 2.52 heads; the template declares the round number.
const HEADS_TALL: f64 = 2.5;

/// The x of the leg's centre line (`LEG_X`).
const LEG_X: f64 = 0.045;

/// The hip zone of the torso (`weightHips`): `1` below `y = 0.385`, falling to `0` at `0.425`.
pub fn weight_hips(y: f64) -> f64 {
    1.0 - sstep(0.385, 0.425, y)
}

/// How far the fingers curl towards the palm at `u` past the hand's origin (the mitt's `curl`).
fn curl(u: f64) -> f64 {
    0.0075 * clamp((u - 0.008) / 0.040, 0.0, 1.2).powi(2)
}

/// The left joints in the A-pose, in metres.
const JOINTS: Joints = Joints {
    hips: DVec3::new(0.0, 0.37, 0.0),          // 'hips'
    spine: DVec3::new(0.0, 0.41, 0.0),         // 'spine'
    chest: DVec3::new(0.0, 0.46, 0.0),         // 'chest'
    upper_chest: DVec3::new(0.0, 0.49, 0.0),   // new: halfway from the chest to the neck
    neck: DVec3::new(0.0, 0.52, 0.0),          // 'neck'
    head_base: DVec3::new(0.0, 0.56, 0.0),     // 'head'
    shoulder: DVec3::new(0.035, 0.485, 0.0),   // new: the collar bone's root, a third of the way to the arm
    upper_arm: DVec3::new(0.1, 0.47, 0.0),     // 'upper_arm.L'
    lower_arm: DVec3::new(0.155, 0.38, -0.01), // 'lower_arm.L' (ARM.elbow)
    hand: DVec3::new(0.2, 0.29, 0.01),         // 'hand.L' (ARM.wrist)
    upper_leg: DVec3::new(LEG_X, 0.38, 0.0),   // 'upper_leg.L'
    lower_leg: DVec3::new(LEG_X, 0.19, 0.0),   // 'lower_leg.L'
    foot: DVec3::new(LEG_X, 0.06, 0.0),        // 'foot.L'
    toes: DVec3::new(LEG_X, 0.012, 0.07),      // 'toes.L'
};

/// Where the sections start and end beyond the bones.
const EXTENTS: Extents = Extents {
    torso_bottom: DVec3::new(0.0, -0.045, 0.0), // TORSO starts at y = 0.325
    torso_top: DVec3::new(0.0, 0.025, 0.0),     // TORSO ends at y = 0.545
    neck_top: DVec3::new(0.0, 0.04, 0.0),       // new: inside the chin (the default cranium's chin is at y = 0.583)
    arm_start: DVec3::new(-0.008, 0.014, 0.0),  // ARM.shoulder = [0.092, 0.484, 0] against 'upper_arm.L'
    leg_top: DVec3::new(0.0, 0.04, 0.0),        // LEG starts at y = 0.42
    leg_bottom: DVec3::new(0.0, -0.02, 0.0),    // LEG ends at y = 0.04
    heel: -0.035,
    toe_tip: 0.095,
};

/// TORSO (the shirt's body curve, `uniform-dims.js`), against height `y`.
const TORSO: RegionCurves = RegionCurves {
    origin: 0.325,
    direction: 1.0,
    across: ON_AXIS,
    // TORSO.cz
    through: Some(&[(0.325, 0.0), (0.40, 0.001), (0.46, 0.002), (0.50, -0.005), (0.52, -0.008), (0.545, -0.012)]),
    // TORSO.rx
    rx: &[
        (0.325, 0.0890),
        (0.345, 0.0890),
        (0.362, 0.0868),
        (0.376, 0.0815),
        (0.39, 0.0770),
        (0.405, 0.0745),
        (0.43, 0.0800),
        (0.46, 0.0875),
        (0.485, 0.0885),
        (0.50, 0.0850),
        (0.51, 0.0770),
        (0.52, 0.0640),
        (0.53, 0.0500),
        (0.545, 0.0425),
    ],
    // TORSO.rz
    ry: &[
        (0.325, 0.0795),
        (0.34, 0.0765),
        (0.365, 0.0695),
        (0.385, 0.0640),
        (0.405, 0.0605),
        (0.43, 0.0640),
        (0.46, 0.0680),
        (0.485, 0.0660),
        (0.50, 0.0630),
        (0.51, 0.0590),
        (0.52, 0.0540),
        (0.53, 0.0480),
        (0.545, 0.0425),
    ],
};

/// The neck, new: it starts as wide as the torso ends (the sections meet) and narrows to the chin.
const NECK: RegionCurves = RegionCurves {
    origin: 0.545,
    direction: 1.0,
    across: ON_AXIS,
    through: Some(&[(0.545, -0.012), (0.60, -0.016)]),
    rx: &[(0.545, 0.0425), (0.575, 0.0350), (0.60, 0.0330)],
    ry: &[(0.545, 0.0425), (0.575, 0.0345), (0.60, 0.0325)],
};

/// The bare arm, new, against arc length from the shoulder point: a little inside the sleeve (`uniform.js` `buildSleeve`
/// is 0.0295 at the shoulder, 0.0262 at the elbow and 0.0182 at the cuff) and as wide as the mitt at the wrist.
const ARM: RegionCurves = RegionCurves {
    origin: 0.0,
    direction: 1.0,
    across: ON_AXIS,
    through: Some(ON_AXIS),
    rx: &[(0.0, 0.0215), (0.07, 0.0225), (0.122, 0.0190), (0.182, 0.0155), (0.222, 0.0128), (0.2246, 0.0125)],
    ry: &[(0.0, 0.0205), (0.07, 0.0215), (0.122, 0.0182), (0.182, 0.0148), (0.222, 0.0120), (0.2246, 0.0118)],
};

/// LEG (the tights' leg curve), against height `y`, running down from the hip. The centre's x is relative to `LEG_X`.
const LEG: RegionCurves = RegionCurves {
    origin: 0.42,
    direction: -1.0,
    // LEG.cx
    across: &[
        (0.04, 0.046 - LEG_X),
        (0.19, 0.0455 - LEG_X),
        (0.30, 0.0440 - LEG_X),
        (0.36, 0.0415 - LEG_X),
        (0.42, 0.039 - LEG_X),
    ],
    // LEG.cz
    through: Some(&[
        (0.04, 0.004),
        (0.07, 0.003),
        (0.11, -0.003),
        (0.15, -0.002),
        (0.19, 0.002),
        (0.23, 0.001),
        (0.30, 0.0),
        (0.42, 0.0),
    ]),
    // LEG.rx
    rx: &[
        (0.04, 0.026),
        (0.07, 0.0265),
        (0.11, 0.0325),
        (0.15, 0.0340),
        (0.19, 0.0345),
        (0.205, 0.0355),
        (0.23, 0.0385),
        (0.26, 0.0415),
        (0.30, 0.0425),
        (0.34, 0.0400),
        (0.38, 0.0370),
        (0.405, 0.0345),
    ],
    // LEG.rz
    ry: &[
        (0.04, 0.028),
        (0.07, 0.0285),
        (0.11, 0.0340),
        (0.15, 0.0358),
        (0.19, 0.0362),
        (0.205, 0.0380),
        (0.23, 0.0425),
        (0.26, 0.0470),
        (0.30, 0.0490),
        (0.34, 0.0470),
        (0.38, 0.0450),
        (0.405, 0.0430),
    ],
};

/// The foot, new: against `z` from the ankle, heel to toe. `rx` is half the width, `ry` half the height; the sole is on
/// the floor. About a third of a head long, with the ankle at the height of the `foot` joint.
const FOOT: RegionCurves = RegionCurves {
    origin: -0.035,
    direction: 1.0,
    across: ON_AXIS,
    through: None,
    rx: &[
        (-0.035, 0.0210),
        (-0.015, 0.0255),
        (0.015, 0.0265),
        (0.050, 0.0290),
        (0.070, 0.0270),
        (0.090, 0.0160),
        (0.095, 0.0050),
    ],
    ry: &[
        (-0.035, 0.0260),
        (-0.015, 0.0335),
        (0.015, 0.0310),
        (0.050, 0.0190),
        (0.070, 0.0140),
        (0.090, 0.0095),
        (0.095, 0.0040),
    ],
};

/// The hand, in the frame of `handFrame` (`body.js`). The mitt's origin, the thumb's root, direction and length and the
/// finger separation lines (at `+-0.0085` and `0`, so fingers at `+-0.00425` and `+-0.01275`) are the prototype's; the
/// knuckle row sits at the lines' start (`u = 0.030`), and the finger lengths end the middle finger at the mitt's tip
/// (`HAND_L = 0.050`). The metacarpal joint and the joint splits are new.
const HAND: HandTable = HandTable {
    shift: 0.022, // HAND_SHIFT
    palm_hint: DVec3::new(-1.0, 0.0, 0.0),
    thumb_hint: DVec3::new(0.0, 0.0, 1.0),
    knuckle: 0.030,
    finger_across: [0.01275, 0.00425, -0.00425, -0.01275],
    finger_length: [0.019, 0.020, 0.0185, 0.0155],
    joint_split: [0.0, 0.45, 0.75],
    curl,
    thumb: ThumbTable {
        base: DVec3::new(-0.010, 0.0100, 0.0030),
        root: DVec3::new(0.015, 0.0115, 0.0005), // handThumb: root
        direction: DVec3::new(0.74, 0.46, 0.26), // handThumb: dir (A, T, N)
        length: 0.0265,                          // handThumb: len
        split: 0.55,
    },
};

/// The soft-sd body table.
pub const TABLE: BodyTable = BodyTable {
    joints: JOINTS,
    extents: EXTENTS,
    torso: TORSO,
    neck: NECK,
    arm: ARM,
    leg: LEG,
    foot: FOOT,
    zones: Zones {
        hip: weight_hips,
        shoulder: [0.43, 0.47, 0.50, 0.53],
        waist: 0.405, // where TORSO is narrowest
        bust: 0.47,
    },
    hand: HAND,
};

/// The soft-sd template (CB-01): a super-deformed body, short and soft, under a large head.
#[derive(Debug)]
pub struct SoftSd;

impl Template for SoftSd {
    fn id(&self) -> &'static str {
        "soft-sd"
    }

    fn summary(&self) -> &'static str {
        "A super-deformed chibi body about 2.5 heads tall: short limbs, a small torso, soft mitten hands."
    }

    fn heads_tall(&self) -> f64 {
        HEADS_TALL
    }

    /// Its authored height, so the scale is exactly 1: soft-sd is the prototype port, with geometry parity and springs,
    /// hair and cloth tuned by eye at this size (a project decision, `docs/research/2026-10-template-stature.md`).
    fn default_stature_m(&self) -> f64 {
        self.authored_height_m()
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
        TemplateDefaults::default()
    }
}

crate::register_template!(SoftSd);
