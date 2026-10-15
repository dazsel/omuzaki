//! Parity of the Rust face painter with the JS prototype's face sections (the references are isolated
//! section PNGs, and the face state per case comes from the prototype's dump).
//!
//! The prototype's `face.js` paints the whole face into one canvas. A replay tool ran its Canvas 2D calls once per section
//! and saved each section alone, for each golden case (an expression in a variant). This harness paints the same case
//! with [`RealPainter::paint_section`], rasterises it on the CPU, and compares it with those PNGs inside the section's
//! bounding rectangle ([`regioncmp`](omuzaki_test::regioncmp)).
//!
//! - [`js_params`]: the crosswalk from the prototype's face record to the painter's inputs.
//! - [`sections`]: which prototype tags each [`Section`] covers, one comparison per case, and the report.
//!
//! # Enforcement without a shared table
//!
//! Every section task adds its **own** `crates/omuzaki-test-face/tests/face_parity_<section>.rs`, calling [`check_section`]
//! with its tolerance, so section PRs never edit a common file. A section without such a test is only printed by
//! [`report`] (`tools/face-parity-report.sh`). Every entry point skips, saying why, when the local assets are absent.
//!
//! # What stays local
//!
//! The references and the face profiles are the sample character's, so they stay in a local assets folder: nothing here is committed with
//! them, and the pictures this module writes go to a local assets folder too. The tests that run everywhere use made-up
//! faces. The painter needs the face design (a [`FaceProfile`]) and the window it paints over (a [`FaceProjection`]);
//! for each look (`sheet` for the `game` variant, `egg-oval` for `close-up`) they are read from
//! `face-sections/parity-setup/<look>.json` beside the references, `{ "projection": .., "profile": .., "palette": .. }`
//! with every part optional. `follow_weight` is how much of the prototype's camera-follow gaze counts as driven gaze (the
//! prototype tunes its iris travel to its character's eye, so the number is not in code). Without that file
//! the default profile, a window the size of the canvas and a follow weight of 0 are used, and the report says so: parity numbers against a default profile only show that the harness runs.
//!
//! [`RealPainter::paint_section`]: omuzaki_face::RealPainter::paint_section
//! [`Section`]: omuzaki_face::Section

use std::path::PathBuf;

use omuzaki_face_api::{DEFAULT_PX_PER_M, FaceError, FaceProfile, FaceProjection, FaceRect, Palette, ResolveError};
use serde::Deserialize;

use omuzaki_test::compare::{CompareError, Tolerance};
use omuzaki_test::jsref::{JsRef, JsRefError};
use omuzaki_test::regioncmp::{RegionError, T_FACE, T_FACE_MIN_PSNR_DB};

pub mod js_params;
pub mod sections;

pub use js_params::ParamError;
pub use sections::{
    CaseResult, InkDrift, Outcome, SectionCheck, SectionReport, Select, check_section, check_tags, report,
};

/// The golden variants and the look of the character each one shows: the prototype's `game` variant is the character
/// sheet's face, `close-up` is the rounder close-up face.
pub const LOOKS: [(&str, &str); 2] = [("game", "sheet"), ("close-up", "egg-oval")];

/// How closely a section must match: the per-pixel rule and the PSNR it must reach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SectionTolerance {
    /// A pixel is bad when a premultiplied channel differs by more than `per_channel`; at most `max_bad_fraction` of
    /// the section's rectangle may be bad.
    pub pixels: Tolerance,
    /// The PSNR the rectangle must reach, in decibels.
    pub min_psnr_db: f64,
}

impl SectionTolerance {
    /// The wave-3 rule for face renders ([`T_FACE`] and [`T_FACE_MIN_PSNR_DB`]): per channel 12, at most 1 % bad
    /// pixels, 34 dB.
    pub const T_FACE: Self = Self { pixels: T_FACE, min_psnr_db: T_FACE_MIN_PSNR_DB };
}

/// Why a parity run could not be completed (as opposed to a section failing the comparison).
#[derive(Debug, thiserror::Error)]
pub enum ParityError {
    /// A reference dump could not be read.
    #[error(transparent)]
    Reference(#[from] JsRefError),
    /// A file of the harness's own could not be read or written.
    #[error("{}: {message}", path.display())]
    File {
        /// The file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// A face record could not be mapped to paint parameters.
    #[error("case {case}: {source}")]
    Params {
        /// The case id.
        case: String,
        /// The cause.
        source: ParamError,
    },
    /// The painter or rasteriser refused a case.
    #[error("case {case}: {source}")]
    Face {
        /// The case id.
        case: String,
        /// The cause.
        source: FaceError,
    },
    /// The profile names a palette slot the palette lacks.
    #[error("the face profile for {look}: {source}")]
    Palette {
        /// The look (`sheet` or `egg-oval`).
        look: String,
        /// The cause.
        source: ResolveError,
    },
    /// The index of references does not describe what is on disk, or names a case the face state lacks.
    #[error("inconsistent references: {0}")]
    Inconsistent(String),
    /// A comparison could not be made.
    #[error("case {case}: {source}")]
    Region {
        /// The case id.
        case: String,
        /// The cause.
        source: RegionError,
    },
}

impl From<CompareError> for ParityError {
    fn from(e: CompareError) -> Self {
        Self::Inconsistent(e.to_string())
    }
}

/// The face design and window a variant is painted with, and where they came from.
#[derive(Debug, Clone)]
pub struct Setup {
    /// The window the layers cover.
    pub projection: FaceProjection,
    /// The face design, palette slots resolved.
    pub profile: FaceProfile,
    /// How much of the camera-follow gaze counts as driven gaze (0 when unknown: the follow is ignored).
    pub follow_weight: f64,
    /// The parts the setup file did not give (`projection`, `profile`, `follow_weight`), which defaults stand in for;
    /// all three when there is no file. Empty for a complete setup.
    pub defaulted: Vec<&'static str>,
}

/// The shape of `parity-setup/<look>.json`.
#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SetupFile {
    projection: Option<FaceProjection>,
    profile: Option<FaceProfile>,
    palette: Palette,
    follow_weight: Option<f64>,
}

/// The setup for `variant` (a [`LOOKS`] key), for a face canvas of `canvas` pixels.
///
/// # Errors
/// [`ParityError::File`] when the file exists but cannot be parsed, [`ParityError::Palette`] for an unknown slot.
pub fn load_setup(js: &JsRef, variant: &str, canvas: (u32, u32)) -> Result<Setup, ParityError> {
    let look = LOOKS.iter().find(|(v, _)| *v == variant).map_or(variant, |(_, look)| *look);
    let path = js.root().join("face-sections").join("parity-setup").join(format!("{look}.json"));
    let file = match std::fs::read(&path) {
        Ok(bytes) => Some(
            serde_json::from_slice::<SetupFile>(&bytes)
                .map_err(|e| ParityError::File { path: path.clone(), message: e.to_string() })?,
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(ParityError::File { path, message: e.to_string() }),
    };
    let file = file.unwrap_or_default();
    let defaulted: Vec<&'static str> = [
        ("projection", file.projection.is_none()),
        ("profile", file.profile.is_none()),
        ("follow_weight", file.follow_weight.is_none()),
    ]
    .into_iter()
    .filter_map(|(part, missing)| missing.then_some(part))
    .collect();
    let projection = file.projection.unwrap_or_else(|| canvas_projection(canvas));
    let profile = file
        .profile
        .unwrap_or_default()
        .resolve_colours(&file.palette)
        .map_err(|source| ParityError::Palette { look: look.to_owned(), source })?;
    Ok(Setup { projection, profile, follow_weight: file.follow_weight.unwrap_or(0.0), defaulted })
}

/// A window exactly the size of the canvas at [`DEFAULT_PX_PER_M`], centred on the face-plane origin: what stands in
/// when the real window is not known.
fn canvas_projection((width, height): (u32, u32)) -> FaceProjection {
    // Canvas sizes are a few thousand pixels, exact in f32.
    #[allow(clippy::cast_precision_loss)]
    let (half_w, half_h) = (width as f32 / (2.0 * DEFAULT_PX_PER_M), height as f32 / (2.0 * DEFAULT_PX_PER_M));
    FaceProjection { rect: FaceRect { x0: -half_w, x1: half_w, y0: -half_h, y1: half_h }, front_fade: [0.0, 0.05] }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_with(json: Option<&str>) -> Setup {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("manifest.json"), "{}").unwrap();
        if let Some(json) = json {
            let file = dir.path().join("face-sections/parity-setup/sheet.json");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, json).unwrap();
        }
        load_setup(&JsRef::open_at(dir.path()).unwrap(), "game", (64, 48)).unwrap()
    }

    #[test]
    fn every_part_the_setup_file_lacks_is_reported_as_defaulted() {
        assert_eq!(setup_with(None).defaulted, ["projection", "profile", "follow_weight"]);
        // An empty file is not a complete setup.
        assert_eq!(setup_with(Some("{}")).defaulted, ["projection", "profile", "follow_weight"]);
        let partial = setup_with(Some(r#"{"follow_weight": 0.3, "profile": {}}"#));
        assert_eq!(partial.defaulted, ["projection"]);
        assert_eq!(partial.follow_weight, 0.3);
    }

    #[test]
    fn the_stand_in_window_is_the_canvas_at_the_default_density() {
        let p = canvas_projection((1536, 1107));
        assert_eq!(p.rect.size_px(DEFAULT_PX_PER_M), (1536, 1107));
    }

    #[test]
    fn the_looks_cover_the_two_golden_variants() {
        assert_eq!(LOOKS.map(|(variant, _)| variant), omuzaki_test::jsref::VARIANTS);
    }
}
