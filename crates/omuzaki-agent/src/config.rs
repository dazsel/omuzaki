//! [`HostConfig`] and the per-message [`Caps`] (D1, D2.7).

use std::sync::Arc;

use omuzaki_agent_api::entry::Entry;
use omuzaki_core::time::Clock;
use omuzaki_llm_api::message::Effort;
use omuzaki_llm_api::usage::PriceTable;
use omuzaki_ops_api::Actor;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::sink::EntrySink;

/// The per-user-message limits (D2.7). They are the eval defaults (`evals.md` §3.2), so the `assistant` driver and the
/// product stop at the same place. The settings task registers this type as the value of `assistant.caps`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Caps {
    /// Most LLM requests for one message.
    pub turns: u32,
    /// Most tool calls for one message.
    pub tool_calls: u32,
    /// Most seconds of wall time, excluding time spent waiting for a person.
    pub wall_s: u32,
    /// Most tokens, all four counters of every turn.
    pub tokens: u64,
}

impl Default for Caps {
    fn default() -> Self {
        Self { turns: 30, tool_calls: 60, wall_s: 600, tokens: 400_000 }
    }
}

/// The language the assistant replies in (ADR 0023: agents read English, people read their language). Tool names,
/// arguments and ids stay English.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Language {
    /// The name in English, such as "German".
    pub name: String,
    /// The BCP 47 tag, such as "de".
    pub tag: String,
}

impl Language {
    /// A language.
    pub fn new(name: impl Into<String>, tag: impl Into<String>) -> Self {
        Self { name: name.into(), tag: tag.into() }
    }

    /// English, the default.
    pub fn english() -> Self {
        Self::new("English", "en")
    }
}

/// Everything the host needs besides its links. Build it with [`HostConfig::new`] and the `with_*` setters; it grows
/// without breaking callers.
#[derive(Clone)]
#[non_exhaustive]
pub struct HostConfig {
    /// The per-message limits (`assistant.caps`).
    pub caps: Caps,
    /// The studio's person: resumes a paused session and performs Stop. Must be the session's principal.
    pub principal: Actor,
    /// Where each fact of the conversation is recorded (D7).
    pub sink: Arc<dyn EntrySink>,
    /// The clock every wait and cap is measured on.
    pub clock: Arc<dyn Clock>,
    /// The model's prices, to charge each turn and bound the next.
    pub prices: PriceTable,
    /// The language to reply in (`ui.language`).
    pub language: Language,
    /// The stored entries of the conversation being continued; empty for a new one (#1395).
    pub history: Vec<Entry>,
    /// A headless run's reply to `ask_person` (the eval task's `person_reply`); `None` leaves the question open.
    pub person_reply: Option<String>,
    /// The most output tokens one turn may produce.
    pub max_output: u32,
    /// How hard the model should think.
    pub effort: Effort,
}

impl HostConfig {
    /// The default caps, English, 2,048 output tokens, the provider's default effort and no history.
    pub fn new(principal: Actor, sink: Arc<dyn EntrySink>, clock: Arc<dyn Clock>, prices: PriceTable) -> Self {
        Self {
            caps: Caps::default(),
            principal,
            sink,
            clock,
            prices,
            language: Language::english(),
            history: Vec::new(),
            person_reply: None,
            max_output: 2_048,
            effort: Effort::Default,
        }
    }

    /// Sets the per-message limits.
    #[must_use]
    pub fn with_caps(mut self, caps: Caps) -> Self {
        self.caps = caps;
        self
    }

    /// Sets the reply language.
    #[must_use]
    pub fn with_language(mut self, language: Language) -> Self {
        self.language = language;
        self
    }

    /// Continues the stored conversation `history`.
    #[must_use]
    pub fn with_history(mut self, history: Vec<Entry>) -> Self {
        self.history = history;
        self
    }

    /// Answers `ask_person` with `reply`, as a headless run does.
    #[must_use]
    pub fn with_person_reply(mut self, reply: impl Into<String>) -> Self {
        self.person_reply = Some(reply.into());
        self
    }

    /// Sets the most output tokens one turn may produce.
    #[must_use]
    pub fn with_max_output(mut self, max_output: u32) -> Self {
        self.max_output = max_output;
        self
    }

    /// Sets how hard the model should think.
    #[must_use]
    pub fn with_effort(mut self, effort: Effort) -> Self {
        self.effort = effort;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_caps_are_the_eval_defaults() {
        assert_eq!(Caps::default(), Caps { turns: 30, tool_calls: 60, wall_s: 600, tokens: 400_000 });
    }

    #[test]
    fn caps_survive_json_and_refuse_unknown_fields() {
        let json = serde_json::to_string(&Caps::default()).unwrap();
        assert_eq!(serde_json::from_str::<Caps>(&json).unwrap(), Caps::default());
        assert!(serde_json::from_str::<Caps>(r#"{"turns":1,"tool_calls":1,"wall_s":1,"tokens":1,"extra":1}"#).is_err());
    }
}
