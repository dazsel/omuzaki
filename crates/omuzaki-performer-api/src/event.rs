//! What the performer reports: [`PerformerEvent`]s, the JSON Lines transcript and [`AiLine`] (`ai-performer.md` §8,
//! §10).
//!
//! The UI and the Build Theatre receive [`TranscriptLine`]s (a bounded queue of
//! [`EVENT_QUEUE`](crate::limits::EVENT_QUEUE) per subscriber, oldest dropped); the transcript is the same lines, one
//! JSON object each: `{"t":800,"p":"3f9a1c0e7b2d","ev":"speaking","turn":1,"chunk":0,"text":"Hi chat!","ms":480}`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::PersonaRevision;
use crate::intent::Intent;

/// The AP-07 safety hook's verdict on a turn. Today the hook is a no-op, so every turn passes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SafetyVerdict {
    /// Nothing was filtered.
    Pass,
}

/// One thing the performer did or saw, as the transcript records it. Private turns' events carry no text
/// ([`PerformerEvent::redacted`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "ev", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PerformerEvent {
    /// A final transcript arrived.
    #[non_exhaustive]
    Heard {
        /// The utterance's number.
        utterance: u64,
        /// What was said; absent on a private line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// Why it was or wasn't addressed, when not by name ("follow-up, 1580 ms after turn 2").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        addressed: Option<String>,
        /// Heard in `CohostOnly`.
        #[serde(default, skip_serializing_if = "is_false")]
        private: bool,
    },
    /// A chat message reached the performer (the §10 record; `ChatMessage` itself is #1915's).
    #[non_exhaustive]
    Chat {
        /// The viewer's display name, as the prompt shows it.
        user: String,
        /// The message, as the prompt shows it.
        text: String,
    },
    /// A turn started.
    #[non_exhaustive]
    Thinking {
        /// The turn.
        turn: u64,
        /// The reply as the model wrote it, tags included; absent on a private line. Rehearsal and fakes fill it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        script: Option<String>,
        /// The safety hook's verdict.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        safety: Option<SafetyVerdict>,
        /// A private turn.
        #[serde(default, skip_serializing_if = "is_false")]
        private: bool,
    },
    /// A chunk started playing.
    #[non_exhaustive]
    Speaking {
        /// The turn.
        turn: u64,
        /// The chunk's number in the turn, from 0.
        chunk: u32,
        /// What is said; absent on a private line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
        /// The chunk's audio length in milliseconds; absent on a private line.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ms: Option<u64>,
        /// A private chunk.
        #[serde(default, skip_serializing_if = "is_false")]
        private: bool,
    },
    /// An intent was applied; the line's `t` is its `at`.
    #[non_exhaustive]
    Intent {
        /// The turn.
        turn: u64,
        /// The intent as applied (validated and clamped).
        intent: Intent,
    },
    /// The human took the floor while the performer spoke (`Heard::Held`).
    #[non_exhaustive]
    Held {
        /// The turn being spoken.
        turn: u64,
    },
    /// The turn was cut.
    #[non_exhaustive]
    Interrupted {
        /// The turn.
        turn: u64,
        /// Characters of the turn spoken before the cut.
        spoken_chars: u32,
    },
    /// An intent or a tag was dropped and never spoken.
    #[non_exhaustive]
    Filtered {
        /// The turn.
        turn: u64,
        /// The tag without its brackets ("clip backflip").
        what: String,
        /// Why ("unknown clip").
        why: String,
    },
    /// Something failed.
    #[non_exhaustive]
    Error {
        /// The turn, if one was running.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        /// What failed, as the error's text.
        kind: String,
    },
}

// serde's `skip_serializing_if` passes a reference.
#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_false(b: &bool) -> bool {
    !*b
}

impl PerformerEvent {
    /// A public `heard` line.
    pub fn heard(utterance: u64, text: impl Into<String>, addressed: Option<String>) -> Self {
        Self::Heard { utterance, text: Some(text.into()), addressed, private: false }
    }

    /// A private `heard` line: no text.
    pub fn heard_private(utterance: u64) -> Self {
        Self::Heard { utterance, text: None, addressed: None, private: true }
    }

    /// A `chat` line.
    pub fn chat(user: impl Into<String>, text: impl Into<String>) -> Self {
        Self::Chat { user: user.into(), text: text.into() }
    }

    /// A `thinking` line.
    pub fn thinking(turn: u64, script: Option<String>, safety: Option<SafetyVerdict>, private: bool) -> Self {
        Self::Thinking { turn, script, safety, private }
    }

    /// A public `speaking` line.
    pub fn speaking(turn: u64, chunk: u32, text: impl Into<String>, ms: u64) -> Self {
        Self::Speaking { turn, chunk, text: Some(text.into()), ms: Some(ms), private: false }
    }

    /// A private `speaking` line: no text, no length.
    pub fn speaking_private(turn: u64, chunk: u32) -> Self {
        Self::Speaking { turn, chunk, text: None, ms: None, private: true }
    }

    /// An `intent` line.
    pub fn intent(turn: u64, intent: Intent) -> Self {
        Self::Intent { turn, intent }
    }

    /// A `held` line.
    pub fn held(turn: u64) -> Self {
        Self::Held { turn }
    }

    /// An `interrupted` line.
    pub fn interrupted(turn: u64, spoken_chars: u32) -> Self {
        Self::Interrupted { turn, spoken_chars }
    }

    /// A `filtered` line.
    pub fn filtered(turn: u64, what: impl Into<String>, why: impl Into<String>) -> Self {
        Self::Filtered { turn, what: what.into(), why: why.into() }
    }

    /// An `error` line.
    pub fn error(turn: Option<u64>, kind: impl Into<String>) -> Self {
        Self::Error { turn, kind: kind.into() }
    }

    /// Whether the event belongs to a private turn.
    pub fn is_private(&self) -> bool {
        matches!(
            self,
            Self::Heard { private: true, .. }
                | Self::Thinking { private: true, .. }
                | Self::Speaking { private: true, .. }
        )
    }

    /// The event with every text of a private turn removed (§8: "private lines carry no text"), and its safety verdict
    /// too, since a verdict says something about what was said.
    #[must_use]
    pub fn redacted(mut self) -> Self {
        match &mut self {
            Self::Heard { text, addressed, private: true, .. } => {
                *text = None;
                *addressed = None;
            }
            Self::Thinking { script, safety, private: true, .. } => {
                *script = None;
                *safety = None;
            }
            Self::Speaking { text, ms, private: true, .. } => {
                *text = None;
                *ms = None;
            }
            _ => {}
        }
        self
    }
}

/// One transcript record: the runner clock in milliseconds, the persona revision, and the event.
///
/// Every way in redacts a private event: [`TranscriptLine::new`] and deserializing alike, so a line read from a file
/// someone edited still can't carry a private turn's text.
#[derive(Clone, Debug, PartialEq, Serialize, JsonSchema)]
#[non_exhaustive]
pub struct TranscriptLine {
    /// Runner clock, milliseconds.
    pub t: u64,
    /// The persona that spoke.
    pub p: PersonaRevision,
    /// The event.
    #[serde(flatten)]
    pub event: PerformerEvent,
}

impl TranscriptLine {
    /// A line at `t` ms. A private event is [`redacted`](PerformerEvent::redacted) here, so no line can carry a
    /// private turn's text.
    pub fn new(t: u64, p: PersonaRevision, event: PerformerEvent) -> Self {
        Self { t, p, event: event.redacted() }
    }

    /// The line as one line of JSON, no newline.
    ///
    /// # Errors
    /// Only if a number is not finite, which JSON cannot hold.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    /// Parses one line of JSON.
    ///
    /// # Errors
    /// When the text is not a transcript line.
    pub fn from_json(line: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(line)
    }
}

impl<'de> Deserialize<'de> for TranscriptLine {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Raw {
            t: u64,
            p: PersonaRevision,
            #[serde(flatten)]
            event: PerformerEvent,
        }
        let Raw { t, p, event } = Raw::deserialize(deserializer)?;
        Ok(Self::new(t, p, event))
    }
}

/// `ai.line`: one spoken chunk, for the BR-20 AI bubble and automations (§8). Published per chunk when it starts
/// playing; a cut chunk publishes nothing more; private turns publish nothing.
///
/// Defined here, not yet registered as a live event kind: the registration and the trusted publication path wait for
/// `omuzaki-live-api` (#1915).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub struct AiLine {
    /// What is spoken, after the safety hook, tags removed.
    pub text: String,
    /// The persona's `name`.
    pub speaker: String,
    /// The chunk's audio length, once known.
    pub duration_s: Option<f32>,
    /// The turn.
    pub turn: u64,
    /// The chunk in the turn.
    pub chunk: u32,
}

impl AiLine {
    /// A line for chunk `chunk` of turn `turn`.
    pub fn new(
        text: impl Into<String>,
        speaker: impl Into<String>,
        duration_s: Option<f32>,
        turn: u64,
        chunk: u32,
    ) -> Self {
        Self { text: text.into(), speaker: speaker.into(), duration_s, turn, chunk }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::score::ExpressionId;

    use super::*;

    fn rev() -> PersonaRevision {
        PersonaRevision::new("3f9a1c0e7b2d").unwrap()
    }

    fn every_event() -> Vec<PerformerEvent> {
        vec![
            PerformerEvent::heard(0, "Mochi, say hi to chat!", None),
            PerformerEvent::heard(1, "What's your favourite fold?", Some("follow-up, 1580 ms after turn 2".into())),
            PerformerEvent::heard_private(4),
            PerformerEvent::chat("viewer_a", "@Mochi hi"),
            PerformerEvent::thinking(1, Some("Hi chat!".into()), Some(SafetyVerdict::Pass), false),
            PerformerEvent::thinking(5, None, None, true),
            PerformerEvent::speaking(1, 0, "Hi chat!", 480),
            PerformerEvent::speaking_private(5, 0),
            PerformerEvent::intent(1, Intent::emote(ExpressionId::new("happy").unwrap(), 0.8, 1.5)),
            PerformerEvent::held(3),
            PerformerEvent::interrupted(3, 35),
            PerformerEvent::filtered(4, "clip backflip", "unknown clip"),
            PerformerEvent::error(None, "the voice provider failed"),
        ]
    }

    #[test]
    fn every_event_round_trips_as_a_line() {
        for (i, event) in every_event().into_iter().enumerate() {
            let line = TranscriptLine::new(i as u64 * 10, rev(), event);
            let json = line.to_json().unwrap();
            assert_eq!(TranscriptLine::from_json(&json).unwrap(), line, "{json}");
        }
    }

    #[test]
    fn lines_match_the_worked_example_byte_for_byte() {
        let cases = [
            (
                800,
                PerformerEvent::speaking(1, 0, "Hi chat!", 480),
                r#"{"t":800,"p":"3f9a1c0e7b2d","ev":"speaking","turn":1,"chunk":0,"text":"Hi chat!","ms":480}"#,
            ),
            (
                27000,
                PerformerEvent::heard_private(4),
                r#"{"t":27000,"p":"3f9a1c0e7b2d","ev":"heard","utterance":4,"private":true}"#,
            ),
            (
                10,
                PerformerEvent::thinking(1, Some("Hi chat! [gesture wave]".into()), Some(SafetyVerdict::Pass), false),
                r#"{"t":10,"p":"3f9a1c0e7b2d","ev":"thinking","turn":1,"script":"Hi chat! [gesture wave]","safety":"pass"}"#,
            ),
            (
                11905,
                PerformerEvent::interrupted(3, 35),
                r#"{"t":11905,"p":"3f9a1c0e7b2d","ev":"interrupted","turn":3,"spoken_chars":35}"#,
            ),
        ];
        for (t, event, json) in cases {
            assert_eq!(TranscriptLine::new(t, rev(), event).to_json().unwrap(), json);
        }
    }

    #[test]
    fn private_lines_never_carry_text() {
        let leaky = [
            PerformerEvent::Heard {
                utterance: 4,
                text: Some("secret".into()),
                addressed: Some("x".into()),
                private: true,
            },
            PerformerEvent::thinking(5, Some("secret".into()), Some(SafetyVerdict::Pass), true),
            PerformerEvent::Speaking { turn: 5, chunk: 0, text: Some("secret".into()), ms: Some(360), private: true },
        ];
        for event in leaky {
            assert!(event.is_private());
            let json = TranscriptLine::new(1, rev(), event.clone()).to_json().unwrap();
            assert!(!json.contains("secret") && !json.contains("\"ms\"") && !json.contains("safety"), "{json}");
            let edited = TranscriptLine { t: 1, p: rev(), event }.to_json().unwrap();
            assert!(edited.contains("secret"), "{edited}");
            let read = TranscriptLine::from_json(&edited).unwrap().to_json().unwrap();
            assert_eq!(read, json, "parsing a leaky line redacts it");
        }
        assert!(!PerformerEvent::held(1).is_private());
    }

    #[test]
    fn ai_line_round_trips() {
        let line = AiLine::new("Hi chat!", "Mochi", Some(0.48), 1, 0);
        let json = serde_json::to_string(&line).unwrap();
        assert_eq!(json, r#"{"text":"Hi chat!","speaker":"Mochi","duration_s":0.48,"turn":1,"chunk":0}"#);
        assert_eq!(serde_json::from_str::<AiLine>(&json).unwrap(), line);
    }
}
