//! Face-shape presets (CB-10): `round-u`, `egg-oval`, `mochi`, `heart` and `long`, as library data.
//!
//! A preset is a [`ShapeProfile`]: a head outline and a feature layout that belong together, in head-local metres, and,
//! for a shape the default cranium cannot give (mochi's wide head, long's tall one), the cranium's width and height
//! as factors of the default's (`[cranium]`, #1690). The outline is fitted on that scaled cranium. The five files are `library/shape-profiles/*.toml`, compiled in. [`ShapeProfiles`] reads them once and hands them out
//! (`get`, `list`) and blends them (`blend` for two, `mix` for any weights); the `face.set_profile` op writes the
//! blended numbers into a character, so the document never refers to a preset.
//!
//! **How they were made.** Each preset is designed from a written geometric description, not traced from any
//! character: the description is turned into numbers first (the silhouette ratios, the chin angle and the CB-12
//! feature ratios in `tests/shape_presets.rs`, the single table of targets), and the outline and layout are then
//! fitted until the head built from them ([`crate::head::mesh::build`]) measures those numbers
//! ([`measure::Silhouette`]) to within 5%.
//!
//! **Inside the plausible space.** The head builder clamps every outline to [`super::outline::plausible`]. A preset
//! stores numbers already inside it (on its own cranium), so the outline it names is the outline that is built (a test holds every preset
//! to that): for instance the exponent never goes below what keeps the chin angle at
//! [`super::outline::MIN_TIP_DEG`], however pointed the description reads.
//!
//! **Blending.** Presets all carry an explicit chin, so blends interpolate every number and no chin switches in at
//! half way; the head built from a blend moves in proportion to the blend step (`tests/head_shape.rs` holds that for
//! any pair of outlines).

pub mod measure;

use std::sync::OnceLock;

use omuzaki_char_api::{CraniumScale, HeadOutline, ProfileId, ShapeProfile};
use omuzaki_face_api::{FaceLayout, Validate};
use serde::Deserialize;

/// Why a preset could not be loaded or used.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ShapeProfileError {
    /// A library file is malformed. The library is compiled in, so this is a bug in the data, which the crate's own
    /// tests catch.
    #[error("shape profile `{file}`: {reason}")]
    Library {
        /// The library file.
        file: &'static str,
        /// What is wrong with it.
        reason: String,
    },
    /// No preset has this id.
    #[error("no face-shape preset called `{0}`; the presets are round-u, egg-oval, mochi, heart and long")]
    Unknown(String),
    /// A blend factor or weight is out of range.
    #[error("bad blend: {0}")]
    Blend(&'static str),
}

/// One preset of the library.
#[derive(Clone, Debug, PartialEq)]
pub struct ShapePreset {
    /// The preset's id (`mochi`), also its file's name.
    pub id: ProfileId,
    /// One line for pickers.
    pub summary: String,
    /// The numbers: the head outline, the feature layout and, for some, the cranium's proportions.
    pub profile: ShapeProfile,
}

/// The file's contents.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PresetFile {
    id: ProfileId,
    summary: String,
    outline: HeadOutline,
    layout: FaceLayout,
    /// The cranium's proportions, for a preset that needs its own (mochi wide, long tall, #1690).
    #[serde(default)]
    cranium: Option<CraniumScale>,
}

/// Every library file, in the order of [`omuzaki_char_api::PRESET_IDS`].
const LIBRARY: [(&str, &str); 5] = [
    ("round-u", include_str!("../../../library/shape-profiles/round-u.toml")),
    ("egg-oval", include_str!("../../../library/shape-profiles/egg-oval.toml")),
    ("mochi", include_str!("../../../library/shape-profiles/mochi.toml")),
    ("heart", include_str!("../../../library/shape-profiles/heart.toml")),
    ("long", include_str!("../../../library/shape-profiles/long.toml")),
];

fn parse(file: &'static str, source: &str) -> Result<ShapePreset, ShapeProfileError> {
    let fail = |reason: String| ShapeProfileError::Library { file, reason };
    let p: PresetFile = toml::from_str(source).map_err(|e| fail(e.to_string()))?;
    if p.id.as_str() != file {
        return Err(fail(format!("its id is `{}`, not the file's name", p.id.as_str())));
    }
    if p.outline.chin.is_none() {
        return Err(fail("a preset states its chin, so that blends never switch one in half way".into()));
    }
    p.outline.validate().map_err(|e| fail(e.to_string()))?;
    p.layout.validate().map_err(|e| fail(e.to_string()))?;
    if let Some(scale) = &p.cranium {
        scale.validate().map_err(|e| fail(e.to_string()))?;
    }
    Ok(ShapePreset {
        id: p.id,
        summary: p.summary,
        profile: ShapeProfile { outline: p.outline, layout: p.layout, cranium: p.cranium },
    })
}

/// The face-shape preset library.
#[derive(Clone, Copy, Debug)]
pub struct ShapeProfiles;

impl ShapeProfiles {
    /// The five presets, in [`omuzaki_char_api::PRESET_IDS`] order, parsed once.
    ///
    /// # Errors
    /// [`ShapeProfileError::Library`] if a library file is malformed (the crate's tests keep that from shipping).
    pub fn list() -> Result<&'static [ShapePreset], ShapeProfileError> {
        static PRESETS: OnceLock<Result<Vec<ShapePreset>, ShapeProfileError>> = OnceLock::new();
        PRESETS
            .get_or_init(|| LIBRARY.iter().map(|(file, source)| parse(file, source)).collect())
            .as_ref()
            .map(Vec::as_slice)
            .map_err(Clone::clone)
    }

    /// The preset called `id`.
    ///
    /// # Errors
    /// [`ShapeProfileError::Unknown`] for an id that is not one of the five; [`ShapeProfileError::Library`] as
    /// [`ShapeProfiles::list`].
    pub fn get(id: &str) -> Result<&'static ShapePreset, ShapeProfileError> {
        Self::list()?.iter().find(|p| p.id.as_str() == id).ok_or_else(|| ShapeProfileError::Unknown(id.to_owned()))
    }

    /// The blend of two presets: `t = 0` is `from`, `t = 1` is `to`.
    ///
    /// # Errors
    /// [`ShapeProfileError::Unknown`] for a bad id; [`ShapeProfileError::Blend`] if `t` is not in `0..=1`.
    pub fn blend(from: &str, to: &str, t: f64) -> Result<ShapeProfile, ShapeProfileError> {
        if !(0.0..=1.0).contains(&t) {
            return Err(ShapeProfileError::Blend("the blend factor must be in 0..=1"));
        }
        Ok(Self::get(from)?.profile.lerp(&Self::get(to)?.profile, t))
    }

    /// The weighted mean of any number of presets: `[("mochi", 0.7), ("heart", 0.3)]`. The weights need not sum to
    /// 1; they are normalised. Listing a preset twice adds its weights.
    ///
    /// # Errors
    /// [`ShapeProfileError::Unknown`] for a bad id; [`ShapeProfileError::Blend`] for an empty list, a negative or
    /// non-finite weight, or weights that sum to zero.
    pub fn mix(weights: &[(&str, f64)]) -> Result<ShapeProfile, ShapeProfileError> {
        if weights.iter().any(|(_, w)| !w.is_finite() || *w < 0.0) {
            return Err(ShapeProfileError::Blend("weights must be finite and not negative"));
        }
        // A running mean: folding `next` in at `w / (total so far + w)` keeps every step a convex blend.
        let mut mean: Option<(ShapeProfile, f64)> = None;
        for (id, w) in weights {
            let next = Self::get(id)?.profile;
            if *w == 0.0 {
                continue;
            }
            mean = Some(match mean {
                None => (next, *w),
                Some((so_far, total)) => (so_far.lerp(&next, w / (total + w)), total + w),
            });
        }
        mean.map(|(profile, _)| profile).ok_or(ShapeProfileError::Blend("a mix needs at least one positive weight"))
    }
}
