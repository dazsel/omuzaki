//! The render-preset library (#376, `docs/design/likeness-measures.md` D10): named looks that `render.preview` takes as
//! `preset`, as data. Each preset is one TOML file under `library/render-presets/`; a preset fixes some of a picture's
//! settings (the camera, the light rig, the expression, the post effects, the line) and leaves the character's geometry
//! alone. `measure`, the look every `measure.*` op reads, is the first. Applying a preset is the op's job; this file
//! reads and checks the files, so a broken file fails the test that loads it.

use omuzaki_face_api::ExpressionId;
use omuzaki_render_api::{Colour, EdgeLines};
use omuzaki_stage_api::Framing;
use serde::Deserialize;

use super::{LightRig, RecipeError};

/// The presets shipped with the crate, by name.
static LIBRARY: [(&str, &str); 1] = [("measure", include_str!("../../library/render-presets/measure.toml"))];

/// The word a preset's `expression` uses for the face at rest, with no expression on it.
pub const NEUTRAL: &str = "neutral";

/// One render preset.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct RenderPreset {
    /// Its name, as `render.preview`'s `preset` spells it.
    pub name: String,
    /// One sentence on what it is for.
    pub summary: String,
    /// The camera it shoots from, if it fixes one.
    #[serde(default)]
    pub camera: Option<PresetCamera>,
    /// The light rig of the library (`stage.light_rigs`) it lights with, if it fixes one.
    #[serde(default)]
    pub light_rig: Option<String>,
    /// The expression it shows: [`NEUTRAL`] for the face at rest, or an expression of the character's face profile.
    #[serde(default)]
    pub expression: Option<String>,
    /// Whether the scene's post effects stay.
    #[serde(default)]
    pub post: PresetPost,
    /// The line drawn around every outlined material, if it fixes one.
    #[serde(default)]
    pub outline: Option<PresetOutline>,
    /// The fewest pixels between the eye centres a picture in this preset may have, if it asks for a size.
    #[serde(default)]
    pub min_iod_px: Option<f32>,
}

/// A preset's camera.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PresetCamera {
    /// The framing preset: `face`, `bust`, `full` or `hands`.
    pub framing: Framing,
    /// How it projects.
    #[serde(default)]
    pub projection: PresetProjection,
    /// Degrees around the character: 0 is in front.
    #[serde(default)]
    pub yaw: f32,
    /// Degrees: + looks from above.
    #[serde(default)]
    pub pitch: f32,
}

/// How a preset's camera projects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetProjection {
    /// The framing's own lens.
    #[default]
    Perspective,
    /// Parallel rays, framing at the head what the framing's lens frames there: no lens distortion between the eyes
    /// and the chin.
    Orthographic,
}

/// Whether a preset keeps the scene's post effects.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetPost {
    /// As the scene and the character's look set them.
    #[default]
    Scene,
    /// No bloom, depth of field, grain or grade; exposure and tone mapping stay.
    Off,
}

/// The line a preset draws around every outlined material.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct PresetOutline {
    /// The line colour, in place of each material's own.
    pub colour: Colour,
    /// The thinnest the line may be in the picture, in pixels: a thinner line is widened to this, a wider one stays.
    pub min_px: f32,
    /// When set, a line `min_px` wide is also drawn wherever the depth jumps by more than this share of the depth (the
    /// render-api `EdgeLines` step): under the chin, at the ears and along the hair, where a hull line hides behind
    /// the part in front.
    #[serde(default)]
    pub edge_step: Option<f32>,
}

impl RenderPreset {
    /// The names of the presets in the library, in file order.
    pub fn names() -> impl Iterator<Item = &'static str> {
        LIBRARY.iter().map(|(name, _)| *name)
    }

    /// The library preset called `name`, parsed and validated.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] naming `name` if the library has no preset of that name, or the errors of
    /// [`RenderPreset::from_toml`].
    pub fn load(name: &str) -> Result<Self, RecipeError> {
        let (_, text) = LIBRARY.iter().find(|(n, _)| *n == name).ok_or_else(|| {
            let known: Vec<&str> = Self::names().collect();
            RecipeError::invalid(name, format!("there is no render preset of that name; the presets are {}", known.join(", ")))
        })?;
        let preset = Self::from_toml(text)?;
        if preset.name != name {
            return Err(RecipeError::invalid(name, format!("the file says it is '{}'", preset.name)));
        }
        Ok(preset)
    }

    /// Parses a preset file and validates it.
    ///
    /// # Errors
    /// [`RecipeError::Parse`] if the text is not a preset, [`RecipeError::Invalid`] for a value out of range, a light
    /// rig the library doesn't have or an expression that isn't an id.
    pub fn from_toml(text: &str) -> Result<Self, RecipeError> {
        let preset: Self = toml::from_str(text).map_err(|e| RecipeError::parse("render preset", &e))?;
        preset.validate()?;
        Ok(preset)
    }

    /// Checks every value, so a preset that loads can always be applied.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] naming the preset and the first value that is wrong.
    pub fn validate(&self) -> Result<(), RecipeError> {
        let bad = |reason: String| RecipeError::invalid(&self.name, reason);
        if self.name.is_empty() {
            return Err(bad("the preset needs a name".into()));
        }
        if let Some(camera) = &self.camera {
            if camera.framing == Framing::Free {
                return Err(bad("camera.framing must be a preset framing: face, bust, full or hands".into()));
            }
            if !((-180.0..=180.0).contains(&camera.yaw) && (-90.0..=90.0).contains(&camera.pitch)) {
                return Err(bad(format!("camera yaw {} and pitch {} must be in -180..=180 and -90..=90", camera.yaw, camera.pitch)));
            }
        }
        if let Some(rig) = &self.light_rig
            && !LightRig::names().any(|known| known == rig)
        {
            return Err(bad(format!("light_rig '{rig}' is not in the light-rig library")));
        }
        self.expression_id()?;
        if let Some(line) = &self.outline
            && !(line.min_px.is_finite() && line.min_px > 0.0)
        {
            return Err(bad(format!("outline.min_px {} must be above 0", line.min_px)));
        }
        if let Some(line) = &self.outline
            && let Some(step) = line.edge_step
        {
            if !(step.is_finite() && step > 0.0) {
                return Err(bad(format!("outline.edge_step {step} must be above 0")));
            }
            if line.min_px > EdgeLines::MAX_WIDTH_PX {
                return Err(bad(format!(
                    "outline.min_px {} must be at most {} when the line also draws at depth edges",
                    line.min_px,
                    EdgeLines::MAX_WIDTH_PX
                )));
            }
        }
        if let Some(iod) = self.min_iod_px
            && !(iod.is_finite() && iod > 0.0)
        {
            return Err(bad(format!("min_iod_px {iod} must be above 0")));
        }
        Ok(())
    }

    /// The expression the preset shows: `None` for none fixed, `Some(None)` for the face at rest ([`NEUTRAL`]),
    /// `Some(Some(id))` for an expression of the face profile.
    ///
    /// # Errors
    /// [`RecipeError::Invalid`] if `expression` is neither [`NEUTRAL`] nor an id.
    pub fn expression_id(&self) -> Result<Option<Option<ExpressionId>>, RecipeError> {
        match self.expression.as_deref() {
            None => Ok(None),
            Some(NEUTRAL) => Ok(Some(None)),
            Some(id) => ExpressionId::new(id)
                .map(|id| Some(Some(id)))
                .map_err(|e| RecipeError::invalid(&self.name, format!("expression '{id}' is not an id: {e}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_library_preset_loads_under_its_own_name() {
        for name in RenderPreset::names() {
            let preset = RenderPreset::load(name).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(preset.name, name);
        }
    }

    #[test]
    fn the_measure_preset_reads_as_design_d10_spells_it() {
        let preset = RenderPreset::load("measure").expect("measure loads");
        let camera = preset.camera.expect("a camera");
        assert_eq!((camera.framing, camera.projection), (Framing::Face, PresetProjection::Orthographic));
        assert_eq!(preset.light_rig.as_deref(), Some("measure-flat"));
        assert_eq!(preset.expression_id().expect("an expression"), Some(None), "the neutral face");
        assert_eq!(preset.post, PresetPost::Off);
        let line = preset.outline.expect("a line");
        assert_eq!(line.colour, Colour::hex(0x1a_1a_1a));
        assert!((line.min_px - 1.5).abs() < f32::EPSILON);
        assert_eq!(preset.min_iod_px, Some(160.0));
    }

    #[test]
    fn an_unknown_preset_lists_the_known_ones_and_bad_values_are_refused() {
        let error = RenderPreset::load("nope").expect_err("no such preset").to_string();
        assert!(error.contains("measure"), "{error}");
        let bad = |extra: &str| RenderPreset::from_toml(&format!("name = \"x\"\nsummary = \"s\"\n{extra}"));
        assert!(bad("").is_ok(), "a preset may fix nothing");
        assert!(bad("light_rig = \"no-rig\"").is_err());
        assert!(bad("expression = \"Not An Id\"").is_err());
        assert!(bad("outline = { colour = \"#000000\", min_px = 0 }").is_err());
        assert!(bad("camera = { framing = \"free\" }").is_err());
        assert!(bad("lens = 3").is_err(), "unknown keys fail loudly");
    }
}
