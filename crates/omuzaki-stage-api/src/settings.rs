//! Scene settings (design D2): `scene.toml`: the subject, the opening camera, the backdrop, the ground, the post look.

pub use omuzaki_render_api::BackdropFit;
use omuzaki_render_api::{ColourGrade, Stylize, Tonemap};
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::{CameraId, MemberId};

/// What shows behind the cast.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Backdrop {
    /// The lighting's `background` colour, so a rig brings its own mood.
    // Struct variants (not unit ones) so `deny_unknown_fields` also refuses a stray `colour` on `lighting`.
    Lighting {},
    /// A colour of the scene's own (a green screen is `#00b140` with the ground off), shown as authored: the exposure
    /// and tone curve don't touch it, so a key matches it exactly.
    Colour {
        /// The colour.
        colour: omuzaki_render_api::Colour,
    },
    /// Nothing: alpha, for OBS and Spout (BR-01).
    Transparent {},
    /// The lighting's environment (phase 2, with LK-08): the sky behind the cast, turned and as bright as it lights
    /// them. Without an environment in the lighting it shows the lighting's `background` colour, and the check says so.
    Environment {},
    /// A vertical gradient from `top` to `bottom` (phase 2), shown as authored: the exposure and tone curve don't touch
    /// it.
    Gradient {
        /// The colour along the frame's top edge.
        top: omuzaki_render_api::Colour,
        /// The colour along its bottom edge.
        bottom: omuzaki_render_api::Colour,
    },
    /// A picture from the asset store (phase 2): a PNG or JPEG whose provenance and licence the store keeps, shown as
    /// authored. `fit` places it; where `contain` leaves bars, `bars` shows (the lighting's `background` colour when
    /// left out).
    Image {
        /// The picture's content hash in the asset store.
        asset: ContentHash,
        /// How it meets a frame of another shape.
        #[serde(default)]
        fit: BackdropFit,
        /// What shows in the bars `contain` leaves.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        bars: Option<Bars>,
    },
}

/// The gradient in the bars around an image backdrop; give both ends one colour for flat bars.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Bars {
    /// The colour at the frame's top edge.
    pub top: omuzaki_render_api::Colour,
    /// The colour at its bottom edge.
    pub bottom: omuzaki_render_api::Colour,
}

impl Default for Backdrop {
    fn default() -> Self {
        Self::Lighting {}
    }
}

/// What the ground is.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum GroundKind {
    /// A shadow-catcher disc: only the cast's shadow shows.
    #[default]
    Shadow,
    /// A lit stage floor: a disc of the ground's `colour` that the lights fall on, so spots leave pools on it and the
    /// cast stands in its own shadow.
    Floor,
    /// No ground.
    None,
}

/// The ground under the cast.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ground {
    /// Which ground.
    #[serde(default)]
    pub kind: GroundKind,
    /// How dark the shadow is, `0..=1`.
    #[serde(default = "default_opacity")]
    pub opacity: f32,
    /// In subject units (design D4). Absent: fits the cast plus 0.83.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// A `floor`'s colour. Absent: [`Ground::FLOOR`], a dark stage grey.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<omuzaki_render_api::Colour>,
}

impl Ground {
    /// A floor's colour when the ground names none: a dark stage grey, which a dark house leaves near black and a spot
    /// lights into a pool of its colour.
    pub const FLOOR: omuzaki_render_api::Colour = omuzaki_render_api::Colour::hex(0x007a_7a7a);
}

fn default_opacity() -> f32 {
    0.25
}

impl Default for Ground {
    fn default() -> Self {
        Self { kind: GroundKind::Shadow, opacity: default_opacity(), radius: None, colour: None }
    }
}

/// Where depth of field focuses.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Focus {
    /// The subject's aim point.
    Subject(FocusSubject),
    /// A distance from the camera, in metres.
    Metres(f32),
}

/// The word `subject`, so [`Focus`] reads `"subject"` or a number.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FocusSubject {
    /// Focus on the subject.
    Subject,
}

/// Depth of field as authored: `post.depth_of_field`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DepthOfFieldDef {
    /// What is sharp.
    pub focus: Focus,
    /// Depth of the sharp zone, in subject units, `>= 0`.
    pub range: f32,
    /// The strongest blur, as a fraction of the image height, `>= 0`.
    pub max_blur: f32,
}

/// A colour grade as authored: the render-api [`ColourGrade`] with a strict reader (a misspelt key is refused).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GradeDef {
    /// 1 leaves colour alone, 0 is greyscale, above 1 is more vivid.
    pub saturation: f32,
    /// 1 leaves contrast alone; above 1 steepens the curve around mid grey.
    pub contrast: f32,
}

/// Ambient occlusion and contact shadows as authored: `post.occlusion` (render-api `Occlusion`). Creases and the
/// places where surfaces meet darken, and small casters (a fringe on the forehead, lashes on the eye) shadow what is
/// just under them, finer than a shadow map can.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct OcclusionDef {
    /// How far round each point occluders are looked for, metres, `> 0`: about the size of the folds that darken.
    pub radius: f32,
    /// How dark full occlusion is, `0..=1`.
    pub strength: f32,
    /// How far toward each shadow-casting light a point looks for a blocker, metres, `>= 0` (`0`: no contact
    /// shadows).
    pub contact: f32,
}

impl Default for OcclusionDef {
    /// Tuned for a whole character in a close-up: 5 mm folds and contact shadows reaching 1.5 cm, so a fringe shadows
    /// the forehead.
    fn default() -> Self {
        Self { radius: 0.005, strength: 0.6, contact: 0.015 }
    }
}

impl From<GradeDef> for ColourGrade {
    fn from(g: GradeDef) -> Self {
        Self { saturation: g.saturation, contrast: g.contrast }
    }
}

impl From<ColourGrade> for GradeDef {
    fn from(g: ColourGrade) -> Self {
        Self { saturation: g.saturation, contrast: g.contrast }
    }
}

/// The look of the frame the author asks for (render-api D6). `Quality` still decides what runs.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PostDef {
    /// Switches the lighting's bloom on or off.
    #[serde(default = "yes")]
    pub bloom: bool,
    /// How light maps to the display.
    #[serde(default)]
    pub tonemap: Tonemap,
    /// Depth of field, if wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub depth_of_field: Option<DepthOfFieldDef>,
    /// Film grain strength, `0..=1`, if wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grain: Option<f32>,
    /// Colour grade, if wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grade: Option<GradeDef>,
    /// Ambient occlusion and contact shadows, if wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub occlusion: Option<OcclusionDef>,
    /// The drawn look laid over the frame (halftone, hatching, posterize, paper, vignette, fringe, split toning), if
    /// wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stylize: Option<Stylize>,
}

fn yes() -> bool {
    true
}

impl Default for PostDef {
    fn default() -> Self {
        Self {
            bloom: true,
            tonemap: Tonemap::Aces,
            depth_of_field: None,
            grain: None,
            grade: None,
            occlusion: None,
            stylize: None,
        }
    }
}

/// How the cast moves in time (#animated timing): every frame, the default, or held as drawn animation is, on twos or
/// threes. Only the poses are held: the camera, the lights, the effects and the render run at the frame rate, so a
/// held pose under a moving camera reads as drawn, not as a dropped frame.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Timing {
    /// Body poses per second: `0` moves every frame; `12` is "on twos" and `8` "on threes" at 24 fps.
    /// `0..=60`.
    #[schemars(range(min = 0.0, max = 60.0))]
    pub body_hz: f32,
    /// Face updates (expressions, eyes, mouth) per second, held the same way: `0` every frame. Speech reads best at 12
    /// or more. `0..=60`.
    #[schemars(range(min = 0.0, max = 60.0))]
    pub face_hz: f32,
    /// Whether hair, cloth and other secondary motion hold with the body (the drawn look; they catch up in one step
    /// at each new pose) or keep moving between the held poses.
    pub hold_secondary: bool,
}

impl Timing {
    /// Every frame: no holds.
    pub const SMOOTH: Self = Self { body_hz: 0.0, face_hz: 0.0, hold_secondary: true };

    /// True when nothing is held.
    pub fn is_smooth(&self) -> bool {
        self.body_hz <= 0.0 && self.face_hz <= 0.0
    }

    /// Which held pose a frame at `time` seconds shows, for a rate of `hz` (`None` when the rate is 0: every frame).
    /// Frames in the same slot show the same pose.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // floored and clamped to >= 0
    pub fn slot(time: f64, hz: f32) -> Option<u64> {
        // A hair over the boundary, so a frame exactly on it (time 0.5 at 12 Hz) starts the new pose.
        (hz > 0.0).then(|| (time.max(0.0) * f64::from(hz) + 1e-6).floor() as u64)
    }
}

impl Default for Timing {
    fn default() -> Self {
        Self::SMOOTH
    }
}

/// `scene.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SceneSettings {
    /// Display name (free text). The id never changes; this does.
    pub name: String,
    /// The member framings and subject-anchored lights follow. Unset: the first member by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<MemberId>,
    /// The camera the scene opens with. Unset: the first camera by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<CameraId>,
    /// What shows behind the cast.
    #[serde(default)]
    pub backdrop: Backdrop,
    /// The ground.
    #[serde(default)]
    pub ground: Ground,
    /// The post look.
    #[serde(default)]
    pub post: PostDef,
    /// How the cast moves in time: every frame, or held on twos or threes.
    #[serde(default, skip_serializing_if = "Timing::is_smooth")]
    pub timing: Timing,
}

impl SceneSettings {
    /// Settings with a name and every default.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            subject: None,
            camera: None,
            backdrop: Backdrop::default(),
            ground: Ground::default(),
            post: PostDef::default(),
            timing: Timing::SMOOTH,
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_bare_scene_reads_with_every_default() {
        let s: SceneSettings = serde_json::from_value(json!({"name": "Main"})).unwrap();
        assert_eq!(s, SceneSettings::named("Main"));
    }

    #[test]
    fn backdrops_are_tagged_by_kind() {
        let b: Backdrop = serde_json::from_value(json!({"kind": "colour", "colour": "#00b140"})).unwrap();
        assert!(matches!(b, Backdrop::Colour { .. }));
        assert_eq!(serde_json::to_value(Backdrop::Transparent {}).unwrap(), json!({"kind": "transparent"}));
        assert!(serde_json::from_value::<Backdrop>(json!({"kind": "gradient"})).is_err(), "a gradient has two ends");
        let g: Backdrop =
            serde_json::from_value(json!({"kind": "gradient", "top": "#102040", "bottom": "#000000"})).unwrap();
        assert!(matches!(g, Backdrop::Gradient { .. }));
        let hash = "ab".repeat(32);
        let image: Backdrop = serde_json::from_value(json!({"kind": "image", "asset": hash})).unwrap();
        assert!(matches!(image, Backdrop::Image { fit: BackdropFit::Cover, bars: None, .. }), "cover, no bars");
        assert_eq!(serde_json::to_value(&image).unwrap(), json!({"kind": "image", "asset": hash, "fit": "cover"}));
        let wrong = json!({"kind": "image", "asset": hash, "bars": {"top": "#000000"}});
        assert!(serde_json::from_value::<Backdrop>(wrong).is_err(), "bars have two ends");
        assert_eq!(
            serde_json::from_value::<Backdrop>(json!({"kind": "environment"})).unwrap(),
            Backdrop::Environment {}
        );
        assert!(serde_json::from_value::<Backdrop>(json!({"kind": "lighting", "colour": "#fff"})).is_err());
    }

    #[test]
    fn focus_reads_as_the_word_subject_or_metres() {
        let on: Focus = serde_json::from_value(json!("subject")).unwrap();
        assert_eq!(on, Focus::Subject(FocusSubject::Subject));
        assert_eq!(serde_json::from_value::<Focus>(json!(1.5)).unwrap(), Focus::Metres(1.5));
        assert!(serde_json::from_value::<Focus>(json!("far")).is_err());
    }
}
