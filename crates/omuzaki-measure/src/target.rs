//! Likeness targets (design D8 and 6.1): what was measured and how, as a small TOML document in the project's store,
//! referenced by content hash from the character's `likeness` key. The inputs are the truth; the `[measured]` table is
//! a cache for a store that lacks the image, used only when its `measure_version` is current.

use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::face::likeness::Compare;
use crate::face::outline::{ANGLES, OutlineProfile, SampleState, Sector};
use crate::face::spacing::FeatureSpacing;
use crate::face::{FaceLandmarks, MEASURE_VERSION};

/// The target format's version.
pub const FORMAT: u32 = 1;

/// The view a target is of. Front only in v1 (design D11).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LikenessView {
    /// A front view.
    #[default]
    Front,
}

/// The reference image, by hash only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetImage {
    /// The image asset.
    pub asset: ContentHash,
}

/// The cached numbers of the last measurement.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measured {
    /// The version of the measures that made them.
    pub measure_version: u32,
    /// The 85 radii, IOD; the string `"-"` where not `ok`.
    pub outline: Vec<Radius>,
    /// The 85 states.
    pub state: Vec<SampleState>,
    /// The spacing fields measured.
    pub spacing: CachedSpacing,
}

/// One cached radius: a number, or `"-"` where the sample was not `ok`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Radius {
    /// The radius, IOD.
    Value(f64),
    /// Not measured.
    Missing(Dash),
}

/// The `"-"` of a missing radius.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Dash {
    /// `"-"`.
    #[serde(rename = "-")]
    Dash,
}

/// The cached spacing: the fields the likeness terms use, plus the width and aspect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CachedSpacing {
    /// Eye line to chin, IOD.
    pub eye_to_chin: f64,
    /// Eye line to mouth, IOD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eye_to_mouth: Option<f64>,
    /// The width at the mouth, IOD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_at_mouth: Option<f64>,
    /// The width at the eye line, IOD.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width_at_eyes: Option<f64>,
    /// The largest width, IOD.
    pub width_max: f64,
    /// Eye-to-chin over the largest width.
    pub aspect: f64,
}

/// A likeness target (design 6.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    /// The format version, [`FORMAT`].
    pub format: u32,
    /// The view.
    pub view: LikenessView,
    /// The preset this target is meant for.
    pub compare: Compare,
    /// Whether the landmarks are snapped at each use.
    pub refine: bool,
    /// Sectors left out.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<Sector>,
    /// The lit skin colour, `#rrggbb`, when it was given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skin: Option<String>,
    /// The reference image.
    pub image: TargetImage,
    /// The landmarks as placed.
    pub landmarks: FaceLandmarks,
    /// The cache.
    pub measured: Measured,
}

impl Target {
    /// The TOML of this target.
    ///
    /// # Errors
    /// A message when the value does not serialise (a non-finite number).
    pub fn to_toml(&self) -> Result<String, String> {
        toml::to_string(self).map_err(|e| e.to_string())
    }

    /// Reads a target.
    ///
    /// # Errors
    /// A message when the bytes are not a target of a known format.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, String> {
        let text = std::str::from_utf8(bytes).map_err(|_| "a likeness target is UTF-8 TOML".to_owned())?;
        let target: Self = toml::from_str(text).map_err(|e| e.to_string())?;
        if target.format != FORMAT {
            return Err(format!("format {} is not one this build reads (it reads {FORMAT})", target.format));
        }
        if target.measured.outline.len() != ANGLES || target.measured.state.len() != ANGLES {
            return Err(format!("the cached outline must have {ANGLES} samples"));
        }
        Ok(target)
    }

    /// Whether the cache was made by these measures.
    #[must_use]
    pub fn cache_is_current(&self) -> bool {
        self.measured.measure_version == MEASURE_VERSION
    }

    /// The cached outline.
    #[must_use]
    pub fn cached_outline(&self) -> OutlineProfile {
        let r = std::array::from_fn(|k| match self.measured.outline.get(k) {
            Some(Radius::Value(v)) => Some(*v),
            _ => None,
        });
        let state = std::array::from_fn(|k| self.measured.state.get(k).copied().unwrap_or(SampleState::Cut));
        OutlineProfile { r, state, beyond: [None; ANGLES], line_width: 0.0, hair: None }
    }

    /// The cached spacing.
    #[must_use]
    pub fn cached_spacing(&self) -> FeatureSpacing {
        let s = &self.measured.spacing;
        FeatureSpacing {
            eye_to_chin: s.eye_to_chin,
            eye_to_mouth: s.eye_to_mouth,
            mouth_to_chin: s.eye_to_mouth.map(|y| s.eye_to_chin - y),
            mouth: s.eye_to_mouth.map(|y| y / s.eye_to_chin),
            nose: None,
            width_at_eyes: s.width_at_eyes,
            width_at_mouth: s.width_at_mouth,
            width_max: s.width_max,
            aspect: s.aspect,
            unmeasured: std::collections::BTreeMap::new(),
        }
    }
}

/// The cache of a measurement.
#[must_use]
pub fn measured(outline: &OutlineProfile, spacing: &FeatureSpacing) -> Measured {
    Measured {
        measure_version: MEASURE_VERSION,
        outline: (0..ANGLES).map(|k| outline.ok(k).map_or(Radius::Missing(Dash::Dash), Radius::Value)).collect(),
        state: outline.state.to_vec(),
        spacing: CachedSpacing {
            eye_to_chin: spacing.eye_to_chin,
            eye_to_mouth: spacing.eye_to_mouth,
            width_at_mouth: spacing.width_at_mouth,
            width_at_eyes: spacing.width_at_eyes,
            width_max: spacing.width_max,
            aspect: spacing.aspect,
        },
    }
}
