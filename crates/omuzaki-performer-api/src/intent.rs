//! Intents: the performer's only actions (`ai-performer.md` §7.1), and the names they point at.
//!
//! An intent is checked against the member's [`Inventory`](crate::stage::Inventory), clamped to the limits in
//! [`crate::limits`] and compiled by the [`Stage`](crate::stage::Stage) into score cues. Serde is externally tagged in
//! snake case, as the §10 transcript shows: `{"emote":{"expression":"happy","intensity":0.8,"hold_s":1.5}}`.

use std::borrow::Cow;
use std::fmt;
use std::str::FromStr;

use omuzaki_anim_api::score::{ClipId, ExpressionId};
use omuzaki_stage_api::ids::{MemberId, PropId};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::limits::{
    EMOTE_HOLD_DEFAULT_S, EMOTE_HOLD_MAX_S, EMOTE_HOLD_MIN_S, LOOK_HOLD_DEFAULT_S, LOOK_HOLD_MAX_S, LOOK_HOLD_MIN_S,
};

/// One action of the performer (§7.1). Growing: AP-08 and later features add verbs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum Intent {
    /// Show an expression (`[emote <expression> [intensity] [hold Ns]]`), compiled to `CueKind::Expression`.
    #[non_exhaustive]
    Emote {
        /// An expression of the character's `ExpressionSet`.
        expression: ExpressionId,
        /// `0..=1`.
        intensity: f32,
        /// Seconds, [`EMOTE_HOLD_MIN_S`]`..=`[`EMOTE_HOLD_MAX_S`].
        hold_s: f32,
    },
    /// Play a clip tagged `gesture` (`[gesture <clip> [weight]]`); one at a time.
    #[non_exhaustive]
    Gesture {
        /// The clip.
        clip: ClipId,
        /// `0..=1`.
        weight: f32,
    },
    /// Play any clip of the character at weight 1 (`[clip <clip>]`), at most
    /// [`PLAY_CLIP_MAX_S`](crate::limits::PLAY_CLIP_MAX_S) long.
    #[non_exhaustive]
    PlayClip {
        /// The clip.
        clip: ClipId,
    },
    /// Look somewhere (`[look <target> [hold Ns]]`), through `IkGoals.look_at`.
    #[non_exhaustive]
    LookAt {
        /// Where.
        target: GazeTarget,
        /// Seconds, [`LOOK_HOLD_MIN_S`]`..=`[`LOOK_HOLD_MAX_S`].
        hold_s: f32,
    },
    /// Walk to a place (`[move <place>]`), as a live placement offset.
    #[non_exhaustive]
    MoveTo {
        /// Where.
        place: PlaceId,
    },
    /// Nod (`[nod [strength]]`): the head dips and comes back, a listener's "mm-hm" (#2453). The decision layer nods
    /// on the speech host's backchannel cues while the person talks; the model may tag one too.
    #[non_exhaustive]
    Nod {
        /// `0..=1`: 1 is a clear nod, 0.3 a small one.
        strength: f32,
    },
    /// Say a chunk: the chunks themselves, never a tag.
    #[non_exhaustive]
    Speak {
        /// The words, tags removed.
        text: String,
        /// The style in force for this chunk.
        tags: Vec<StyleTag>,
    },
}

impl Intent {
    /// `[emote expression intensity hold]`.
    pub fn emote(expression: ExpressionId, intensity: f32, hold_s: f32) -> Self {
        Self::Emote { expression, intensity, hold_s }
    }

    /// `[gesture clip weight]`.
    pub fn gesture(clip: ClipId, weight: f32) -> Self {
        Self::Gesture { clip, weight }
    }

    /// `[clip clip]`.
    pub fn play_clip(clip: ClipId) -> Self {
        Self::PlayClip { clip }
    }

    /// `[look target hold]`.
    pub fn look_at(target: GazeTarget, hold_s: f32) -> Self {
        Self::LookAt { target, hold_s }
    }

    /// `[move place]`.
    pub fn move_to(place: PlaceId) -> Self {
        Self::MoveTo { place }
    }

    /// `[nod strength]`.
    pub fn nod(strength: f32) -> Self {
        Self::Nod { strength }
    }

    /// A spoken chunk with its style.
    pub fn speak(text: impl Into<String>, tags: Vec<StyleTag>) -> Self {
        Self::Speak { text: text.into(), tags }
    }

    /// The tag verb that writes this intent (`emote`, `gesture`, `clip`, `look`, `move`, `nod`), or `None` for speech.
    pub fn verb(&self) -> Option<&'static str> {
        match self {
            Self::Emote { .. } => Some("emote"),
            Self::Gesture { .. } => Some("gesture"),
            Self::PlayClip { .. } => Some("clip"),
            Self::LookAt { .. } => Some("look"),
            Self::MoveTo { .. } => Some("move"),
            Self::Nod { .. } => Some("nod"),
            Self::Speak { .. } => None,
        }
    }

    /// The intent with every number brought inside its §7.1 range, and whether anything changed (§7.1: "out-of-range
    /// values are clamped and reported"). A value that is not finite takes the default: intensity and weight 1, the
    /// verb's default hold.
    #[must_use]
    pub fn clamped(&self) -> (Self, bool) {
        let mut out = self.clone();
        let changed = match &mut out {
            Self::Emote { intensity, hold_s, .. } => {
                let a = clamp(intensity, 0.0, 1.0, 1.0);
                let b = clamp(hold_s, EMOTE_HOLD_MIN_S, EMOTE_HOLD_MAX_S, EMOTE_HOLD_DEFAULT_S);
                a | b
            }
            Self::Gesture { weight, .. } => clamp(weight, 0.0, 1.0, 1.0),
            Self::Nod { strength } => clamp(strength, 0.0, 1.0, 1.0),
            Self::LookAt { hold_s, .. } => clamp(hold_s, LOOK_HOLD_MIN_S, LOOK_HOLD_MAX_S, LOOK_HOLD_DEFAULT_S),
            Self::PlayClip { .. } | Self::MoveTo { .. } | Self::Speak { .. } => false,
        };
        (out, changed)
    }
}

/// Clamps `v` to `lo..=hi` (a non-finite value becomes `default`) and says whether it changed.
fn clamp(v: &mut f32, lo: f32, hi: f32, default: f32) -> bool {
    let new = if v.is_finite() { v.clamp(lo, hi) } else { default };
    let changed = new.to_bits() != v.to_bits();
    *v = new;
    changed
}

/// Why a [`GazeTarget`] or [`PlaceId`] did not parse.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{got:?} is not a {what}: use {expected}")]
pub struct TargetError {
    /// What was being parsed.
    pub what: &'static str,
    /// The text that was given.
    pub got: String,
    /// The forms that are accepted.
    pub expected: &'static str,
}

/// Where a `look` points (§7.1): `camera`, `chat`, `away`, `member:<id>` or `prop:<id>`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GazeTarget {
    /// The program camera.
    Camera,
    /// Where the scene puts the chat (the person's side).
    Chat,
    /// Away from everyone, as when thinking.
    Away,
    /// Another member of the scene.
    Member(MemberId),
    /// A prop of the scene.
    Prop(PropId),
}

/// Where a `move` goes (§7.1): the member's own cast position (`home`), or beside another member or a prop. Names a spot
/// without a scene schema change.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum PlaceId {
    /// The member's own cast position.
    Home,
    /// Beside another member.
    Member(MemberId),
    /// Beside a prop.
    Prop(PropId),
}

/// Parses `member:<id>` or `prop:<id>`, the forms gaze targets and places share.
fn parse_named<T>(s: &str, member: fn(MemberId) -> T, prop: fn(PropId) -> T) -> Option<T> {
    let (kind, id) = s.split_once(':')?;
    match kind {
        "member" => MemberId::new(id).ok().map(member),
        "prop" => PropId::new(id).ok().map(prop),
        _ => None,
    }
}

impl GazeTarget {
    /// The canonical text form.
    pub fn as_text(&self) -> Cow<'static, str> {
        match self {
            Self::Camera => "camera".into(),
            Self::Chat => "chat".into(),
            Self::Away => "away".into(),
            Self::Member(m) => format!("member:{m}").into(),
            Self::Prop(p) => format!("prop:{p}").into(),
        }
    }
}

impl FromStr for GazeTarget {
    type Err = TargetError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "camera" => Ok(Self::Camera),
            "chat" => Ok(Self::Chat),
            "away" => Ok(Self::Away),
            _ => parse_named(s, Self::Member, Self::Prop).ok_or_else(|| TargetError {
                what: "gaze target",
                got: s.to_owned(),
                expected: "camera, chat, away, member:<id> or prop:<id>",
            }),
        }
    }
}

impl PlaceId {
    /// The canonical text form.
    pub fn as_text(&self) -> Cow<'static, str> {
        match self {
            Self::Home => "home".into(),
            Self::Member(m) => format!("member:{m}").into(),
            Self::Prop(p) => format!("prop:{p}").into(),
        }
    }
}

impl FromStr for PlaceId {
    type Err = TargetError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "home" {
            return Ok(Self::Home);
        }
        parse_named(s, Self::Member, Self::Prop).ok_or_else(|| TargetError {
            what: "place",
            got: s.to_owned(),
            expected: "home, member:<id> or prop:<id>",
        })
    }
}

/// Serde, `Display` and a string schema for a type with `as_text` and `FromStr`.
macro_rules! text_form {
    ($ty:ident, $description:literal) => {
        impl fmt::Display for $ty {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.as_text())
            }
        }

        impl Serialize for $ty {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(&self.as_text())
            }
        }

        impl<'de> Deserialize<'de> for $ty {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                String::deserialize(d)?.parse().map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $ty {
            fn schema_name() -> Cow<'static, str> {
                stringify!($ty).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "description": $description })
            }
        }
    };
}

text_form!(GazeTarget, "camera, chat, away, member:<id> or prop:<id>");
text_form!(PlaceId, "home, member:<id> or prop:<id>");

/// A speaking style (`[style <tag…>]`): the closed set of `ai-performer-voice-face.md` §3.6. Each one multiplies the
/// TTS speed, may set its expressiveness and adds gain, from the next chunk to the end of the turn. Unknown tags are
/// dropped by the parser, so the set is closed on purpose.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StyleTag {
    /// Slower and flatter.
    Calm,
    /// Faster and livelier, a little louder.
    Excited,
    /// Slower, flatter and quieter.
    Sad,
    /// Slow and very quiet.
    Whisper,
    /// Lively; also sets `TalkState.laughing`.
    Laugh,
    /// Faster.
    Fast,
    /// Slower.
    Slow,
}

impl StyleTag {
    /// Every tag, in the §3.6 table's order.
    pub const ALL: [Self; 7] =
        [Self::Calm, Self::Excited, Self::Sad, Self::Whisper, Self::Laugh, Self::Fast, Self::Slow];

    /// The tag's name in the grammar.
    pub fn name(self) -> &'static str {
        match self {
            Self::Calm => "calm",
            Self::Excited => "excited",
            Self::Sad => "sad",
            Self::Whisper => "whisper",
            Self::Laugh => "laugh",
            Self::Fast => "fast",
            Self::Slow => "slow",
        }
    }

    /// The tag called `name`, if there is one.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == name)
    }

    /// The factor on TTS speed (§3.6); products are clamped to the assignment's speed range by the TTS host.
    pub fn speed_factor(self) -> f32 {
        match self {
            Self::Calm => 0.95,
            Self::Excited => 1.08,
            Self::Sad | Self::Whisper => 0.90,
            Self::Laugh => 1.00,
            Self::Fast => 1.15,
            Self::Slow => 0.87,
        }
    }

    /// The TTS `expressiveness` it sets (`0..=1`, 0.5 neutral), or `None` to leave it.
    pub fn expressiveness(self) -> Option<f32> {
        match self {
            Self::Calm => Some(0.35),
            Self::Excited => Some(0.80),
            Self::Sad => Some(0.30),
            Self::Whisper => Some(0.20),
            Self::Laugh => Some(0.75),
            Self::Fast | Self::Slow => None,
        }
    }

    /// The gain it adds to the PCM, in dB.
    pub fn gain_db(self) -> f32 {
        match self {
            Self::Excited => 1.0,
            Self::Sad => -2.0,
            Self::Whisper => -8.0,
            Self::Calm | Self::Laugh | Self::Fast | Self::Slow => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_intent() -> Vec<Intent> {
        vec![
            Intent::emote(ExpressionId::new("happy").unwrap(), 0.8, 1.5),
            Intent::gesture(ClipId::new("wave").unwrap(), 1.0),
            Intent::play_clip(ClipId::new("backflip").unwrap()),
            Intent::look_at(GazeTarget::Chat, 1.5),
            Intent::look_at(GazeTarget::Member(MemberId::new("kai").unwrap()), 2.0),
            Intent::look_at(GazeTarget::Prop(PropId::new("lamp").unwrap()), 0.5),
            Intent::move_to(PlaceId::Home),
            Intent::move_to(PlaceId::Member(MemberId::new("kai").unwrap())),
            Intent::move_to(PlaceId::Prop(PropId::new("door").unwrap())),
            Intent::speak("Hi chat!", vec![StyleTag::Excited, StyleTag::Fast]),
        ]
    }

    #[test]
    fn every_intent_round_trips_through_json() {
        for intent in every_intent() {
            let json = serde_json::to_string(&intent).unwrap();
            assert_eq!(serde_json::from_str::<Intent>(&json).unwrap(), intent, "{json}");
        }
    }

    #[test]
    fn json_matches_the_worked_example() {
        let cases = [
            (every_intent()[0].clone(), r#"{"emote":{"expression":"happy","intensity":0.8,"hold_s":1.5}}"#),
            (every_intent()[1].clone(), r#"{"gesture":{"clip":"wave","weight":1.0}}"#),
            (every_intent()[3].clone(), r#"{"look_at":{"target":"chat","hold_s":1.5}}"#),
            (every_intent()[8].clone(), r#"{"move_to":{"place":"prop:door"}}"#),
        ];
        for (intent, json) in cases {
            assert_eq!(serde_json::to_string(&intent).unwrap(), json);
        }
        assert!(serde_json::from_str::<Intent>(r#"{"emote":{"expression":"happy"}}"#).is_err());
        assert!(serde_json::from_str::<Intent>(r#"{"move_to":{"place":"door"}}"#).is_err());
    }

    #[test]
    fn verbs_match_the_tag_grammar() {
        let verbs: Vec<_> = every_intent().iter().map(Intent::verb).collect();
        assert_eq!(&verbs[..4], [Some("emote"), Some("gesture"), Some("clip"), Some("look")]);
        assert_eq!((verbs[6], verbs[9]), (Some("move"), None));
    }

    #[test]
    fn out_of_range_values_are_clamped_and_reported() {
        let happy = ExpressionId::new("happy").unwrap();
        let (out, changed) = Intent::emote(happy.clone(), 1.7, 20.0).clamped();
        assert_eq!((out, changed), (Intent::emote(happy.clone(), 1.0, EMOTE_HOLD_MAX_S), true));
        let (out, changed) = Intent::emote(happy.clone(), f32::NAN, 0.1).clamped();
        assert_eq!((out, changed), (Intent::emote(happy.clone(), 1.0, EMOTE_HOLD_MIN_S), true));
        let ok = Intent::emote(happy, 0.5, 1.5);
        assert_eq!(ok.clamped(), (ok.clone(), false));
        let (out, changed) = Intent::look_at(GazeTarget::Away, 60.0).clamped();
        assert_eq!((out, changed), (Intent::look_at(GazeTarget::Away, LOOK_HOLD_MAX_S), true));
        let (out, changed) = Intent::gesture(ClipId::new("wave").unwrap(), -0.5).clamped();
        assert_eq!((out, changed), (Intent::gesture(ClipId::new("wave").unwrap(), 0.0), true));
    }

    #[test]
    fn targets_and_places_parse_only_their_forms() {
        assert_eq!("away".parse::<GazeTarget>().unwrap(), GazeTarget::Away);
        assert!("home".parse::<GazeTarget>().is_err());
        assert!("camera".parse::<PlaceId>().is_err());
        assert!("member:".parse::<PlaceId>().is_err());
        assert!("member:Kai".parse::<GazeTarget>().is_err());
        assert!("thing:kai".parse::<GazeTarget>().is_err());
        assert_eq!(PlaceId::Member(MemberId::new("kai").unwrap()).to_string(), "member:kai");
    }

    #[test]
    fn style_tags_follow_the_table() {
        let row = |t: StyleTag| (t.name(), t.speed_factor(), t.expressiveness(), t.gain_db());
        assert_eq!(row(StyleTag::Calm), ("calm", 0.95, Some(0.35), 0.0));
        assert_eq!(row(StyleTag::Excited), ("excited", 1.08, Some(0.80), 1.0));
        assert_eq!(row(StyleTag::Sad), ("sad", 0.90, Some(0.30), -2.0));
        assert_eq!(row(StyleTag::Whisper), ("whisper", 0.90, Some(0.20), -8.0));
        assert_eq!(row(StyleTag::Laugh), ("laugh", 1.00, Some(0.75), 0.0));
        assert_eq!(row(StyleTag::Fast), ("fast", 1.15, None, 0.0));
        assert_eq!(row(StyleTag::Slow), ("slow", 0.87, None, 0.0));
        for t in StyleTag::ALL {
            assert_eq!(StyleTag::from_name(t.name()), Some(t));
            assert_eq!(serde_json::to_string(&t).unwrap(), format!("\"{}\"", t.name()));
        }
        assert_eq!(StyleTag::from_name("angry"), None);
    }
}
