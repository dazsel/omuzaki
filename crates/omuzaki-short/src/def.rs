//! The short file (`*.short.toml`): a script as lines, each with a face, a shot and a caption.
//!
//! A short is written for people and agents alike: a line is its text plus a few words of direction.
//!
//! ```toml
//! character = "lumi"
//!
//! [voice]
//! kind = "silent"
//!
//! [[line]]
//! text = "German is not an aggressive language!"
//! face = ["flustered", "angry"]
//! camera = "close+tilt-left+punch"
//! caption = "shout"
//! ```
//!
//! Faces are expressions of the character's face profile, layered in order; `channels` sets single channels on top.
//! A camera is a shot preset with modifiers joined by `+` ([`ShotCue`]), or a table of its numbers. A line without a
//! face or a camera keeps the one before it, so a short only says what changes.

use std::collections::BTreeMap;

use omuzaki_char_api::CharId;
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::voice::{SPEED_MAX, SPEED_MIN};
use omuzaki_render_api::Colour;
use omuzaki_stage::ops::render::pose::PoseArg;
use omuzaki_stage_api::SceneId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ShortError;

/// The most lines a short holds: about ten minutes of speech at a line every two seconds.
pub const MAX_LINES: usize = 300;
/// The longest a short may run, in seconds.
pub const MAX_SECONDS: f32 = 600.0;
/// The longest line text, in characters.
pub const MAX_TEXT: usize = 400;
/// The smallest and largest frame rate.
pub const FPS_RANGE: (u32, u32) = (12, 60);
/// The smallest and largest side of the picture, in pixels.
pub const SIDE_RANGE: (u32, u32) = (64, 4096);

fn default_size() -> [u32; 2] {
    [1080, 1920]
}

const fn default_fps() -> u32 {
    30
}

const fn default_lead() -> f32 {
    0.3
}

const fn default_tail() -> f32 {
    0.8
}

const fn default_pause() -> f32 {
    0.25
}

/// A short: who performs, how it looks and sounds, and the lines.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShortDef {
    /// What the short is called (for people; nothing reads it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// The character who performs it: built from the kit or imported. With `puppet`, it names the performer (its voice
    /// and the video's default name) and needn't be in the project.
    pub character: CharId,
    /// A flat 2D puppet to perform it in place of the character's 3D body (EX-08), relative to the project: a puppet
    /// bundle folder (`puppets/pip.puppet`, holding `puppet.json`) or layered art (`art/pip.layers.toml`), which is
    /// rigged on the spot. The scene, light rig and pose don't apply to a puppet; `look.backdrop` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub puppet: Option<String>,
    /// The scene it is shot in; the project's opening scene when left out. The short's `look` is laid over it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<SceneId>,
    /// Width and height in pixels: 1080x1920 (9:16 for phones) by default.
    #[serde(default = "default_size")]
    pub size: [u32; 2],
    /// Frames per second.
    #[serde(default = "default_fps")]
    pub fps: u32,
    /// Seeds the life layer and the handheld camera, so a short renders the same on every run.
    #[serde(default)]
    pub seed: u64,
    /// Seconds before the first line.
    #[serde(default = "default_lead")]
    pub lead: f32,
    /// Seconds after the last line.
    #[serde(default = "default_tail")]
    pub tail: f32,
    /// The light and backdrop.
    #[serde(default)]
    pub look: LookDef,
    /// Where the voice comes from.
    #[serde(default)]
    pub voice: VoiceDef,
    /// How captions are drawn.
    #[serde(default)]
    pub captions: CaptionsDef,
    /// The lines, in order.
    #[serde(rename = "line", default)]
    pub lines: Vec<LineDef>,
    /// Faces and shots placed at a moment of the short rather than on a line's words (`[[cue]]`), in any order: a
    /// cut timed to a frame, or a face held through a pause. A cue wins over a line's face or shot that lands at the
    /// same moment, and holds until the next change from either.
    #[serde(rename = "cue", default, skip_serializing_if = "Vec::is_empty")]
    pub cues: Vec<CueDef>,
    /// How much the handheld camera drifts: 1 is a person holding a phone (the default), 0 a locked-off camera, up
    /// to [`MAX_HANDHELD`]. A shot's `shake` comes on top.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub handheld: f32,
}

/// The most handheld drift a short may ask for.
pub const MAX_HANDHELD: f32 = 4.0;

#[allow(clippy::trivially_copy_pass_by_ref, reason = "serde's skip_serializing_if takes a reference")]
fn is_one(v: &f32) -> bool {
    (*v - 1.0).abs() < f32::EPSILON
}

/// A cue: a face, a shot or both, at a moment of the short.
///
/// ```toml
/// [[cue]]
/// at = 2.6
/// face = "stare+gloom"
/// camera = { zoom = 1.3, roll = -6.0 }
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CueDef {
    /// When it lands, in seconds from the start of the short.
    pub at: f32,
    /// The face from then on: one expression or several, layered in order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<FaceCue>,
    /// Channels set on top of the face, by name (`angerVein = 1.0`). With no `face`, they go over the face in force.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, f32>,
    /// The shot from then on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ShotCue>,
    /// How long a shot's push in takes, seconds; until the next cue or line that changes the shot when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hold: Option<f32>,
}

/// The light and backdrop of a short, over its scene.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LookDef {
    /// A light rig from `stage.light_rigs`; `short-rim` (black, a warm rim on the hair) by default. `scene` keeps the
    /// scene's own lighting.
    #[serde(default = "default_rig")]
    pub light_rig: String,
    /// The backdrop colour; the light rig's background when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<Colour>,
    /// The pose the character performs from: a pose of the pose library (`pose.list`); `stand-relaxed` (arms down,
    /// at ease) by default.
    #[serde(default = "default_pose")]
    pub pose: PoseArg,
}

fn default_rig() -> String {
    "short-rim".to_owned()
}

/// The pose a short performs from when the file names none.
pub const DEFAULT_POSE: &str = "stand-relaxed";

fn default_pose() -> PoseArg {
    PoseArg { id: DEFAULT_POSE.to_owned(), mirror: false, blend: None }
}

impl Default for LookDef {
    fn default() -> Self {
        Self { light_rig: default_rig(), backdrop: None, pose: default_pose() }
    }
}

/// Where a short's voice comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceDef {
    /// No sound: each line lasts as long as it takes to say at `words_per_minute`, and the mouth moves with the text.
    /// For drafts, previews and silent shorts.
    Silent {
        /// Speaking rate.
        #[serde(default = "default_wpm")]
        words_per_minute: f32,
    },
    /// A recording of the whole script (WAV, any rate, mono or stereo), relative to the project. Lines are matched to
    /// the recording's spoken stretches in order, unless a line gives its own `at`; the mouth moves with the sound.
    File {
        /// The WAV file, relative to the project folder.
        path: String,
    },
    /// Said by the project's local text-to-speech (Kokoro, `tts@1`): each line is synthesised in `voice`, lasts as long
    /// as it takes to say (unless it gives its own `duration`), and the mouth moves with the sound. `short.plan` only
    /// estimates the timing from the words; `short.render` synthesises.
    Speech {
        /// The voice, `<model>/<voice>` as `voice.list` names it (`kokoro-82m/af_heart`).
        voice: String,
        /// Tempo: 1 is the voice's own pace.
        #[serde(default = "one")]
        speed: f32,
    },
}

const fn default_wpm() -> f32 {
    170.0
}

impl VoiceDef {
    /// Checks the voice's numbers and names.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] naming the field and the rule.
    fn check(&self) -> Result<(), ShortError> {
        match self {
            Self::Silent { words_per_minute } if !(60.0..=400.0).contains(words_per_minute) => {
                Err(ShortError::invalid("voice.words_per_minute", format!("{words_per_minute} is outside 60 to 400")))
            }
            Self::Speech { voice, speed } => {
                VoiceId::new(voice).map_err(|e| {
                    ShortError::invalid("voice.voice", format!("{voice:?} isn't a voice id: {} (voice.list)", e.rule))
                })?;
                if (SPEED_MIN..=SPEED_MAX).contains(speed) {
                    Ok(())
                } else {
                    Err(ShortError::invalid("voice.speed", format!("{speed} is outside {SPEED_MIN} to {SPEED_MAX}")))
                }
            }
            _ => Ok(()),
        }
    }
}

impl Default for VoiceDef {
    fn default() -> Self {
        Self::Silent { words_per_minute: default_wpm() }
    }
}

/// How captions are drawn.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CaptionsDef {
    /// Whether lines get captions at all.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// Where the caption's middle sits, as a share of the frame height from the top.
    #[serde(default = "default_caption_y")]
    pub y: f32,
    /// The normal caption's letter height, as a share of the frame width.
    #[serde(default = "default_caption_size")]
    pub size: f32,
    /// The most characters on a caption before a phrase is split.
    #[serde(default = "default_caption_chars")]
    pub max_chars: usize,
}

const fn yes() -> bool {
    true
}

const fn default_caption_y() -> f32 {
    0.64
}

const fn default_caption_size() -> f32 {
    0.072
}

const fn default_caption_chars() -> usize {
    24
}

impl Default for CaptionsDef {
    fn default() -> Self {
        Self { enabled: true, y: default_caption_y(), size: default_caption_size(), max_chars: default_caption_chars() }
    }
}

/// How a line's caption looks.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptionStyle {
    /// White bold with a dark outline.
    #[default]
    Normal,
    /// Larger and in capitals, for shouted words.
    Shout,
    /// Smaller, for asides.
    Aside,
    /// One or two words as large as the frame allows.
    Big,
    /// No caption for this line.
    None,
}

/// One line of the script.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LineDef {
    /// What is said. A change inside the line goes in braces before the word it lands on: `{surprised}` changes the
    /// face, `{@close+punch}` the shot, `{surprised @close+punch}` both. The braces are not said or captioned.
    pub text: String,
    /// The face for the line: one expression or several layered in order; the line before's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face: Option<FaceCue>,
    /// Channels set on top of the face, by name (`angerVein = 1.0`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub channels: BTreeMap<String, f32>,
    /// The shot for the line; the line before's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera: Option<ShotCue>,
    /// How the caption looks.
    #[serde(default)]
    pub caption: CaptionStyle,
    /// The caption's text when it differs from what is said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption_text: Option<String>,
    /// Seconds of quiet after the line.
    #[serde(default = "default_pause")]
    pub pause: f32,
    /// When the line starts, in seconds from the start of the short; after the line before when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<f32>,
    /// How long the line is spoken, in seconds; from the voice when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<f32>,
}

impl LineDef {
    /// A line saying `text`, with no direction.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            face: None,
            channels: BTreeMap::new(),
            camera: None,
            caption: CaptionStyle::Normal,
            caption_text: None,
            pause: default_pause(),
            at: None,
            duration: None,
        }
    }
}

/// A change inside a line: a face, a shot or both, landing a share of the way through the line's words.
#[derive(Clone, Debug, PartialEq)]
pub struct Beat {
    /// How far through the line it lands: the share of the spoken characters before it, `0..1`.
    pub at: f32,
    /// The new face, if it changes.
    pub face: Option<FaceCue>,
    /// The new shot, if it changes.
    pub camera: Option<ShotCue>,
}

impl LineDef {
    /// What is said: the text without its beats, spaces collapsed.
    #[must_use]
    pub fn spoken(&self) -> String {
        let mut out = String::new();
        let mut depth = 0usize;
        for c in self.text.chars() {
            match c {
                '{' => depth += 1,
                '}' => depth = depth.saturating_sub(1),
                c if depth == 0 => out.push(c),
                _ => {}
            }
        }
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The beats in the text, in order.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] for an unclosed or empty brace, or a shot whose words don't read.
    #[allow(clippy::cast_precision_loss, reason = "character counts")]
    pub fn beats(&self) -> Result<Vec<Beat>, ShortError> {
        let total = self.spoken().chars().count().max(1) as f32;
        let mut beats = Vec::new();
        let mut rest = self.text.as_str();
        let mut before = String::new();
        while let Some(open) = rest.find('{') {
            before.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            let close = after.find('}').ok_or_else(|| ShortError::invalid("text", "a `{` with no `}`"))?;
            let inside = after[..close].trim();
            if inside.is_empty() {
                return Err(ShortError::invalid("text", "an empty `{}`: name a face, or a shot after `@`"));
            }
            let (mut faces, mut shots) = (Vec::new(), Vec::new());
            for token in inside.split_whitespace() {
                match token.strip_prefix('@') {
                    Some(shot) => shots.push(shot.to_owned()),
                    None => faces.push(token.to_owned()),
                }
            }
            let camera = (!shots.is_empty()).then(|| ShotCue::Words(shots.join("+")));
            if let Some(camera) = &camera {
                camera.numbers()?;
            }
            let said = before.split_whitespace().collect::<Vec<_>>().join(" ");
            let chars = said.chars().count() + usize::from(!said.is_empty());
            beats.push(Beat {
                at: (chars as f32 / total).min(1.0),
                face: (!faces.is_empty()).then(|| FaceCue::One(faces.join("+"))),
                camera,
            });
            rest = &after[close + 1..];
        }
        Ok(beats)
    }
}

/// A face: one expression id or several, layered in order.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum FaceCue {
    /// One expression.
    One(String),
    /// Several, layered in order.
    Many(Vec<String>),
}

impl FaceCue {
    /// The expression ids, in order. `"a+b"` in a single string names two.
    #[must_use]
    pub fn ids(&self) -> Vec<String> {
        let split =
            |s: &str| s.split('+').map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned).collect::<Vec<_>>();
        match self {
            Self::One(s) => split(s),
            Self::Many(all) => all.iter().flat_map(|s| split(s)).collect(),
        }
    }
}

/// A shot: a preset with modifiers (`"close+tilt-left+punch"`), or its numbers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum ShotCue {
    /// Words: a framing (`face`, `close`, `eyes`, `mouth`, `bust`, `wide`) and modifiers (`tilt-left`, `tilt-right`,
    /// `low`, `high`, `left`, `right`, `punch`, `shake`, `push`, `drift`), joined by `+`.
    Words(String),
    /// The numbers.
    Numbers(ShotNumbers),
}

/// A shot as numbers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ShotNumbers {
    /// `face` or `bust`.
    #[serde(default)]
    pub framing: ShotFraming,
    /// Degrees around the character: + toward screen right.
    #[serde(default)]
    pub yaw: f32,
    /// Degrees: + from above.
    #[serde(default)]
    pub pitch: f32,
    /// Dutch tilt in degrees: + leans the picture clockwise.
    #[serde(default)]
    pub roll: f32,
    /// Above 1 moves in.
    #[serde(default = "one")]
    pub zoom: f32,
    /// The subject's offset in the frame, as shares of its width and height (+ moves it right and up).
    #[serde(default)]
    pub shift: [f32; 2],
    /// A punch-in at the start of the line: the zoom grows by this factor over [`PUNCH_S`](crate::camera::PUNCH_S).
    #[serde(default)]
    pub punch: f32,
    /// A slow push in over the line: the zoom grows by this factor by the line's end.
    #[serde(default)]
    pub push: f32,
    /// Handheld shake, 0 (steady) to 1 (a jolt).
    #[serde(default)]
    pub shake: f32,
    /// Seconds to ease from the shot before; 0 cuts.
    #[serde(default)]
    pub ease: f32,
    /// How wide the head (hair, ears and hats included) is across the frame, as a share of its width: 0.8 fills most
    /// of the frame with it. The framing's own size when left out. `zoom`, `punch` and `push` scale it from there, so
    /// a punch-in still lands. A 3D character's; a puppet keeps its framing's size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_width: Option<f32>,
}

const fn one() -> f32 {
    1.0
}

/// What a shot frames.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShotFraming {
    /// Chin to crown.
    #[default]
    Face,
    /// Chest to crown.
    Bust,
}

impl Default for ShotNumbers {
    fn default() -> Self {
        Self {
            framing: ShotFraming::Face,
            yaw: 0.0,
            pitch: 0.0,
            roll: 0.0,
            zoom: 1.0,
            shift: [0.0, 0.0],
            punch: 0.0,
            push: 0.0,
            shake: 0.0,
            ease: 0.0,
            head_width: None,
        }
    }
}

impl ShotCue {
    /// The shot's numbers: the words read as a preset and its modifiers.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] naming the unknown word and the words there are.
    pub fn numbers(&self) -> Result<ShotNumbers, ShortError> {
        match self {
            Self::Numbers(n) => Ok(*n),
            Self::Words(words) => shot_words(words),
        }
    }
}

/// The framing words and the modifiers a shot's words may use, for the error that lists them.
pub const SHOT_WORDS: [&str; 16] = [
    "face",
    "close",
    "eyes",
    "mouth",
    "bust",
    "wide",
    "tilt-left",
    "tilt-right",
    "low",
    "high",
    "left",
    "right",
    "punch",
    "shake",
    "push",
    "drift",
];

fn shot_words(words: &str) -> Result<ShotNumbers, ShortError> {
    let mut n = ShotNumbers::default();
    for word in words.split('+').map(str::trim).filter(|w| !w.is_empty()) {
        match word {
            "face" => {}
            "close" => n.zoom *= 1.35,
            "eyes" => {
                n.zoom *= 1.9;
                n.shift[1] -= 0.04;
            }
            "mouth" => {
                n.zoom *= 2.6;
                n.shift[1] += 0.42;
            }
            "bust" => n.framing = ShotFraming::Bust,
            "wide" => {
                n.framing = ShotFraming::Bust;
                n.zoom *= 0.8;
            }
            "tilt-left" => n.roll -= 9.0,
            "tilt-right" => n.roll += 9.0,
            "low" => n.pitch -= 16.0,
            "high" => n.pitch += 14.0,
            "left" => n.yaw -= 22.0,
            "right" => n.yaw += 22.0,
            "punch" => n.punch = n.punch.max(1.18),
            "shake" => n.shake = n.shake.max(0.6),
            "push" => n.push = n.push.max(1.12),
            "drift" => n.ease = n.ease.max(0.6),
            other => {
                return Err(ShortError::invalid(
                    "camera",
                    format!("'{other}' is not a shot word; the words are {}", SHOT_WORDS.join(", ")),
                ));
            }
        }
    }
    Ok(n)
}

impl ShortDef {
    /// A short of `character` at the defaults, with no lines yet.
    #[must_use]
    pub fn new(character: CharId) -> Self {
        Self {
            title: None,
            character,
            puppet: None,
            scene: None,
            size: default_size(),
            fps: default_fps(),
            seed: 0,
            lead: default_lead(),
            tail: default_tail(),
            look: LookDef::default(),
            voice: VoiceDef::default(),
            captions: CaptionsDef::default(),
            lines: Vec::new(),
            cues: Vec::new(),
            handheld: 1.0,
        }
    }

    /// Reads a short file.
    ///
    /// # Errors
    /// [`ShortError::Parse`] with the parser's message (line and column) for text that isn't a short file;
    /// [`ShortError::Invalid`] for one that breaks a rule ([`ShortDef::check`]).
    pub fn from_toml(text: &str) -> Result<Self, ShortError> {
        let def: Self = toml::from_str(text).map_err(|e| ShortError::Parse(e.to_string()))?;
        def.check()?;
        Ok(def)
    }

    /// The short as a short file.
    ///
    /// # Errors
    /// [`ShortError::Parse`] if TOML can't hold a value (it always can for a checked short).
    pub fn to_toml(&self) -> Result<String, ShortError> {
        toml::to_string_pretty(self).map_err(|e| ShortError::Parse(e.to_string()))
    }

    /// Checks the rules a parser can't: sizes, rates and timings in range, words and faces well formed.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] naming the field and the rule.
    pub fn check(&self) -> Result<(), ShortError> {
        for (i, side) in self.size.iter().enumerate() {
            if !(SIDE_RANGE.0..=SIDE_RANGE.1).contains(side) {
                return Err(ShortError::invalid(
                    format!("size[{i}]"),
                    format!("{side} px is outside {} to {}", SIDE_RANGE.0, SIDE_RANGE.1),
                ));
            }
            if side % 2 != 0 {
                return Err(ShortError::invalid(
                    format!("size[{i}]"),
                    format!("{side} px is odd: video needs even sides"),
                ));
            }
        }
        if !(FPS_RANGE.0..=FPS_RANGE.1).contains(&self.fps) {
            return Err(ShortError::invalid(
                "fps",
                format!("{} is outside {} to {}", self.fps, FPS_RANGE.0, FPS_RANGE.1),
            ));
        }
        for (field, value) in [("lead", self.lead), ("tail", self.tail)] {
            if !(0.0..=10.0).contains(&value) {
                return Err(ShortError::invalid(field, format!("{value} s is outside 0 to 10")));
            }
        }
        self.voice.check()?;
        if !(0.05..=0.95).contains(&self.captions.y) {
            return Err(ShortError::invalid("captions.y", format!("{} is outside 0.05 to 0.95", self.captions.y)));
        }
        if !(0.01..=0.2).contains(&self.captions.size) {
            return Err(ShortError::invalid("captions.size", format!("{} is outside 0.01 to 0.2", self.captions.size)));
        }
        if !(4..=80).contains(&self.captions.max_chars) {
            return Err(ShortError::invalid(
                "captions.max_chars",
                format!("{} is outside 4 to 80", self.captions.max_chars),
            ));
        }
        if self.lines.is_empty() {
            return Err(ShortError::invalid("line", "a short needs at least one line"));
        }
        if self.lines.len() > MAX_LINES {
            return Err(ShortError::invalid("line", format!("{} lines; at most {MAX_LINES}", self.lines.len())));
        }
        for (i, line) in self.lines.iter().enumerate() {
            check_line(i, line)?;
        }
        if !(0.0..=MAX_HANDHELD).contains(&self.handheld) {
            return Err(ShortError::invalid("handheld", format!("{} is outside 0 to {MAX_HANDHELD}", self.handheld)));
        }
        if self.cues.len() > MAX_CUES {
            return Err(ShortError::invalid("cue", format!("{} cues; at most {MAX_CUES}", self.cues.len())));
        }
        for (i, cue) in self.cues.iter().enumerate() {
            check_cue(i, cue)?;
        }
        Ok(())
    }
}

fn check_line(i: usize, line: &LineDef) -> Result<(), ShortError> {
    let field = |name: &str| format!("line[{i}].{name}");
    if line.spoken().is_empty() {
        return Err(ShortError::invalid(field("text"), "a line needs words"));
    }
    for beat in line.beats().map_err(|e| e.at(field("text")))? {
        if let Some(camera) = &beat.camera {
            check_shot(&field("text"), &camera.numbers()?)?;
        }
    }
    if line.text.chars().count() > MAX_TEXT {
        return Err(ShortError::invalid(field("text"), format!("at most {MAX_TEXT} characters")));
    }
    if !(0.0..=10.0).contains(&line.pause) {
        return Err(ShortError::invalid(field("pause"), format!("{} s is outside 0 to 10", line.pause)));
    }
    if let Some(at) = line.at
        && !(0.0..=MAX_SECONDS).contains(&at)
    {
        return Err(ShortError::invalid(field("at"), format!("{at} s is outside 0 to {MAX_SECONDS}")));
    }
    if let Some(d) = line.duration
        && !(0.1..=60.0).contains(&d)
    {
        return Err(ShortError::invalid(field("duration"), format!("{d} s is outside 0.1 to 60")));
    }
    if let Some(face) = &line.face
        && face.ids().is_empty()
    {
        return Err(ShortError::invalid(field("face"), "name at least one expression"));
    }
    for (name, value) in &line.channels {
        if !value.is_finite() {
            return Err(ShortError::invalid(field(&format!("channels.{name}")), "not a number"));
        }
    }
    if let Some(camera) = &line.camera {
        let n = camera.numbers().map_err(|e| e.at(field("camera")))?;
        check_shot(&field("camera"), &n)?;
    }
    Ok(())
}

/// The most cues a short holds: one every frame of a minute at 30 fps would be 1800; a cut a second for ten minutes
/// is 600.
pub const MAX_CUES: usize = 2000;

fn check_cue(i: usize, cue: &CueDef) -> Result<(), ShortError> {
    let field = |name: &str| format!("cue[{i}].{name}");
    if !(0.0..=MAX_SECONDS).contains(&cue.at) {
        return Err(ShortError::invalid(field("at"), format!("{} s is outside 0 to {MAX_SECONDS}", cue.at)));
    }
    if cue.face.is_none() && cue.channels.is_empty() && cue.camera.is_none() {
        return Err(ShortError::invalid(format!("cue[{i}]"), "a cue changes a face, channels or a shot"));
    }
    if let Some(face) = &cue.face
        && face.ids().is_empty()
    {
        return Err(ShortError::invalid(field("face"), "name at least one expression"));
    }
    for (name, value) in &cue.channels {
        if !value.is_finite() {
            return Err(ShortError::invalid(field(&format!("channels.{name}")), "not a number"));
        }
    }
    if let Some(hold) = cue.hold
        && !(0.05..=60.0).contains(&hold)
    {
        return Err(ShortError::invalid(field("hold"), format!("{hold} s is outside 0.05 to 60")));
    }
    if let Some(camera) = &cue.camera {
        let n = camera.numbers().map_err(|e| e.at(field("camera")))?;
        check_shot(&field("camera"), &n)?;
    }
    Ok(())
}

fn check_shot(field: &str, n: &ShotNumbers) -> Result<(), ShortError> {
    let ranges = [
        ("yaw", n.yaw, -180.0, 180.0),
        ("pitch", n.pitch, -80.0, 80.0),
        ("roll", n.roll, -45.0, 45.0),
        ("zoom", n.zoom, 0.2, 6.0),
        ("shake", n.shake, 0.0, 1.0),
        ("ease", n.ease, 0.0, 5.0),
        ("shift[0]", n.shift[0], -1.0, 1.0),
        ("shift[1]", n.shift[1], -1.0, 1.0),
    ];
    for (name, value, lo, hi) in ranges {
        if !(lo..=hi).contains(&value) {
            return Err(ShortError::invalid(format!("{field}.{name}"), format!("{value} is outside {lo} to {hi}")));
        }
    }
    if let Some(w) = n.head_width
        && !(0.05..=4.0).contains(&w)
    {
        return Err(ShortError::invalid(format!("{field}.head_width"), format!("{w} is outside 0.05 to 4")));
    }
    for (name, value) in [("punch", n.punch), ("push", n.push)] {
        if value != 0.0 && !(1.0..=3.0).contains(&value) {
            return Err(ShortError::invalid(
                format!("{field}.{name}"),
                format!("{value} is outside 1 to 3 (or 0 for none)"),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
character = "lumi"

[[line]]
text = "Hello there!"
face = "happy+sparkle"
camera = "close+tilt-left+punch"
caption = "shout"

[[line]]
text = "Second line."
"#;

    #[test]
    fn reads_a_short_with_defaults() {
        let def = ShortDef::from_toml(SAMPLE).unwrap();
        assert_eq!(def.size, [1080, 1920]);
        assert_eq!(def.fps, 30);
        assert_eq!(def.lines.len(), 2);
        assert_eq!(def.lines[0].face.as_ref().unwrap().ids(), ["happy", "sparkle"]);
        assert_eq!(def.lines[0].caption, CaptionStyle::Shout);
        let shot = def.lines[0].camera.as_ref().unwrap().numbers().unwrap();
        assert!((shot.zoom - 1.35).abs() < 1e-6 && shot.roll < 0.0 && shot.punch > 1.0);
        assert!(def.lines[1].face.is_none());
    }

    #[test]
    fn round_trips_through_toml() {
        let def = ShortDef::from_toml(SAMPLE).unwrap();
        assert_eq!(ShortDef::from_toml(&def.to_toml().unwrap()).unwrap(), def);
    }

    #[test]
    fn refuses_unknown_shot_words_and_names_them() {
        let text = SAMPLE.replace("close+tilt-left+punch", "close+spin");
        let err = ShortDef::from_toml(&text).unwrap_err().to_string();
        assert!(err.contains("line[0].camera") && err.contains("'spin'") && err.contains("tilt-left"), "{err}");
    }

    #[test]
    fn refuses_odd_sizes_and_empty_shorts() {
        let odd = format!("size = [1081, 1920]\n{SAMPLE}");
        assert!(ShortDef::from_toml(&odd).unwrap_err().to_string().contains("odd"));
        assert!(ShortDef::from_toml("character = \"lumi\"").unwrap_err().to_string().contains("at least one line"));
    }

    #[test]
    fn reads_a_spoken_voice_and_checks_it() {
        let spoken = format!("{SAMPLE}\n[voice]\nkind = \"speech\"\nvoice = \"kokoro-82m/af_heart\"\n");
        let def = ShortDef::from_toml(&spoken).unwrap();
        assert_eq!(def.voice, VoiceDef::Speech { voice: "kokoro-82m/af_heart".into(), speed: 1.0 });
        assert_eq!(ShortDef::from_toml(&def.to_toml().unwrap()).unwrap(), def);
        let fast = spoken.replace("af_heart\"", "af_heart\"\nspeed = 3.0");
        assert!(ShortDef::from_toml(&fast).unwrap_err().to_string().contains("voice.speed"));
        let unnamed = spoken.replace("kokoro-82m/af_heart", "af_heart");
        assert!(ShortDef::from_toml(&unnamed).unwrap_err().to_string().contains("voice.voice"));
    }

    #[test]
    fn reads_cues_and_the_handheld_drift_and_checks_them() {
        let text = format!(
            "handheld = 0.0\n{SAMPLE}\n[[cue]]\nat = 1.5\nface = \"stare+gloom\"\ncamera = {{ zoom = 1.3, roll = -6.0 }}\n"
        );
        let def = ShortDef::from_toml(&text).unwrap();
        assert!(def.handheld.abs() < f32::EPSILON);
        assert_eq!(def.cues.len(), 1);
        assert_eq!(def.cues[0].face.as_ref().unwrap().ids(), ["stare", "gloom"]);
        assert_eq!(ShortDef::from_toml(&def.to_toml().unwrap()).unwrap(), def);
        let empty = format!("{SAMPLE}\n[[cue]]\nat = 1.0\n");
        assert!(ShortDef::from_toml(&empty).unwrap_err().to_string().contains("cue[0]"));
        let wild = text.replace("roll = -6.0", "roll = -60.0");
        assert!(ShortDef::from_toml(&wild).unwrap_err().to_string().contains("cue[0].camera.roll"));
        let shaky = text.replace("handheld = 0.0", "handheld = 9.0");
        assert!(ShortDef::from_toml(&shaky).unwrap_err().to_string().contains("handheld"));
        assert!(!ShortDef::from_toml(SAMPLE).unwrap().to_toml().unwrap().contains("handheld"), "the default stays out");
    }

    #[test]
    fn refuses_unknown_fields() {
        let text = format!("{SAMPLE}\nmood = 3\n");
        assert!(matches!(ShortDef::from_toml(&text), Err(ShortError::Parse(_))));
    }
}
