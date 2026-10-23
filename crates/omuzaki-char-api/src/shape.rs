//! Face-shape profiles (spec CB-10, CB-12): one bundle of head outline and feature layout that presets, blending
//! and the report card treat as a unit.
//!
//! A profile touches two files: the head part's `outline` (3D shape) and the face's `layout` (where the painted
//! features sit). The document stores the resulting **explicit numbers**, never a preset reference, so undo is exact,
//! variants override single fields, and a character does not change when the preset library is edited. The
//! `face.set_profile` op loads a preset from the library, blends it with the current numbers and writes a patch
//! touching both places in one transaction.
//!
//! Preset ids are library data (spec §28), not contract; the five CB-10 names are reserved: [`PRESET_IDS`].

use omuzaki_face_api::{Check, FaceLayout, FaceProfile, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::head::{Cranium, HeadOutline, HeadParams};

/// The reserved preset ids (CB-10). `egg-oval` is the close-up profile the prototype called by another name:
/// longer lower face, widest at the temples, tapering to a soft chin.
pub const PRESET_IDS: [&str; 5] = ["round-u", "egg-oval", "mochi", "heart", "long"];

/// The cranium's proportions a face shape asks for (#835): its width and height as multiples of [`Cranium::default`]'s.
/// A wide, round preset (`mochi`) widens the head; a long one (`long`) makes it taller and narrower.
///
/// `width` scales the cranium's `half_width`; `height` scales its `half_height_up`, `half_height_down_back` and
/// `half_height_down_front`. Both are relative to the default cranium, not to the head they are applied to, so applying
/// a profile twice gives the same head as applying it once. The plausible range of each is [`CraniumScale::RANGE`]: a
/// head well outside it reads as another species, not another face shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct CraniumScale {
    /// The cranium's width over the default's.
    pub width: f64,
    /// The cranium's height over the default's.
    pub height: f64,
}

impl Default for CraniumScale {
    fn default() -> Self {
        Self { width: 1.0, height: 1.0 }
    }
}

impl CraniumScale {
    /// The plausible range of each factor.
    pub const RANGE: (f64, f64) = (0.8, 1.25);

    /// A cranium `width` and `height` times the default's.
    #[must_use]
    pub fn new(width: f64, height: f64) -> Self {
        Self { width, height }
    }

    /// The scale `cranium` has against the default cranium, or `None` when it has the default's proportions exactly.
    #[must_use]
    #[allow(clippy::float_cmp)] // "exactly the default" is what keeps an unscaled head's profile without a cranium
    pub fn of(cranium: &Cranium) -> Option<Self> {
        let base = Cranium::default();
        let scale = Self::new(cranium.half_width / base.half_width, cranium.half_height_up / base.half_height_up);
        (scale != Self::default()).then_some(scale)
    }

    /// Writes the proportions into `cranium`, relative to the default cranium.
    pub fn apply(&self, cranium: &mut Cranium) {
        let base = Cranium::default();
        cranium.half_width = base.half_width * self.width;
        cranium.half_height_up = base.half_height_up * self.height;
        cranium.half_height_down_back = base.half_height_down_back * self.height;
        cranium.half_height_down_front = base.half_height_down_front * self.height;
    }

    /// Linear blend: `t = 0` is `self`, `t = 1` is `other`.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        let m = |a: f64, b: f64| a + (b - a) * t;
        Self::new(m(self.width, other.width), m(self.height, other.height))
    }
}

impl Validate for CraniumScale {
    fn check(&self, c: &mut Check) {
        c.range("width", self.width, Self::RANGE.0, Self::RANGE.1);
        c.range("height", self.height, Self::RANGE.0, Self::RANGE.1);
    }
}

/// An outline and a layout that belong together, and the cranium proportions the shape needs, if any.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeProfile {
    /// The head outline.
    pub outline: HeadOutline,
    /// The feature layout.
    pub layout: FaceLayout,
    /// The cranium's width and height against the default cranium's; `None` for the default's (1 x 1). Not written
    /// when `None`, so a profile without it reads and writes as before.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cranium: Option<CraniumScale>,
}

impl ShapeProfile {
    /// What a character has now.
    pub fn of(head: &HeadParams, face: &FaceProfile) -> Self {
        Self { outline: head.outline, layout: face.layout, cranium: CraniumScale::of(&head.cranium) }
    }

    /// Writes the profile into a character's head and face. A profile with no cranium scale leaves the cranium as it
    /// is: [`ShapeProfile::lerp`] turns a blend away from a scaled profile into an explicit 1 x 1, so moving off a wide
    /// preset brings the head back.
    pub fn apply(&self, head: &mut HeadParams, face: &mut FaceProfile) {
        head.outline = self.outline;
        face.layout = self.layout;
        if let Some(scale) = self.cranium {
            scale.apply(&mut head.cranium);
        }
    }

    /// Linear blend: `t = 0` is `self`, `t = 1` is `other`. A side with no cranium scale counts as 1 x 1, and the blend
    /// has none only when neither side has one.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        let cranium = match (self.cranium, other.cranium) {
            (None, None) => None,
            (a, b) => Some(a.unwrap_or_default().lerp(&b.unwrap_or_default(), t)),
        };
        Self { outline: self.outline.lerp(&other.outline, t), layout: self.layout.lerp(&other.layout, t), cranium }
    }
}

impl Validate for ShapeProfile {
    fn check(&self, c: &mut Check) {
        c.nested("outline", &self.outline);
        c.nested("layout", &self.layout);
        if let Some(scale) = &self.cranium {
            c.nested("cranium", scale);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::head::Chin;

    #[test]
    fn blending_two_profiles_blends_outline_and_layout() {
        let a = ShapeProfile { outline: HeadOutline::default(), layout: FaceLayout::default(), cranium: None };
        let b = ShapeProfile {
            outline: HeadOutline { chin: Some(Chin::default()), exponent: 1.7, ..HeadOutline::default() },
            layout: FaceLayout { mouth_y: -0.2, ..FaceLayout::default() },
            cranium: None,
        };
        let mid = a.lerp(&b, 0.5);
        assert!((mid.layout.mouth_y - -0.175).abs() < 1e-6 && (mid.outline.exponent - 1.8).abs() < 1e-6);
        assert_eq!(a.lerp(&b, 0.0), a);
    }

    #[test]
    fn apply_then_of_round_trips_and_keeps_everything_else() {
        let mut head = HeadParams::default();
        let mut face = FaceProfile::default();
        let p = ShapeProfile {
            outline: HeadOutline { exponent: 1.5, ..HeadOutline::default() },
            layout: FaceLayout { eye_y: -0.08, ..FaceLayout::default() },
            cranium: None,
        };
        p.apply(&mut head, &mut face);
        assert_eq!(ShapeProfile::of(&head, &face), p);
        head.validate().unwrap();
        face.validate().unwrap();
        assert_eq!(head.ears, HeadParams::default().ears, "ears are untouched");

        // With a cranium scale: `of` reads it back, and applying twice is applying once.
        let scaled = ShapeProfile { cranium: Some(CraniumScale::new(1.1, 0.9)), ..p };
        scaled.apply(&mut head, &mut face);
        let once = head.clone();
        scaled.apply(&mut head, &mut face);
        assert_eq!(head, once);
        let read = ShapeProfile::of(&head, &face).cranium.unwrap();
        assert!((read.width - 1.1).abs() < 1e-12 && (read.height - 0.9).abs() < 1e-12, "{read:?}");
        assert!((head.cranium.half_height_down_front - 0.9 * Cranium::default().half_height_down_front).abs() < 1e-12);
        assert_eq!(head.cranium.half_depth_front, Cranium::default().half_depth_front, "depth is untouched");
        head.validate().unwrap();
        // An explicit 1 x 1 brings the default cranium back, and then the profile reads as having none.
        ShapeProfile { cranium: Some(CraniumScale::default()), ..p }.apply(&mut head, &mut face);
        assert_eq!(head.cranium, Cranium::default());
        assert_eq!(ShapeProfile::of(&head, &face), p);
    }

    #[test]
    fn a_profile_without_a_cranium_reads_and_writes_as_before() {
        let p = ShapeProfile { outline: HeadOutline::default(), layout: FaceLayout::default(), cranium: None };
        let json = serde_json::to_value(p).unwrap();
        assert!(json.get("cranium").is_none(), "{json}");
        assert_eq!(serde_json::from_value::<ShapeProfile>(json).unwrap(), p);
        let scaled = ShapeProfile { cranium: Some(CraniumScale::new(1.2, 1.0)), ..p };
        assert_eq!(serde_json::from_value::<ShapeProfile>(serde_json::to_value(scaled).unwrap()).unwrap(), scaled);
    }

    #[test]
    fn blending_cranium_scales_counts_a_missing_one_as_one() {
        let base = ShapeProfile { outline: HeadOutline::default(), layout: FaceLayout::default(), cranium: None };
        let wide = ShapeProfile { cranium: Some(CraniumScale::new(1.2, 0.9)), ..base };
        assert_eq!(base.lerp(&base, 0.5).cranium, None, "two without stay without");
        let half = base.lerp(&wide, 0.5).cranium.unwrap();
        assert!((half.width - 1.1).abs() < 1e-12 && (half.height - 0.95).abs() < 1e-12, "{half:?}");
        let back = wide.lerp(&base, 1.0).cranium.unwrap();
        assert_eq!(back, CraniumScale::default(), "moving fully off a scaled profile asks for the default cranium");
        assert_eq!(wide.lerp(&base, 0.0).cranium, wide.cranium);
        let both =
            wide.lerp(&ShapeProfile { cranium: Some(CraniumScale::new(1.0, 1.1)), ..base }, 0.5).cranium.unwrap();
        assert!((both.width - 1.1).abs() < 1e-12 && (both.height - 1.0).abs() < 1e-12, "{both:?}");
    }

    #[test]
    fn a_cranium_scale_out_of_range_fails_validation_naming_the_field() {
        let base = ShapeProfile { outline: HeadOutline::default(), layout: FaceLayout::default(), cranium: None };
        base.validate().unwrap();
        let wide = ShapeProfile { cranium: Some(CraniumScale::new(1.5, 1.0)), ..base };
        let errors = wide.validate().unwrap_err().0;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].path, "cranium/width");
        let short = ShapeProfile { cranium: Some(CraniumScale::new(1.0, 0.5)), ..base };
        assert_eq!(short.validate().unwrap_err().0[0].path, "cranium/height");
    }

    #[test]
    fn preset_ids_are_valid_slugs() {
        for id in PRESET_IDS {
            crate::ids::ProfileId::new(id).unwrap();
        }
    }
}
