//! Stage effects (design: `docs/design/particles.md` D9 to D14): the preset files, the scene's weather, the requests a
//! show or a person makes live, and the [`EffectService`] the ops reach the stage through.
//!
//! - **Presets** ([`EffectPreset`]) are typed files in `omuzaki-stage` (`library/effects/<name>.toml`), a burst or a
//!   weather each, with the render contract's [`ParticleParams`] as their motion and look (D9).
//! - **Weather** ([`WeatherDef`]) is scene state: the materialised values of a weather preset in
//!   `scenes/<id>/weather.toml`, edited by `stage.set_weather` (D10).
//! - **Bursts and the live weather** are performance, not edits: [`BurstRequest`] and [`LiveWeather`] go to the
//!   [`EffectService`], which queues them for the next frame and records each as an [`EffectEvent`] (D10 to D12, D14).
//!
//! Everything here is data and pure checks; the queue, the anchors, the emission log and the frame step are the
//! stage's.

use std::fmt;
use std::str::FromStr;

use omuzaki_core::colour::Colour;
use omuzaki_core::id::Slug;
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{ParticleParams, SpriteShape};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;
use crate::error::did_you_mean;
use crate::ids::{MemberId, PropId};

/// The most particles one burst asks for (D11).
pub const MAX_BURST_COUNT: u32 = 1_000;
/// The most chat emotes one burst carries (D13).
pub const MAX_BURST_EMOTES: usize = 8;
/// The most colours a burst may override with (D11).
pub const MAX_BURST_COLOURS: usize = 8;
/// The requests the effect queue holds before it refuses more with [`EffectError::Limited`] (D11).
pub const MAX_PENDING_EFFECTS: usize = 64;
/// The longest fade a clear takes, in seconds (D11).
pub const MAX_CLEAR_FADE_S: f32 = 5.0;
/// The only atlas a sprite may name: the stage's chat-emote atlas (D13).
pub const CHAT_EMOTE_ATLAS: &str = "chat-emotes";

/// What a preset does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum EffectKind {
    /// A handful of particles at once, from an anchor: confetti, hit stars, emote rain.
    Burst,
    /// A steady stream over the scene: rain, snow, petals.
    Weather,
}

impl EffectKind {
    /// The kind as written in files and ops.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Burst => "burst",
            Self::Weather => "weather",
        }
    }
}

/// The atlas an atlas sprite names. There is one: the chat-emote atlas the stage fills at run time (D13).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum AtlasName {
    /// `chat-emotes`.
    #[serde(rename = "chat-emotes")]
    ChatEmotes,
}

/// An atlas sprite as written: `{ atlas = "chat-emotes" }`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AtlasSprite {
    /// Which atlas.
    pub atlas: AtlasName,
}

/// What a preset's particles look like, as written in a file: a procedural shape (`{ shape = "rect", aspect = 0.6 }`,
/// the render contract's [`SpriteShape`]) or the chat-emote atlas (`{ atlas = "chat-emotes" }`). The renderer's
/// `Sprite` holds a texture handle, which no file may; the stage binds the atlas.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum SpriteDef {
    /// Drawn by the shader.
    Shape(SpriteShape),
    /// Cells of the chat-emote atlas.
    Atlas(AtlasSprite),
}

impl SpriteDef {
    /// The chat-emote atlas sprite.
    pub const fn chat_emotes() -> Self {
        Self::Atlas(AtlasSprite { atlas: AtlasName::ChatEmotes })
    }

    /// The shape, if this sprite is one.
    pub const fn shape(&self) -> Option<SpriteShape> {
        match self {
            Self::Shape(shape) => Some(*shape),
            Self::Atlas(_) => None,
        }
    }
}

/// Where a burst's colours come from (D9).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ColourSource {
    /// The anchor member's palette (`hair_mid`, `outfit_a`, `outfit_b`, `trim`, `eyes`) plus white; the preset's own
    /// colours when the anchor is no member.
    Palette,
    /// The preset's `particles.colours`.
    #[default]
    Preset,
}

/// What a point of an [`EffectAt`] is measured from (D12). Written as a string: `"subject"`, `"stage"`, `"camera"`,
/// `"world"`, `"member:<id>"` or `"prop:<id>"`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EffectOrigin {
    /// The scene's subject member.
    #[default]
    Subject,
    /// The stage origin.
    Stage,
    /// The active camera.
    Camera,
    /// The world origin, offsets in metres: a physics hit point (BR-12).
    World,
    /// A named cast member.
    Member(MemberId),
    /// A named prop's origin.
    Prop(PropId),
}

impl EffectOrigin {
    /// Whether offsets from this origin are in the member's subject units (else metres; scenes D4).
    pub const fn in_subject_units(&self) -> bool {
        matches!(self, Self::Subject | Self::Member(_))
    }

    /// The scene [`Anchor`] this origin is measured like, for an origin that is one.
    pub fn anchor(&self) -> Option<Anchor> {
        match self {
            Self::Subject => Some(Anchor::Subject),
            Self::Stage => Some(Anchor::Stage),
            Self::Member(id) => Some(Anchor::Member(id.clone())),
            Self::Camera | Self::World | Self::Prop(_) => None,
        }
    }
}

impl fmt::Display for EffectOrigin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Subject => f.write_str("subject"),
            Self::Stage => f.write_str("stage"),
            Self::Camera => f.write_str("camera"),
            Self::World => f.write_str("world"),
            Self::Member(id) => write!(f, "member:{id}"),
            Self::Prop(id) => write!(f, "prop:{id}"),
        }
    }
}

impl FromStr for EffectOrigin {
    type Err = EffectError;

    fn from_str(s: &str) -> Result<Self, EffectError> {
        let bad = |why: String| EffectError::Invalid(format!("at.from: {why}"));
        match s {
            "subject" => Ok(Self::Subject),
            "stage" => Ok(Self::Stage),
            "camera" => Ok(Self::Camera),
            "world" => Ok(Self::World),
            _ => {
                if let Some(id) = s.strip_prefix("member:") {
                    MemberId::checked(id).map(Self::Member).map_err(|e| bad(e.rule))
                } else if let Some(id) = s.strip_prefix("prop:") {
                    PropId::new(id).map(Self::Prop).map_err(|e| bad(e.to_string()))
                } else {
                    Err(bad(format!("`{s}` isn't `subject`, `stage`, `camera`, `world`, `member:<id>` or `prop:<id>`")))
                }
            }
        }
    }
}

impl Serialize for EffectOrigin {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for EffectOrigin {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for EffectOrigin {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "EffectOrigin".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "description": "`subject`, `stage`, `camera`, `world`, `member:<id>` or `prop:<id>`",
            "pattern": "^(subject|stage|camera|world|member:[a-z0-9_-]+|prop:[a-z0-9_-]+)$"
        })
    }
}

/// Where an effect starts (D12): `{ from = "member:kiri", point = "rightHand", offset = [0, 0.05, 0] }`.
///
/// `point` is a VRM humanoid bone name (`head`, `chest`, `rightHand`, ...) or `top` (just above the head) for the
/// subject and members; `top` (a strip above the frame's top edge at the subject's distance) or `centre` for the
/// camera; nothing for the others. Offsets are in subject units for members and metres otherwise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectAt {
    /// What it is measured from.
    #[serde(default)]
    pub from: EffectOrigin,
    /// A point on it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub point: Option<String>,
    /// An offset from the point, in the origin's frame.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub offset: [f32; 3],
}

fn is_zero(v: &[f32; 3]) -> bool {
    v.iter().all(|x| *x == 0.0)
}

impl EffectAt {
    /// The point `point` of `from`, with no offset.
    pub fn new(from: EffectOrigin, point: Option<&str>) -> Self {
        Self { from, point: point.map(str::to_owned), offset: [0.0; 3] }
    }

    /// Checks the structural rules: an offset is finite, and the point fits the origin (a bone or `top` on a member,
    /// `top` or `centre` on the camera, none elsewhere). Whether a bone exists is the stage's to say.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] naming the rule.
    pub fn validate(&self) -> Result<(), EffectError> {
        if !self.offset.iter().all(|v| v.is_finite()) {
            return Err(EffectError::Invalid("at.offset must be finite".into()));
        }
        let point = self.point.as_deref();
        match (&self.from, point) {
            (_, None) | (EffectOrigin::Camera, Some("top" | "centre")) => Ok(()),
            (EffectOrigin::Subject | EffectOrigin::Member(_), Some(p)) => {
                if !p.is_empty() && p.chars().all(|c| c.is_ascii_alphanumeric()) {
                    Ok(())
                } else {
                    Err(EffectError::Invalid(format!("at.point `{p}` is not a humanoid bone name or `top`")))
                }
            }
            (EffectOrigin::Camera, Some(p)) => {
                Err(EffectError::Invalid(format!("at.point on the camera is `top` or `centre`, not `{p}`")))
            }
            (from, Some(p)) => Err(EffectError::Invalid(format!("at.point `{p}`: `{from}` has no points"))),
        }
    }
}

/// A burst preset's defaults (`[burst]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BurstDefaults {
    /// Particles per burst, `1..=1,000`; for an emote burst, per emote.
    pub count: u32,
    /// Where it bursts from.
    #[serde(default)]
    pub at: EffectAt,
    /// Where its colours come from.
    #[serde(default)]
    pub colours: ColourSource,
}

/// An explicit weather volume: a box in an anchor's frame, births on its top face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AreaBox {
    /// What the box is measured against.
    #[serde(default)]
    pub anchor: Anchor,
    /// The low corner.
    pub min: [f32; 3],
    /// The high corner.
    pub max: [f32; 3],
}

/// The keyword `"auto"`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AutoArea {
    /// Around the cast and the active camera.
    Auto,
}

/// Where weather falls (D10): `"auto"` (the cast's floor bounds and the active camera, grown by one subject unit) or
/// `{ anchor, min, max }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum WeatherArea {
    /// Computed by the stage at resolve.
    Auto(AutoArea),
    /// A fixed box.
    Box(AreaBox),
}

impl Default for WeatherArea {
    fn default() -> Self {
        Self::Auto(AutoArea::Auto)
    }
}

impl WeatherArea {
    /// `"auto"`.
    pub const AUTO: Self = Self::Auto(AutoArea::Auto);

    /// A box from `min` to `max` in `anchor`'s frame.
    pub const fn boxed(anchor: Anchor, min: [f32; 3], max: [f32; 3]) -> Self {
        Self::Box(AreaBox { anchor, min, max })
    }
}

/// A weather preset's defaults (`[weather]`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeatherDefaults {
    /// Births per square metre per second at intensity 1, `> 0`.
    pub density: f32,
    /// Subject units above the floor where particles appear, `> 0`.
    pub height: f32,
    /// Where it falls.
    #[serde(default)]
    pub area: WeatherArea,
}

/// A preset file, `library/effects/<name>.toml` (D9; §3 shows a whole file).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct EffectPreset {
    /// The name ops use (`confetti`).
    pub name: Slug,
    /// The title people read.
    pub title: String,
    /// One sentence.
    pub summary: String,
    /// Search tags.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Semver text.
    pub version: String,
    /// The physical target its values were designed from.
    pub origin: String,
    /// Burst or weather.
    pub kind: EffectKind,
    /// What the particles look like.
    pub sprite: SpriteDef,
    /// A burst's defaults; required for `kind = "burst"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub burst: Option<BurstDefaults>,
    /// A weather's defaults; required for `kind = "weather"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weather: Option<WeatherDefaults>,
    /// Motion and look.
    pub particles: ParticleParams,
}

impl EffectPreset {
    /// Checks the preset's rules: its kind's block is present (and the other absent), the burst count is in range,
    /// a weather has a floor, positive density and height and draws a shape, the anchor fits, and the particles pass
    /// the render contract's validation.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] naming the field and the rule.
    pub fn validate(&self) -> Result<(), EffectError> {
        let bad = |rule: String| Err(EffectError::Invalid(format!("{}: {rule}", self.name)));
        if semver_parts(&self.version).is_none() {
            return bad(format!("version `{}` is not semver (major.minor.patch)", self.version));
        }
        match (self.kind, &self.burst, &self.weather) {
            (EffectKind::Burst, Some(burst), None) => {
                if !(1..=MAX_BURST_COUNT).contains(&burst.count) {
                    return bad(format!("burst.count must be in 1..={MAX_BURST_COUNT}, got {}", burst.count));
                }
                burst.at.validate().or_else(|e| bad(format!("burst.{e}")))?;
            }
            (EffectKind::Weather, None, Some(weather)) => {
                check_weather(weather.density, weather.height, &weather.area, self.sprite)
                    .or_else(|(path, rule)| bad(format!("weather.{path} {rule}")))?;
                if self.particles.floor.is_none() {
                    return bad("a weather preset needs particles.floor".into());
                }
            }
            (EffectKind::Burst, _, _) => return bad("kind = \"burst\" needs a [burst] block and no [weather]".into()),
            (EffectKind::Weather, _, _) => {
                return bad("kind = \"weather\" needs a [weather] block and no [burst]".into());
            }
        }
        if let SpriteDef::Shape(SpriteShape::Streak) = self.sprite
            && !matches!(self.particles.orientation, omuzaki_render_api::Orientation::Velocity { .. })
        {
            return bad("a streak sprite needs orientation kind = \"velocity\"".into());
        }
        self.particles.validate().or_else(|e| bad(format!("particles: {e}")))
    }

    /// The row `stage.effect.list` shows.
    pub fn info(&self) -> PresetInfo {
        PresetInfo {
            name: self.name.clone(),
            title: self.title.clone(),
            summary: self.summary.clone(),
            kind: self.kind,
            default_count: self.burst.as_ref().map(|b| b.count),
        }
    }

    /// The scene weather this preset describes at `intensity`, its values materialised (D10). `None` for a burst.
    pub fn weather_def(&self, intensity: f32) -> Option<WeatherDef> {
        let weather = self.weather.as_ref()?;
        Some(WeatherDef {
            preset: Some(self.name.clone()),
            intensity,
            area: weather.area.clone(),
            density: weather.density,
            height: weather.height,
            sprite: self.sprite,
            particles: self.particles.clone(),
        })
    }
}

impl EffectPreset {
    /// Reads a preset from its file's value (TOML read into JSON values), refusing a misspelt key with the nearest
    /// valid name, then [`validate`](Self::validate)s it.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] naming the key or the rule.
    pub fn from_value(value: &serde_json::Value) -> Result<Self, EffectError> {
        check_keys::<Self>("", value)?;
        for (table, check) in [
            ("burst", check_keys::<BurstDefaults> as fn(&str, &serde_json::Value) -> Result<(), EffectError>),
            ("weather", check_keys::<WeatherDefaults>),
            ("particles", check_keys::<ParticleParams>),
        ] {
            if let Some(inner) = value.get(table) {
                check(&format!("{table}."), inner)?;
            }
        }
        let preset: Self = serde_json::from_value(value.clone()).map_err(|e| EffectError::Invalid(e.to_string()))?;
        preset.validate()?;
        Ok(preset)
    }
}

/// Refuses a key of the table `value` that `T`'s schema doesn't list, naming the nearest one.
fn check_keys<T: JsonSchema>(prefix: &str, value: &serde_json::Value) -> Result<(), EffectError> {
    let Some(table) = value.as_object() else { return Ok(()) };
    let schema = schemars::schema_for!(T);
    let Some(known) = schema.get("properties").and_then(serde_json::Value::as_object) else { return Ok(()) };
    let names: Vec<&str> = known.keys().map(String::as_str).collect();
    match table.keys().find(|k| !known.contains_key(k.as_str())) {
        Some(unknown) => Err(EffectError::Invalid(format!("{prefix}{}", did_you_mean(unknown, &names)))),
        None => Ok(()),
    }
}

/// `major.minor.patch` as numbers.
fn semver_parts(text: &str) -> Option<[u32; 3]> {
    let mut parts = text.split('.').map(str::parse::<u32>);
    let out = [parts.next()?.ok()?, parts.next()?.ok()?, parts.next()?.ok()?];
    parts.next().is_none().then_some(out)
}

/// The weather rules shared by presets and scenes: `(path, rule)` of the first one broken.
fn check_weather(density: f32, height: f32, area: &WeatherArea, sprite: SpriteDef) -> Result<(), (String, String)> {
    if !(density.is_finite() && density > 0.0) {
        return Err(("density".into(), format!("must be more than 0, got {density}")));
    }
    if !(height.is_finite() && height > 0.0) {
        return Err(("height".into(), format!("must be more than 0, got {height}")));
    }
    if let WeatherArea::Box(b) = area {
        if !b.min.iter().chain(&b.max).all(|v| v.is_finite()) {
            return Err(("area".into(), "corners must be finite".into()));
        }
        if let Some(axis) = (0..3).find(|&i| b.min[i] > b.max[i]) {
            let name = ["x", "y", "z"][axis];
            return Err(("area".into(), format!("min.{name} is above max.{name}")));
        }
    }
    if let SpriteDef::Atlas(_) = sprite {
        return Err(("sprite".into(), "weather draws shapes only, not an atlas".into()));
    }
    Ok(())
}

/// The scene's weather (`scenes/<id>/weather.toml`, D10): a weather preset's values copied in, so retuning the preset
/// never changes a saved scene and a person can edit any value. Absent means no weather.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeatherDef {
    /// The preset the values came from: information only, like the lighting's `rig`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<Slug>,
    /// How much of `density`, `0..=1`.
    pub intensity: f32,
    /// Where it falls.
    #[serde(default)]
    pub area: WeatherArea,
    /// Births per square metre per second at intensity 1, `> 0`.
    pub density: f32,
    /// Subject units above the floor where particles appear, `> 0`.
    pub height: f32,
    /// A shape (weather draws no atlas).
    pub sprite: SpriteDef,
    /// Motion and look.
    pub particles: ParticleParams,
}

impl WeatherDef {
    /// Checks the scene rules for weather (design D13 of scenes; particles D10): `(path, rule)` of the first one
    /// broken, the path from the weather node. The anchor of an area box is checked against the cast by the scene.
    ///
    /// # Errors
    /// The path and the rule.
    pub fn check(&self) -> Result<(), (String, String)> {
        if !(self.intensity.is_finite() && (0.0..=1.0).contains(&self.intensity)) {
            return Err(("intensity".into(), format!("must be between 0 and 1, got {}", self.intensity)));
        }
        check_weather(self.density, self.height, &self.area, self.sprite)?;
        if let SpriteDef::Shape(SpriteShape::Streak) = self.sprite
            && !matches!(self.particles.orientation, omuzaki_render_api::Orientation::Velocity { .. })
        {
            return Err(("sprite".into(), "a streak needs orientation kind = \"velocity\"".into()));
        }
        self.particles.validate().map_err(|e| ("particles".into(), e.to_string()))
    }
}

/// A chat emote a burst shows (D13): the fields of a `chat.message` `EmoteSpan` that matter, so a span decodes as one
/// (its offsets are ignored).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct ChatEmoteRef {
    /// The platform's emote id.
    pub id: String,
    /// The emote's name as typed in chat.
    pub name: String,
    /// An `https` URL of a static picture, when the platform gives one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
}

/// `stage.effect.burst`'s arguments (D11): a burst of a preset, everything but the preset optional.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BurstRequest {
    /// The burst preset.
    pub preset: Slug,
    /// Where, instead of the preset's anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<EffectAt>,
    /// Particles, `1..=1,000`, instead of the preset's count; for an emote burst, per emote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
    /// One to eight colours instead of the preset's or the palette.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colours: Option<Vec<Colour>>,
    /// A size multiplier, `0.25..=4`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// Up to eight chat emotes to show (an atlas preset).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emotes: Vec<ChatEmoteRef>,
    /// Spawn nothing when `emotes` is empty (a chat message without emotes).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub only_with_emotes: bool,
    /// The seed, to repeat a burst exactly; else the service draws one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u32>,
}

impl BurstRequest {
    /// A burst of `preset` with its defaults.
    pub fn new(preset: Slug) -> Self {
        Self {
            preset,
            at: None,
            count: None,
            colours: None,
            scale: None,
            emotes: Vec::new(),
            only_with_emotes: false,
            seed: None,
        }
    }

    /// Checks the ranges. Nothing out of range is clamped silently (D11).
    ///
    /// # Errors
    /// [`EffectError::Invalid`] naming the field and the rule.
    pub fn validate(&self) -> Result<(), EffectError> {
        let bad = |rule: String| Err(EffectError::Invalid(rule));
        if let Some(count) = self.count
            && !(1..=MAX_BURST_COUNT).contains(&count)
        {
            return bad(format!("count must be in 1..={MAX_BURST_COUNT}, got {count}"));
        }
        if let Some(colours) = &self.colours
            && !(1..=MAX_BURST_COLOURS).contains(&colours.len())
        {
            return bad(format!("colours holds 1 to {MAX_BURST_COLOURS}, got {}", colours.len()));
        }
        if let Some(scale) = self.scale
            && !(scale.is_finite() && (0.25..=4.0).contains(&scale))
        {
            return bad(format!("scale must be in 0.25..=4, got {scale}"));
        }
        if self.emotes.len() > MAX_BURST_EMOTES {
            return bad(format!("emotes holds at most {MAX_BURST_EMOTES}, got {}", self.emotes.len()));
        }
        if let Some(e) = self.emotes.iter().find(|e| e.image.as_deref().is_some_and(|u| !u.starts_with("https://"))) {
            return bad(format!("emote `{}`: an image must be an https URL", e.name));
        }
        match &self.at {
            Some(at) => at.validate(),
            None => Ok(()),
        }
    }
}

/// What `stage.effect.burst` returns.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BurstAccepted {
    /// The preset.
    pub preset: Slug,
    /// The particles asked for (before any thinning, which the event and the status report).
    pub count: u32,
    /// The seed it will use.
    pub seed: u32,
}

/// The live weather override's values: a weather preset at an intensity, for a while or until changed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WeatherOverride {
    /// The weather preset.
    pub preset: Slug,
    /// `0..=1`, default 1.
    #[serde(default = "full")]
    pub intensity: f32,
    /// Seconds until the scene's weather comes back; forever when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub for_s: Option<f32>,
}

const fn full() -> f32 {
    1.0
}

/// `stage.effect.weather`'s arguments (D10, D11): a preset (`{ preset = "rain", intensity = 0.8, for_s = 60 }`),
/// `{ preset = "none" }` for no weather, or `{ preset = "scene" }` to give the scene's weather back.
#[derive(Clone, Debug, PartialEq)]
pub enum LiveWeather {
    /// A preset replaces the scene's weather.
    Preset(WeatherOverride),
    /// No weather until changed.
    None,
    /// The scene's own weather again.
    Scene,
}

impl LiveWeather {
    /// `preset` at `intensity`, for `for_s` seconds or until changed.
    pub fn preset(preset: Slug, intensity: f32, for_s: Option<f32>) -> Self {
        Self::Preset(WeatherOverride { preset, intensity, for_s })
    }

    /// Checks the ranges.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] naming the field and the rule.
    pub fn validate(&self) -> Result<(), EffectError> {
        let Self::Preset(o) = self else { return Ok(()) };
        if !(o.intensity.is_finite() && (0.0..=1.0).contains(&o.intensity)) {
            return Err(EffectError::Invalid(format!("intensity must be in 0..=1, got {}", o.intensity)));
        }
        if let Some(s) = o.for_s
            && !(s.is_finite() && s > 0.0)
        {
            return Err(EffectError::Invalid(format!("for_s must be more than 0, got {s}")));
        }
        Ok(())
    }
}

/// The flat form [`LiveWeather`] is written in.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LiveWeatherText {
    preset: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    intensity: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    for_s: Option<f32>,
}

impl Serialize for LiveWeather {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let text = match self {
            Self::Preset(o) => {
                LiveWeatherText { preset: o.preset.as_str().to_owned(), intensity: Some(o.intensity), for_s: o.for_s }
            }
            Self::None => LiveWeatherText { preset: "none".into(), intensity: None, for_s: None },
            Self::Scene => LiveWeatherText { preset: "scene".into(), intensity: None, for_s: None },
        };
        text.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LiveWeather {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = LiveWeatherText::deserialize(deserializer)?;
        let only_preset = |what| {
            if text.intensity.is_some() || text.for_s.is_some() {
                Err(serde::de::Error::custom(format!("`{what}` takes no intensity or for_s")))
            } else {
                Ok(())
            }
        };
        match text.preset.as_str() {
            "none" => only_preset("none").map(|()| Self::None),
            "scene" => only_preset("scene").map(|()| Self::Scene),
            name => {
                let preset = Slug::new(name).map_err(serde::de::Error::custom)?;
                Ok(Self::Preset(WeatherOverride {
                    preset,
                    intensity: text.intensity.unwrap_or(1.0),
                    for_s: text.for_s,
                }))
            }
        }
    }
}

impl JsonSchema for LiveWeather {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "LiveWeather".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "object",
            "description": "A weather preset for a while, `none` for no weather, or `scene` for the scene's own again.",
            "properties": {
                "preset": { "type": "string", "description": "A weather preset's name, `none` or `scene`." },
                "intensity": { "type": "number", "minimum": 0.0, "maximum": 1.0, "description": "How much, default 1." },
                "for_s": { "type": "number", "exclusiveMinimum": 0.0, "description": "Seconds until the scene's weather comes back." }
            },
            "required": ["preset"],
            "additionalProperties": false
        })
    }
}

/// Where in a burst's emitter its particles are born, as an event records it.
#[derive(Copy, Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BurstShape {
    /// The emitter's origin.
    #[default]
    Point,
    /// A rectangle in the emitter's XZ plane: the strip above a camera frame's top edge.
    #[non_exhaustive]
    Rect {
        /// Half extents along X and Z, metres.
        half: [f32; 2],
    },
}

impl BurstShape {
    /// A rectangle with half extents `half` (X, Z).
    pub const fn rect(half: [f32; 2]) -> Self {
        Self::Rect { half }
    }
}

/// A burst as it happened: everything replay needs to draw the same particles (D14).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BurstEvent {
    /// The preset.
    pub preset: Slug,
    /// The resolved emitter transform, column-major.
    pub emitter: [f32; 16],
    /// Where in the emitter they were born.
    #[serde(default)]
    pub shape: BurstShape,
    /// The anchor's velocity at the burst, metres per second.
    #[serde(default)]
    pub velocity: [f32; 3],
    /// The particles spawned, after thinning.
    pub count: u32,
    /// The particles the cap took away.
    pub thinned: u32,
    /// The seed.
    pub seed: u32,
    /// The colours it used, when not the preset's.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub colours: Vec<Colour>,
    /// The size multiplier.
    #[serde(default = "full")]
    pub scale: f32,
    /// The emotes it showed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub emotes: Vec<ChatEmoteRef>,
}

/// A clear as it happened.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClearEvent {
    /// Seconds over which the particles shrank away.
    pub fade_s: f32,
}

/// What happened.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum EffectEventKind {
    /// A burst spawned.
    Burst(BurstEvent),
    /// The live weather changed.
    Weather(LiveWeather),
    /// Everything was cleared.
    Clear(ClearEvent),
}

/// One effect event, at the scene time it took effect: what a take records and replays (D14).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EffectEvent {
    /// Scene seconds.
    pub at: Seconds,
    /// What happened.
    pub kind: EffectEventKind,
}

/// Where the weather drawn now comes from.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WeatherSource {
    /// No weather.
    #[default]
    None,
    /// The scene's.
    Scene,
    /// The live override's.
    Override,
}

/// What `stage.effect.status` reports (D11).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct EffectStatus {
    /// Live slots (an upper bound on live particles).
    pub slots: u32,
    /// The live cap.
    pub cap: u32,
    /// Where the weather comes from.
    pub weather: WeatherSource,
    /// The particle density the scheduler set, `0..=1`.
    pub density: f32,
    /// Requests queued for the next frame.
    pub pending: u32,
    /// Whether the renderer draws particles at all.
    pub drawn: bool,
    /// Particles the cap thinned away since the stage started.
    pub thinned: u64,
    /// What looked wrong (an anchor that fell back to the subject, an emote that drew the placeholder).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub findings: Vec<String>,
}

/// One row of `stage.effect.list`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PresetInfo {
    /// The name ops use.
    pub name: Slug,
    /// The title people read.
    pub title: String,
    /// One sentence.
    pub summary: String,
    /// Burst or weather.
    pub kind: EffectKind,
    /// A burst's default count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_count: Option<u32>,
}

/// Why the effect service refused a request.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum EffectError {
    /// No preset has that name.
    #[error("there is no effect preset '{name}'")]
    #[non_exhaustive]
    UnknownPreset {
        /// The name asked for.
        name: String,
        /// The nearest names, best first.
        nearest: Vec<String>,
    },
    /// The queue is full (a flood): try again later.
    #[error("too many effects are waiting; try again in a moment")]
    Limited,
    /// The request breaks a rule.
    #[error("{0}")]
    Invalid(String),
    /// The renderer draws no particles.
    #[error("the renderer draws no particles")]
    Unsupported,
}

impl EffectError {
    /// An unknown preset `name`, with the `nearest` names.
    pub fn unknown_preset(name: impl Into<String>, nearest: Vec<String>) -> Self {
        Self::UnknownPreset { name: name.into(), nearest }
    }
}

/// The stage's effects as the ops see them (D12). `omuzaki-stage` implements it with a bounded queue into the render
/// thread that never blocks, so a live action may call it from its `run`; the app registers it as an ops service.
pub trait EffectService: Send + Sync {
    /// The presets, in name order.
    fn presets(&self) -> &[PresetInfo];

    /// Queues a burst for the next frame.
    ///
    /// # Errors
    /// [`EffectError::UnknownPreset`], [`EffectError::Invalid`] (a weather preset, a rule broken),
    /// [`EffectError::Limited`] (the queue is full).
    fn burst(&self, request: &BurstRequest) -> Result<BurstAccepted, EffectError>;

    /// Sets, ends or replaces the live weather override.
    ///
    /// # Errors
    /// As [`burst`](Self::burst), for a burst preset or a bad range.
    fn live_weather(&self, request: &LiveWeather) -> Result<(), EffectError>;

    /// Shrinks every live particle away over `fade_s` seconds and ends the override.
    ///
    /// # Errors
    /// [`EffectError::Invalid`] for a fade outside `0..=5`, [`EffectError::Limited`] when the queue is full.
    fn clear(&self, fade_s: f32) -> Result<(), EffectError>;

    /// The latest published status.
    fn status(&self) -> EffectStatus;

    /// Replays a recorded event at its time (takes, D14).
    ///
    /// # Errors
    /// As the request it replays.
    fn replay(&self, event: &EffectEvent) -> Result<(), EffectError>;
}
