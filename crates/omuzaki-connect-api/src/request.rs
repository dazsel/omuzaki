//! What to make (D2): one typed request per [`Capability`], tagged by `capability`, so an op's JSON schema is one flat
//! `oneOf` and an agent's typo fails with the field named (`deny_unknown_fields`).
//!
//! Absent options and empty lists may be left out, and are left out when written, so a tray record stays short.

use std::collections::BTreeMap;

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::Capability;

/// The longest prompt, text or message, in bytes.
pub const MAX_PROMPT_BYTES: usize = 16 * 1024;
/// The most outputs one request may ask for.
pub const MAX_COUNT: u8 = 8;
/// The longest image or texture side, in pixels.
pub const MAX_SIDE: u32 = 4096;
/// The most reference images of an image or texture request.
pub const MAX_REFERENCES: usize = 8;
/// The most images of a model3d request.
pub const MAX_MODEL3D_IMAGES: usize = 4;
/// The most images of a vision request.
pub const MAX_VISION_IMAGES: usize = 8;
/// The longest music or motion, in seconds.
pub const MAX_SECONDS: u32 = 300;
/// The most messages of an LLM request.
pub const MAX_MESSAGES: usize = 64;
/// The most output tokens an LLM or vision request may ask for.
pub const MAX_TOKENS: u32 = 64_000;
/// The largest `LoRA` weight, either way.
pub const MAX_LORA_WEIGHT: f32 = 2.0;

/// What to make. Tagged by capability: `{"capability": "image", "prompt": "...", ...}`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "capability", rename_all = "snake_case")]
pub enum Request {
    /// Images.
    Image(ImageRequest),
    /// Textures.
    Texture(TextureRequest),
    /// A 3D model.
    Model3d(Model3dRequest),
    /// Speech.
    Voice(VoiceRequest),
    /// Music or a sound effect.
    Music(MusicRequest),
    /// Motion.
    Motion(MotionRequest),
    /// One LLM turn.
    Llm(LlmRequest),
    /// An answer about images.
    Vision(VisionRequest),
    /// A DCC program call.
    Dcc(DccRequest),
}

/// Images from a prompt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImageRequest {
    /// What to draw.
    pub prompt: String,
    /// What to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub negative: Option<String>,
    /// Width in pixels, 1 to [`MAX_SIDE`].
    pub width: u32,
    /// Height in pixels, 1 to [`MAX_SIDE`].
    pub height: u32,
    /// How many, 1 to [`MAX_COUNT`].
    pub count: u8,
    /// The seed; the host fills it from the run when absent (D10).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Reference images, at most [`MAX_REFERENCES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ContentHash>,
    /// The model, as the connector names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// `LoRA`s and their weights.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub loras: Vec<LoraUse>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// One `LoRA` and its weight.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LoraUse {
    /// Its name, as the connector names it.
    pub name: String,
    /// Its weight, finite, within ±[`MAX_LORA_WEIGHT`].
    pub weight: f32,
}

/// Textures from a prompt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TextureRequest {
    /// What to make.
    pub prompt: String,
    /// Width in pixels, 1 to [`MAX_SIDE`].
    pub width: u32,
    /// Height in pixels, 1 to [`MAX_SIDE`].
    pub height: u32,
    /// Whether opposite edges must meet.
    #[serde(default)]
    pub tileable: bool,
    /// How many, 1 to [`MAX_COUNT`].
    pub count: u8,
    /// The seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Reference images, at most [`MAX_REFERENCES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<ContentHash>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// How hard a 3D generator works.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Quality {
    /// Fast and rough.
    Draft,
    /// The vendor's default.
    #[default]
    Standard,
    /// Slow and detailed.
    High,
}

/// A 3D model from a prompt or images.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Model3dRequest {
    /// What to make; needed unless `images` are given.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    /// Views of the object, at most [`MAX_MODEL3D_IMAGES`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<ContentHash>,
    /// How hard to work.
    #[serde(default)]
    pub quality: Quality,
    /// The seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// Which voice speaks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum VoiceChoice {
    /// One of the connector's own voices.
    #[non_exhaustive]
    Preset {
        /// The voice's id, as the connector names it.
        id: String,
    },
    /// A clone of a real person's voice: needs their consent record (D6).
    #[non_exhaustive]
    Clone {
        /// A recording of the voice.
        reference: ContentHash,
        /// Whose voice it is, as the consent record names them.
        person: String,
    },
}

impl VoiceChoice {
    /// [`VoiceChoice::Preset`].
    pub fn preset(id: impl Into<String>) -> Self {
        Self::Preset { id: id.into() }
    }

    /// [`VoiceChoice::Clone`].
    pub fn clone_of(reference: ContentHash, person: impl Into<String>) -> Self {
        Self::Clone { reference, person: person.into() }
    }
}

/// Speech from text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VoiceRequest {
    /// What to say.
    pub text: String,
    /// Who says it.
    pub voice: VoiceChoice,
    /// The language, as a BCP 47 tag, when the connector needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// The seed, for a voice engine that samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// Music or a sound effect.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MusicKind {
    /// A piece of music.
    #[default]
    Music,
    /// A sound effect.
    Sfx,
}

/// Music or a sound effect from a prompt.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MusicRequest {
    /// What it sounds like.
    pub prompt: String,
    /// How long, 1 to [`MAX_SECONDS`].
    pub seconds: u32,
    /// Music or a sound effect.
    #[serde(default)]
    pub kind: MusicKind,
    /// Whether it should loop seamlessly.
    #[serde(default)]
    pub looping: bool,
    /// How many, 1 to [`MAX_COUNT`].
    pub count: u8,
    /// The seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// Where a motion comes from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MotionSource {
    /// A description.
    #[non_exhaustive]
    Prompt {
        /// What the motion is.
        text: String,
    },
    /// A video of someone moving.
    #[non_exhaustive]
    Video {
        /// The video.
        asset: ContentHash,
    },
    /// A motion file to convert or clean up.
    #[non_exhaustive]
    File {
        /// The file.
        asset: ContentHash,
    },
}

impl MotionSource {
    /// [`MotionSource::Prompt`].
    pub fn prompt(text: impl Into<String>) -> Self {
        Self::Prompt { text: text.into() }
    }

    /// [`MotionSource::Video`].
    pub fn video(asset: ContentHash) -> Self {
        Self::Video { asset }
    }

    /// [`MotionSource::File`].
    pub fn file(asset: ContentHash) -> Self {
        Self::File { asset }
    }
}

/// Motion for a rig.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MotionRequest {
    /// Where it comes from.
    pub source: MotionSource,
    /// How long, 1 to [`MAX_SECONDS`]; the connector decides when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seconds: Option<u32>,
    /// The seed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// Adapter knobs the manifest declares.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub params: BTreeMap<String, Value>,
}

/// Who wrote a chat message.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The caller.
    User,
    /// The model, in an earlier turn.
    Assistant,
}

/// One message of an LLM request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    /// Who wrote it.
    pub role: Role,
    /// What it says.
    pub text: String,
}

/// One finished LLM turn: no caller tools, no streaming (the seam owns those, D2).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LlmRequest {
    /// The system prompt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub system: Option<String>,
    /// The conversation, 1 to [`MAX_MESSAGES`], the last from the user.
    pub messages: Vec<ChatMessage>,
    /// The most output tokens, 1 to [`MAX_TOKENS`].
    pub max_tokens: u32,
    /// Sampling temperature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// A JSON schema the answer must match; the answer is then [`Output::Json`](crate::Output::Json).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
    /// The model, as the provider names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// An answer about images.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct VisionRequest {
    /// What to say about them.
    pub instruction: String,
    /// The images, 1 to [`MAX_VISION_IMAGES`].
    pub images: Vec<ContentHash>,
    /// The most output tokens, 1 to [`MAX_TOKENS`].
    pub max_tokens: u32,
    /// A JSON schema the answer must match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json_schema: Option<Value>,
    /// The model, as the provider names it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

/// A call into a DCC program (D3): `tool` is the program and equals the connector id, `action` the mapped MCP tool.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DccRequest {
    /// The program, the connector's id (`blender`).
    pub tool: String,
    /// The mapped MCP tool to call.
    pub action: String,
    /// An input file for it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<ContentHash>,
    /// The action's arguments, declared in the manifest as params.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub args: BTreeMap<String, Value>,
}

/// One problem of a request, naming the field (`references/2`, `params/steps`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RequestProblem {
    /// The field path, `/`-separated.
    pub field: String,
    /// What is wrong, and what is allowed.
    pub problem: String,
}

impl RequestProblem {
    /// A problem at `field`.
    pub fn new(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self { field: field.into(), problem: problem.into() }
    }
}

impl std::fmt::Display for RequestProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.problem)
    }
}

impl Request {
    /// The capability it asks for.
    pub fn capability(&self) -> Capability {
        match self {
            Request::Image(_) => Capability::Image,
            Request::Texture(_) => Capability::Texture,
            Request::Model3d(_) => Capability::Model3d,
            Request::Voice(_) => Capability::Voice,
            Request::Music(_) => Capability::Music,
            Request::Motion(_) => Capability::Motion,
            Request::Llm(_) => Capability::Llm,
            Request::Vision(_) => Capability::Vision,
            Request::Dcc(_) => Capability::Dcc,
        }
    }

    /// Every asset it sends, in the order a connector reads them: references, images, a clone's reference, a video
    /// or file, a DCC input. Duplicates are kept: the preview counts what is sent.
    pub fn input_assets(&self) -> Vec<ContentHash> {
        match self {
            Request::Image(r) => r.references.clone(),
            Request::Texture(r) => r.references.clone(),
            Request::Model3d(r) => r.images.clone(),
            Request::Vision(r) => r.images.clone(),
            Request::Voice(r) => match &r.voice {
                VoiceChoice::Clone { reference, .. } => vec![*reference],
                VoiceChoice::Preset { .. } => Vec::new(),
            },
            Request::Motion(r) => match &r.source {
                MotionSource::Video { asset } | MotionSource::File { asset } => vec![*asset],
                MotionSource::Prompt { .. } => Vec::new(),
            },
            Request::Dcc(r) => r.asset.into_iter().collect(),
            Request::Music(_) | Request::Llm(_) => Vec::new(),
        }
    }

    /// How many outputs it asks for: `count`, or 1 for the kinds without one.
    pub fn count(&self) -> u32 {
        match self {
            Request::Image(r) => r.count.into(),
            Request::Texture(r) => r.count.into(),
            Request::Music(r) => r.count.into(),
            _ => 1,
        }
    }

    /// The text that goes in provenance as the prompt: the prompt, text or instruction, a motion's description, or
    /// the last user message of an LLM request.
    pub fn prompt_text(&self) -> Option<&str> {
        match self {
            Request::Image(r) => Some(&r.prompt),
            Request::Texture(r) => Some(&r.prompt),
            Request::Model3d(r) => r.prompt.as_deref(),
            Request::Voice(r) => Some(&r.text),
            Request::Music(r) => Some(&r.prompt),
            Request::Motion(r) => match &r.source {
                MotionSource::Prompt { text } => Some(text),
                _ => None,
            },
            Request::Llm(r) => r.messages.iter().rev().find(|m| m.role == Role::User).map(|m| m.text.as_str()),
            Request::Vision(r) => Some(&r.instruction),
            Request::Dcc(_) => None,
        }
    }

    /// Every byte of text it sends (prompt, negative, system and every message), for the token bound and the preview.
    pub fn text_bytes(&self) -> u64 {
        let len = |s: &str| s.len() as u64;
        match self {
            Request::Image(r) => len(&r.prompt) + r.negative.as_deref().map_or(0, len),
            Request::Llm(r) => {
                r.system.as_deref().map_or(0, len) + r.messages.iter().map(|m| len(&m.text)).sum::<u64>()
            }
            _ => self.prompt_text().map_or(0, len),
        }
    }

    /// The output-token budget of an LLM or vision request.
    pub fn max_tokens(&self) -> Option<u32> {
        match self {
            Request::Llm(r) => Some(r.max_tokens),
            Request::Vision(r) => Some(r.max_tokens),
            _ => None,
        }
    }

    /// The adapter knobs: `params`, or a DCC request's `args`. `None` for LLM and vision, which take none.
    pub fn params(&self) -> Option<&BTreeMap<String, Value>> {
        match self {
            Request::Image(r) => Some(&r.params),
            Request::Texture(r) => Some(&r.params),
            Request::Model3d(r) => Some(&r.params),
            Request::Voice(r) => Some(&r.params),
            Request::Music(r) => Some(&r.params),
            Request::Motion(r) => Some(&r.params),
            Request::Dcc(r) => Some(&r.args),
            Request::Llm(_) | Request::Vision(_) => None,
        }
    }

    /// The seed, if it has one.
    pub fn seed(&self) -> Option<u64> {
        match self {
            Request::Image(r) => r.seed,
            Request::Texture(r) => r.seed,
            Request::Model3d(r) => r.seed,
            Request::Voice(r) => r.seed,
            Request::Music(r) => r.seed,
            Request::Motion(r) => r.seed,
            Request::Llm(_) | Request::Vision(_) | Request::Dcc(_) => None,
        }
    }

    /// The same request with `seed` set; unchanged for the kinds without a seed (LLM, vision, DCC).
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        let slot = match &mut self {
            Request::Image(r) => &mut r.seed,
            Request::Texture(r) => &mut r.seed,
            Request::Model3d(r) => &mut r.seed,
            Request::Voice(r) => &mut r.seed,
            Request::Music(r) => &mut r.seed,
            Request::Motion(r) => &mut r.seed,
            Request::Llm(_) | Request::Vision(_) | Request::Dcc(_) => return self,
        };
        *slot = Some(seed);
        self
    }

    /// Every problem, each naming its field. Empty means the request is well formed; whether a connector supports
    /// it, and its `params`, are the preview's checks.
    pub fn check(&self) -> Vec<RequestProblem> {
        let mut p = Problems::default();
        match self {
            Request::Image(r) => {
                p.text("prompt", &r.prompt);
                if let Some(n) = &r.negative {
                    p.max_bytes("negative", n);
                }
                p.side("width", r.width);
                p.side("height", r.height);
                p.count(r.count);
                p.references(&r.references);
                for (i, lora) in r.loras.iter().enumerate() {
                    if !(lora.weight.is_finite() && lora.weight.abs() <= MAX_LORA_WEIGHT) {
                        p.add(format!("loras/{i}/weight"), format!("a weight is within ±{MAX_LORA_WEIGHT}"));
                    }
                }
            }
            Request::Texture(r) => {
                p.text("prompt", &r.prompt);
                p.side("width", r.width);
                p.side("height", r.height);
                p.count(r.count);
                p.references(&r.references);
            }
            Request::Model3d(r) => {
                if let Some(prompt) = &r.prompt {
                    p.text("prompt", prompt);
                }
                if r.images.len() > MAX_MODEL3D_IMAGES {
                    p.add("images", format!("at most {MAX_MODEL3D_IMAGES} images"));
                }
                if r.prompt.is_none() && r.images.is_empty() {
                    p.add("prompt", format!("give a prompt or 1 to {MAX_MODEL3D_IMAGES} images"));
                }
            }
            Request::Voice(r) => {
                p.text("text", &r.text);
                if let VoiceChoice::Clone { person, .. } = &r.voice
                    && person.trim().is_empty()
                {
                    p.add("voice/person", "name the person whose voice this is");
                }
            }
            Request::Music(r) => {
                p.text("prompt", &r.prompt);
                p.seconds("seconds", r.seconds);
                p.count(r.count);
            }
            Request::Motion(r) => {
                if let MotionSource::Prompt { text } = &r.source {
                    p.text("source/text", text);
                }
                if let Some(seconds) = r.seconds {
                    p.seconds("seconds", seconds);
                }
            }
            Request::Llm(r) => check_llm(r, &mut p),
            Request::Vision(r) => {
                p.text("instruction", &r.instruction);
                if r.images.is_empty() || r.images.len() > MAX_VISION_IMAGES {
                    p.add("images", format!("1 to {MAX_VISION_IMAGES} images"));
                }
                p.max_tokens(r.max_tokens);
            }
            Request::Dcc(r) => {
                if r.tool.trim().is_empty() {
                    p.add("tool", "name the program (the connector's id)");
                }
                if r.action.trim().is_empty() {
                    p.add("action", "name the action to call");
                }
            }
        }
        p.0
    }
}

fn check_llm(r: &LlmRequest, p: &mut Problems) {
    if let Some(system) = &r.system {
        p.max_bytes("system", system);
    }
    if r.messages.is_empty() || r.messages.len() > MAX_MESSAGES {
        p.add("messages", format!("1 to {MAX_MESSAGES} messages"));
    } else if r.messages.last().is_some_and(|m| m.role != Role::User) {
        p.add(format!("messages/{}/role", r.messages.len() - 1), "the last message is the user's");
    }
    for (i, m) in r.messages.iter().enumerate() {
        p.max_bytes(&format!("messages/{i}/text"), &m.text);
    }
    p.max_tokens(r.max_tokens);
    if r.temperature.is_some_and(|t| !t.is_finite()) {
        p.add("temperature", "a temperature is a finite number");
    }
}

/// Collects problems with the shared rules' wording.
#[derive(Default)]
struct Problems(Vec<RequestProblem>);

impl Problems {
    fn add(&mut self, field: impl Into<String>, problem: impl Into<String>) {
        self.0.push(RequestProblem::new(field, problem));
    }

    fn text(&mut self, field: &str, text: &str) {
        if text.trim().is_empty() {
            self.add(field, "it can't be empty");
        } else {
            self.max_bytes(field, text);
        }
    }

    fn max_bytes(&mut self, field: &str, text: &str) {
        if text.len() > MAX_PROMPT_BYTES {
            self.add(field, format!("{} bytes is longer than {MAX_PROMPT_BYTES}", text.len()));
        }
    }

    fn side(&mut self, field: &str, side: u32) {
        if side == 0 || side > MAX_SIDE {
            self.add(field, format!("1 to {MAX_SIDE} pixels, not {side}"));
        }
    }

    fn count(&mut self, count: u8) {
        if count == 0 || count > MAX_COUNT {
            self.add("count", format!("1 to {MAX_COUNT}, not {count}"));
        }
    }

    fn seconds(&mut self, field: &str, seconds: u32) {
        if seconds == 0 || seconds > MAX_SECONDS {
            self.add(field, format!("1 to {MAX_SECONDS} seconds, not {seconds}"));
        }
    }

    fn references(&mut self, references: &[ContentHash]) {
        if references.len() > MAX_REFERENCES {
            self.add(format!("references/{MAX_REFERENCES}"), format!("at most {MAX_REFERENCES} references"));
        }
    }

    fn max_tokens(&mut self, max_tokens: u32) {
        if max_tokens == 0 || max_tokens > MAX_TOKENS {
            self.add("max_tokens", format!("1 to {MAX_TOKENS}, not {max_tokens}"));
        }
    }
}
