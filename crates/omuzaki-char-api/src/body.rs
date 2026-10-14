//! The body part's parameters (spec CB-01..04, RG-09): a template, proportion sliders, a body-shape space and hands.
//!
//! The *template* (`BodyTemplateId`, on the character) fixes the skeleton, the weights and the default sizes; the
//! proportions here scale those defaults, so fitting to a reference sheet (CB-03) edits only numbers near 1. The
//! [`BodyShape`] axes (CB-04) move the whole figure between body types, and [`HandParams`] shape the hands. Every
//! default leaves the template exactly as it is.

use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::head::SkinColours;
use crate::ids::BodyShapeId;

/// How hands are built (CB-02). Every template ships five-finger hands (finger bones and a hand-shape library, RG-09,
/// P0); the style picks what the *mesh* shows, so mitten stays a style, not a limit of the template.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HandStyle {
    /// Soft mittens with a separate thumb (the default; cheapest and cutest).
    #[default]
    Mitten,
    /// Simplified hands with fused fingers.
    Simple,
    /// Five separate fingers (RG-09): shaped by [`HandParams`].
    FiveFinger,
}

/// Size multipliers on the template's defaults. 1 leaves a part of the body as the template has it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Proportions {
    /// Neck length (short for chibi, longer for close-ups).
    pub neck_length: f64,
    /// Torso length.
    pub torso_length: f64,
    /// Shoulder width.
    pub shoulder_width: f64,
    /// Hip width.
    pub hip_width: f64,
    /// Arm length.
    pub arm_length: f64,
    /// Leg length.
    pub leg_length: f64,
    /// Hand size.
    pub hand_size: f64,
    /// Foot size.
    pub foot_size: f64,
}

impl Default for Proportions {
    fn default() -> Self {
        Self {
            neck_length: 1.0,
            torso_length: 1.0,
            shoulder_width: 1.0,
            hip_width: 1.0,
            arm_length: 1.0,
            leg_length: 1.0,
            hand_size: 1.0,
            foot_size: 1.0,
        }
    }
}

impl Validate for Proportions {
    fn check(&self, c: &mut Check) {
        for (name, v) in [
            ("neck_length", self.neck_length),
            ("torso_length", self.torso_length),
            ("shoulder_width", self.shoulder_width),
            ("hip_width", self.hip_width),
            ("arm_length", self.arm_length),
            ("leg_length", self.leg_length),
            ("hand_size", self.hand_size),
            ("foot_size", self.foot_size),
        ] {
            c.range(name, v, 0.5, 2.0);
        }
    }
}

/// The body-shape space (CB-04): five independent axes, each `-1..=1` with `0` the template as it is, so
/// `BodyShape::default()` is the identity. Axes combine, so a broad, muscular, shoulder-heavy figure and a slim,
/// curvy, hip-heavy one are both a handful of numbers, and presets are blends of them (`lerp`).
///
/// The axes move *shape*; [`Proportions`] stay the place for plain lengths and widths. The generator applies the
/// shape first and the proportions on top, so fitting a reference sheet (CB-03) still edits numbers near 1.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BodyShape {
    /// Body length, `-1` shorter to `+1` taller: at the extremes the body is 12% shorter or taller than the
    /// template. The head keeps its size (head height is the template's unit), so tall figures read as taller, not as
    /// bigger-headed.
    pub height: f64,
    /// Slim (`-1`) to broad (`+1`): the girth of torso, arms and legs together.
    pub build: f64,
    /// Waist-to-hip ratio and bust definition (`-1` straight, `+1` pronounced). Independent of `build`: a broad body
    /// can be straight or curvy.
    pub curves: f64,
    /// Muscle definition (`-1` soft, `+1` heavy) in the shoulders, arms, thighs, calves and neck.
    pub musculature: f64,
    /// Hip-heavy (`-1`) to shoulder-heavy (`+1`): where the width of the torso sits.
    pub shoulder_hip: f64,
    /// The preset the axes were last derived from; informational (the numbers above are what builds), like
    /// `HeadParams::preset`.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub preset: Option<BodyShapeId>,
}

impl BodyShape {
    /// The five axes by name, for validation and tools that list them.
    fn axes(&self) -> [(&'static str, f64); 5] {
        [
            ("height", self.height),
            ("build", self.build),
            ("curves", self.curves),
            ("musculature", self.musculature),
            ("shoulder_hip", self.shoulder_hip),
        ]
    }

    /// Linear blend: `t = 0` is `self`, `t = 1` is `other`. The result keeps a preset only where it is still true:
    /// an end of the blend keeps its own, and two shapes from the same preset keep it; anything in between has none.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        let m = |a: f64, b: f64| a + (b - a) * t;
        let preset = if t <= 0.0 {
            self.preset.clone()
        } else if t >= 1.0 || self.preset == other.preset {
            other.preset.clone()
        } else {
            None
        };
        Self {
            height: m(self.height, other.height),
            build: m(self.build, other.build),
            curves: m(self.curves, other.curves),
            musculature: m(self.musculature, other.musculature),
            shoulder_hip: m(self.shoulder_hip, other.shoulder_hip),
            preset,
        }
    }
}

impl Validate for BodyShape {
    fn check(&self, c: &mut Check) {
        for (name, v) in self.axes() {
            c.range(name, v, -1.0, 1.0);
        }
    }
}

/// The shape of the hands (RG-09), for `HandStyle::FiveFinger` hands; `HandStyle::Mitten` and `Simple` use the palm
/// and thumb numbers and ignore the finger ones. Every field is relative to the template's hand, so the defaults leave
/// it unchanged: the multipliers are `1` at the template (`0.5..=2`), and the thumb spread is an offset in degrees.
/// Overall size is `Proportions::hand_size`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct HandParams {
    /// Palm width (across the knuckles).
    pub palm_width: f64,
    /// Palm length (wrist to knuckles).
    pub palm_length: f64,
    /// Finger length, all four fingers together.
    pub finger_length: f64,
    /// Finger thickness.
    pub finger_thickness: f64,
    /// Thumb length.
    pub thumb_length: f64,
    /// How far the thumb stands out from the palm, in degrees added to the template's rest angle (`-30..=30`;
    /// positive opens the thumb). An offset rather than an absolute angle, so `0` is the template on every skeleton.
    pub thumb_spread_deg: f64,
    /// Fingertip taper: below `1` blunter, above `1` more pointed.
    pub taper: f64,
}

impl Default for HandParams {
    fn default() -> Self {
        Self {
            palm_width: 1.0,
            palm_length: 1.0,
            finger_length: 1.0,
            finger_thickness: 1.0,
            thumb_length: 1.0,
            thumb_spread_deg: 0.0,
            taper: 1.0,
        }
    }
}

impl Validate for HandParams {
    fn check(&self, c: &mut Check) {
        for (name, v) in [
            ("palm_width", self.palm_width),
            ("palm_length", self.palm_length),
            ("finger_length", self.finger_length),
            ("finger_thickness", self.finger_thickness),
            ("thumb_length", self.thumb_length),
            ("taper", self.taper),
        ] {
            c.range(name, v, 0.5, 2.0);
        }
        c.range("thumb_spread_deg", self.thumb_spread_deg, -30.0, 30.0);
    }
}

/// The parameters of the `body` part kind: the bare body under the clothes.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct BodyParams {
    /// Proportion sliders.
    pub proportions: Proportions,
    /// The body-shape space (CB-04).
    pub shape: BodyShape,
    /// How the hands are built (`hand` below shapes them).
    pub hands: HandStyle,
    /// The shape of the hands (RG-09).
    pub hand: HandParams,
    /// Skin colours (the same palette slots as the head's).
    pub skin: SkinColours,
}

impl Validate for BodyParams {
    fn check(&self, c: &mut Check) {
        c.nested("proportions", &self.proportions);
        c.nested("shape", &self.shape);
        c.nested("hand", &self.hand);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_leave_the_template_unchanged_and_are_valid() {
        let b = BodyParams::default();
        b.validate().unwrap();
        assert_eq!(b.proportions, Proportions::default());
        assert_eq!(b.shape, BodyShape::default());
        assert_eq!(b.hand, HandParams::default());
        assert_eq!(b.hands, HandStyle::Mitten);
        let json = serde_json::to_string(&b).unwrap();
        assert!(json.contains(r#""hands":"mitten""#), "{json}");
        assert_eq!(serde_json::from_str::<BodyParams>(&json).unwrap(), b);
    }

    #[test]
    fn extreme_proportions_are_rejected_with_their_field() {
        let mut b = BodyParams::default();
        b.proportions.leg_length = 3.0;
        let e = b.validate().unwrap_err();
        assert_eq!(e.0[0].path, "proportions/leg_length");
    }

    #[test]
    fn shapes_blend_and_keep_a_preset_only_where_it_is_still_true() {
        let id = |s: &str| Some(BodyShapeId::new(s).unwrap());
        let slim = BodyShape { build: -1.0, curves: 0.2, preset: id("slim"), ..BodyShape::default() };
        let broad = BodyShape { build: 1.0, height: 0.5, preset: id("broad"), ..BodyShape::default() };
        assert_eq!(slim.lerp(&broad, 0.0), slim);
        assert_eq!(slim.lerp(&broad, 1.0), broad);
        let mid = slim.lerp(&broad, 0.5);
        assert!(mid.build.abs() < 1e-12 && (mid.height - 0.25).abs() < 1e-12 && (mid.curves - 0.1).abs() < 1e-12);
        assert_eq!(mid.preset, None, "halfway between two presets is neither");
        let same = BodyShape { height: -0.5, ..slim.clone() };
        assert_eq!(slim.lerp(&same, 0.3).preset, id("slim"));
        assert_eq!(
            BodyShape::default().lerp(&BodyShape::default(), 0.7),
            BodyShape::default(),
            "identity blends to identity"
        );
    }

    #[test]
    fn shape_and_hand_ranges_are_rejected_with_their_field_path() {
        let mut b = BodyParams::default();
        b.shape.build = 1.5;
        b.shape.height = f64::NAN;
        b.hand.thumb_length = 0.4;
        b.hand.thumb_spread_deg = 45.0;
        let paths: Vec<_> = b.validate().unwrap_err().0.iter().map(|e| e.path.clone()).collect();
        assert_eq!(paths, ["shape/height", "shape/build", "hand/thumb_length", "hand/thumb_spread_deg"]);
    }
}
