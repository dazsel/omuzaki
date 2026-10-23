//! The layout as ratios (CB-12): fractions of the eye-to-chin drop, and lengths over the eye distance. They describe a
//! face whatever the head's size, so they are what the overlay shows, what a reference is compared with, and what
//! `face.set_layout_ratios` inverts.

use omuzaki_face_api::{Check, FaceLayout, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The layout as ratios. Fractions are of the eye-to-chin drop, measured down from the eye line: 0 is the eye line, 1 is
/// the chin, so a larger fraction sits lower on the face. Lengths are over the eye distance, the distance between the eye
/// centres, so they do not depend on how big the head is.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LayoutRatios {
    /// The eyes' size: half an eye's width over the eye distance (`FaceLayout::eye_width` is a half-width, so a drawn
    /// eye's full width is twice this ratio's numerator).
    pub eye_width_to_iod: f64,
    /// The eye line to the chin, over the eye distance: how long the lower face is.
    pub drop_to_iod: f64,
    /// The nose's height as a fraction of the eye-to-chin drop.
    pub nose: f64,
    /// The mouth line's height as a fraction of the eye-to-chin drop.
    pub mouth: f64,
    /// The blush's height as a fraction of the eye-to-chin drop.
    pub blush: f64,
    /// The distance between the blush centres over the eye distance: how far apart the features sit across the face.
    pub blush_spacing_to_iod: f64,
}

impl LayoutRatios {
    /// The ratios of `layout` on a head whose chin is at `chin_y` (head-local). `None` when the chin is not below the eye
    /// line, since then no drop, and so no ratio, exists.
    pub fn measure(layout: &FaceLayout, chin_y: f64) -> Option<Self> {
        let base = layout.ratios(chin_y)?;
        let drop = layout.eye_y - chin_y;
        let iod = 2.0 * layout.eye_spacing;
        Some(Self {
            eye_width_to_iod: base.eye_width_to_iod,
            drop_to_iod: base.drop_to_iod,
            nose: base.nose,
            mouth: base.mouth,
            blush: (layout.eye_y - layout.blush_y) / drop,
            blush_spacing_to_iod: layout.blush_spacing / iod,
        })
    }

    /// The layout these ratios describe, on a head whose chin is at `chin_y`. The eye line and the brows stay as `current`
    /// has them: the chin fixes the eye-to-chin drop, the ratio fixes the eye distance from it, and every feature follows.
    /// This is the inverse of [`measure`](Self::measure): `measure(layout_on(current, c), c)` gives back these ratios.
    /// `None` when the chin is not below `current`'s eye line, or the drop ratio is not positive.
    pub fn layout_on(&self, current: &FaceLayout, chin_y: f64) -> Option<FaceLayout> {
        let drop = current.eye_y - chin_y;
        if drop <= 0.0 || self.drop_to_iod <= 0.0 {
            return None;
        }
        let iod = drop / self.drop_to_iod;
        Some(FaceLayout {
            eye_spacing: iod / 2.0,
            eye_width: self.eye_width_to_iod * iod,
            blush_spacing: self.blush_spacing_to_iod * iod,
            nose_y: current.eye_y - self.nose * drop,
            mouth_y: current.eye_y - self.mouth * drop,
            blush_y: current.eye_y - self.blush * drop,
            ..*current
        })
    }

    /// The field-by-field difference `self - other`.
    #[must_use]
    pub fn minus(&self, other: &Self) -> Self {
        Self {
            eye_width_to_iod: self.eye_width_to_iod - other.eye_width_to_iod,
            drop_to_iod: self.drop_to_iod - other.drop_to_iod,
            nose: self.nose - other.nose,
            mouth: self.mouth - other.mouth,
            blush: self.blush - other.blush,
            blush_spacing_to_iod: self.blush_spacing_to_iod - other.blush_spacing_to_iod,
        }
    }

    /// The ratio named `field`, one of [`FIELDS`]' names.
    fn get(&self, field: &str) -> Option<f64> {
        Some(match field {
            "eye_width_to_iod" => self.eye_width_to_iod,
            "drop_to_iod" => self.drop_to_iod,
            "nose" => self.nose,
            "mouth" => self.mouth,
            "blush" => self.blush,
            "blush_spacing_to_iod" => self.blush_spacing_to_iod,
            _ => return None,
        })
    }

    /// Plain-language lines for a difference (`self` is the character minus the reference), one per feature in field
    /// order. A difference that rounds to 0.00 at two places reads as a match, so the notes never claim a gap too small
    /// to see on the overlay.
    #[must_use]
    pub fn notes(difference: &Self) -> Vec<String> {
        FIELDS.iter().filter_map(|f| difference.get(f.name).map(|d| f.note(d))).collect()
    }
}

/// Each ratio: its name, and the words its note uses.
struct Field {
    name: &'static str,
    feature: &'static str,
    more: &'static str,
    less: &'static str,
    unit: &'static str,
}

impl Field {
    fn note(&self, diff: f64) -> String {
        note(self.feature, diff, self.more, self.less, self.unit)
    }
}

/// The ratios in field order.
const FIELDS: [Field; 6] = [
    Field {
        name: "eye_width_to_iod",
        feature: "the eye size",
        more: "larger",
        less: "smaller",
        unit: " (in eye distances)",
    },
    Field {
        name: "drop_to_iod",
        feature: "the eye-to-chin distance",
        more: "longer",
        less: "shorter",
        unit: " (in eye distances)",
    },
    Field { name: "nose", feature: "the nose", more: "lower", less: "higher", unit: "" },
    Field { name: "mouth", feature: "the mouth", more: "lower", less: "higher", unit: "" },
    Field { name: "blush", feature: "the blush", more: "lower", less: "higher", unit: "" },
    Field {
        name: "blush_spacing_to_iod",
        feature: "the blush spacing",
        more: "wider",
        less: "narrower",
        unit: " (in eye distances)",
    },
];

/// Reference ratios, any of them: an image measure gives only the ratios it can see (a profile shot has no eye
/// distance, a cropped one no chin). Every field is optional; a full set is exactly a [`LayoutRatios`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartialRatios {
    /// As [`LayoutRatios::eye_width_to_iod`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eye_width_to_iod: Option<f64>,
    /// As [`LayoutRatios::drop_to_iod`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drop_to_iod: Option<f64>,
    /// As [`LayoutRatios::nose`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nose: Option<f64>,
    /// As [`LayoutRatios::mouth`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mouth: Option<f64>,
    /// As [`LayoutRatios::blush`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blush: Option<f64>,
    /// As [`LayoutRatios::blush_spacing_to_iod`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blush_spacing_to_iod: Option<f64>,
}

impl PartialRatios {
    /// The ratio named `field`, if given.
    fn get(&self, field: &str) -> Option<f64> {
        match field {
            "eye_width_to_iod" => self.eye_width_to_iod,
            "drop_to_iod" => self.drop_to_iod,
            "nose" => self.nose,
            "mouth" => self.mouth,
            "blush" => self.blush,
            "blush_spacing_to_iod" => self.blush_spacing_to_iod,
            _ => None,
        }
    }

    /// Whether no ratio is given.
    pub fn is_empty(&self) -> bool {
        FIELDS.iter().all(|f| self.get(f.name).is_none())
    }

    /// `ratios` minus these, for the fields given, in field order.
    pub fn difference(&self, ratios: &LayoutRatios) -> Vec<(&'static str, f64)> {
        FIELDS
            .iter()
            .filter_map(|f| Some((f.name, ratios.get(f.name)? - self.get(f.name)?)))
            .collect()
    }

    /// One plain-language line per field given ([`LayoutRatios::notes`]), for `ratios` against these.
    pub fn notes(&self, ratios: &LayoutRatios) -> Vec<String> {
        let by_name = self.difference(ratios);
        FIELDS
            .iter()
            .filter_map(|f| by_name.iter().find(|(n, _)| *n == f.name).map(|(_, d)| f.note(*d)))
            .collect()
    }
}

impl Validate for PartialRatios {
    fn check(&self, c: &mut Check) {
        if let Some(v) = self.eye_width_to_iod {
            c.positive("eye_width_to_iod", v);
        }
        if let Some(v) = self.drop_to_iod {
            c.positive("drop_to_iod", v);
        }
        for (name, v) in [("nose", self.nose), ("mouth", self.mouth), ("blush", self.blush)] {
            if let Some(v) = v {
                c.range(name, v, 0.0, 1.0);
            }
        }
        if let Some(v) = self.blush_spacing_to_iod
            && !(v.is_finite() && v >= 0.0)
        {
            c.fail("blush_spacing_to_iod", format!("{v} must be 0 or more"));
        }
    }
}

/// One note: how far `diff` is from the reference, in the words for its direction.
fn note(feature: &str, diff: f64, more: &str, less: &str, unit: &str) -> String {
    let shown = format!("{:.2}", diff.abs());
    if shown == "0.00" {
        format!("{feature} matches the reference")
    } else if diff > 0.0 {
        format!("{feature} is {shown} {more} than the reference{unit}")
    } else {
        format!("{feature} is {shown} {less} than the reference{unit}")
    }
}

impl Validate for LayoutRatios {
    fn check(&self, c: &mut Check) {
        c.positive("eye_width_to_iod", self.eye_width_to_iod);
        c.positive("drop_to_iod", self.drop_to_iod);
        c.range("nose", self.nose, 0.0, 1.0);
        c.range("mouth", self.mouth, 0.0, 1.0);
        c.range("blush", self.blush, 0.0, 1.0);
        if !(self.blush_spacing_to_iod.is_finite() && self.blush_spacing_to_iod >= 0.0) {
            c.fail("blush_spacing_to_iod", format!("{} must be 0 or more", self.blush_spacing_to_iod));
        }
    }
}
