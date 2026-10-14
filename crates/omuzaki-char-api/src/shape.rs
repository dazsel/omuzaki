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

use omuzaki_face_api::{FaceLayout, FaceProfile};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::head::{HeadOutline, HeadParams};

/// The reserved preset ids (CB-10). `egg-oval` is the close-up profile the prototype called by another name:
/// longer lower face, widest at the temples, tapering to a soft chin.
pub const PRESET_IDS: [&str; 5] = ["round-u", "egg-oval", "mochi", "heart", "long"];

/// An outline and a layout that belong together.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShapeProfile {
    /// The head outline.
    pub outline: HeadOutline,
    /// The feature layout.
    pub layout: FaceLayout,
}

impl ShapeProfile {
    /// What a character has now.
    pub fn of(head: &HeadParams, face: &FaceProfile) -> Self {
        Self { outline: head.outline, layout: face.layout }
    }

    /// Writes the profile into a character's head and face.
    pub fn apply(&self, head: &mut HeadParams, face: &mut FaceProfile) {
        head.outline = self.outline;
        face.layout = self.layout;
    }

    /// Linear blend: `t = 0` is `self`, `t = 1` is `other`.
    #[must_use]
    pub fn lerp(&self, other: &Self, t: f64) -> Self {
        Self { outline: self.outline.lerp(&other.outline, t), layout: self.layout.lerp(&other.layout, t) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::head::Chin;
    use omuzaki_face_api::Validate;

    #[test]
    fn blending_two_profiles_blends_outline_and_layout() {
        let a = ShapeProfile { outline: HeadOutline::default(), layout: FaceLayout::default() };
        let b = ShapeProfile {
            outline: HeadOutline { chin: Some(Chin::default()), exponent: 1.7, ..HeadOutline::default() },
            layout: FaceLayout { mouth_y: -0.2, ..FaceLayout::default() },
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
        };
        p.apply(&mut head, &mut face);
        assert_eq!(ShapeProfile::of(&head, &face), p);
        head.validate().unwrap();
        face.validate().unwrap();
        assert_eq!(head.ears, HeadParams::default().ears, "ears are untouched");
    }

    #[test]
    fn preset_ids_are_valid_slugs() {
        for id in PRESET_IDS {
            crate::ids::ProfileId::new(id).unwrap();
        }
    }
}
