//! Scene settings (design D2): `scene.toml`: the subject, the opening camera, the backdrop, the ground, the post look.

pub use omuzaki_render_api::BackdropFit;
use omuzaki_render_api::{ColourGrade, Glare, Stylize, Tonemap};
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
    /// Bokeh: out-of-focus highlights drawn as discs of their blur, `0..=8` (0 off, 1 keeps a light's energy, more makes
    /// them glow).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub bokeh: f32,
    /// The linear luminance above which a pixel is a highlight for `bokeh`, `>= 0`; just under white when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bokeh_threshold: Option<f32>,
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's skip_serializing_if takes a reference
fn is_zero(v: &f32) -> bool {
    *v == 0.0
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
    /// Lens glare from the brightest lights (streaks, a halo, ghosts), if wanted. It rides the bloom switch of the
    /// render quality but not `bloom` above: a scene may have glare without glow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub glare: Option<Glare>,
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
            glare: None,
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

/// Squash and stretch (#animated motion): the cast's bodies stretch along the up axis as they move up or down and
/// squash when that motion stops or turns (a landing, the push of a hop), keeping their volume, as a cartoon body
/// does. The weight is a damped spring on the hips: it lags behind a sudden move and bounces after it, so an impact
/// squashes and then settles. Drawn only: the rig, the physics and the face see the body as posed. Off (both gains 0)
/// by default.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Squash {
    /// How much the body stretches for each metre per second it moves up or down: `0.1` stretches it 10% at 1 m/s.
    /// `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub stretch: f32,
    /// How much it squashes for each metre per second of the weight's lag behind the hips (a sudden stop): the
    /// landing and the push-off. `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub squash: f32,
    /// How fast the weight bounces back after an impact, hertz. `0.5..=12`.
    #[schemars(range(min = 0.5, max = 12.0))]
    pub bounce_hz: f32,
    /// How quickly the bounce dies: `1` settles with no wobble, `0.1` jiggles. `0.05..=1`.
    #[schemars(range(min = 0.05, max = 1.0))]
    pub damping: f32,
    /// The most the height changes either way, as a share of it. `0..=0.6`.
    #[schemars(range(min = 0.0, max = 0.6))]
    pub limit: f32,
}

impl Squash {
    /// No squash and stretch.
    pub const OFF: Self = Self { stretch: 0.0, squash: 0.0, bounce_hz: 3.0, damping: 0.35, limit: 0.25 };

    /// True when nothing deforms.
    pub fn is_off(&self) -> bool {
        (self.stretch <= 0.0 && self.squash <= 0.0) || self.limit <= 0.0
    }
}

impl Default for Squash {
    fn default() -> Self {
        Self::OFF
    }
}

/// Smear frames (#animated motion): a fast move is drawn as one streaked shape, the way animated films draw a swing,
/// instead of a sharp pose. Each body part is drawn between where it is and where it was `length_s` ago: the side
/// facing away from the motion is pulled back toward the earlier pose, so a hand that whips across the frame leaves a
/// streak behind it, and anything moving slower than `min_speed` stays crisp. Drawn only, like squash and stretch.
/// Off (`amount` 0) by default.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Smears {
    /// How far back the streak reaches: `1` to where the part was `length_s` ago, `2` as far again. `0..=2`.
    #[schemars(range(min = 0.0, max = 2.0))]
    pub amount: f32,
    /// How far back in time the earlier pose is taken, seconds: longer streaks a slower move. `0.01..=0.25`.
    #[schemars(range(min = 0.01, max = 0.25))]
    pub length_s: f32,
    /// The speed a part must pass before it smears, metres per second; it fades in up to twice that. `0..=10`.
    #[schemars(range(min = 0.0, max = 10.0))]
    pub min_speed: f32,
    /// How ragged the streak's edge is: `0` one clean shape, `1` uneven fingers. `0..=1`.
    #[schemars(range(min = 0.0, max = 1.0))]
    pub jag: f32,
}

impl Smears {
    /// No smears.
    pub const OFF: Self = Self { amount: 0.0, length_s: 0.05, min_speed: 1.0, jag: 0.5 };

    /// True when nothing smears.
    pub fn is_off(&self) -> bool {
        self.amount <= 0.0
    }
}

impl Default for Smears {
    fn default() -> Self {
        Self::OFF
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
    /// Squash and stretch of the cast's bodies.
    #[serde(default, skip_serializing_if = "Squash::is_off")]
    pub squash: Squash,
    /// Smear frames on the cast's fast moves.
    #[serde(default, skip_serializing_if = "Smears::is_off")]
    pub smears: Smears,
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
            squash: Squash::OFF,
            smears: Smears::OFF,
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
    fn squash_is_left_out_while_off_and_reads_with_defaults() {
        let s = SceneSettings::named("Main");
        assert!(serde_json::to_value(&s).unwrap().get("squash").is_none());
        let on: SceneSettings = serde_json::from_value(json!({"name": "Main", "squash": {"stretch": 0.1}})).unwrap();
        assert_eq!(on.squash, Squash { stretch: 0.1, ..Squash::OFF });
        assert!(!on.squash.is_off());
        assert!(serde_json::from_value::<Squash>(json!({"wobble": 1})).is_err());
        assert!(serde_json::to_value(&s).unwrap().get("smears").is_none());
        let smeared: SceneSettings =
            serde_json::from_value(json!({"name": "Main", "smears": {"amount": 1.0}})).unwrap();
        assert_eq!(smeared.smears, Smears { amount: 1.0, ..Smears::OFF });
    }

    #[test]
    fn focus_reads_as_the_word_subject_or_metres() {
        let on: Focus = serde_json::from_value(json!("subject")).unwrap();
        assert_eq!(on, Focus::Subject(FocusSubject::Subject));
        assert_eq!(serde_json::from_value::<Focus>(json!(1.5)).unwrap(), Focus::Metres(1.5));
        assert!(serde_json::from_value::<Focus>(json!("far")).is_err());
    }
}
