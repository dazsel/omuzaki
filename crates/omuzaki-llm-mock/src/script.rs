//! The `FakeLlm` script format (llm-api D10): JSON, one entry per turn.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::stream::LlmEvent;

/// A whole script: the model the fake pretends to be, and its turns in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Script {
    /// The model id reported by [`LlmClient::info`](omuzaki_llm_api::LlmClient::info).
    pub model: String,
    /// One entry per `stream()` call.
    pub turns: Vec<ScriptTurn>,
}

/// One scripted turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ScriptTurn {
    /// What the request must look like; a request that does not match is `Malformed`, naming the turn.
    #[serde(default)]
    pub expect: Option<Expect>,
    /// The events to send, in order.
    pub events: Vec<LlmEvent>,
    /// Split every `TextDelta` and `ToolUseInput` into pieces of about this many bytes (never inside a character).
    #[serde(default)]
    pub chunk: Option<usize>,
    /// Fail with an error before one event.
    #[serde(default)]
    pub error: Option<Injected>,
    /// Emit this many `Retrying` events before the first event.
    #[serde(default)]
    pub retries: u8,
    /// Milliseconds of `FakeClock` time between events.
    #[serde(default)]
    pub delay_ms: u64,
}

/// What a turn's request must contain.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Expect {
    /// The text of the last user message (tool-result text included) contains this.
    #[serde(default)]
    pub user_contains: Option<String>,
    /// The last user message answers exactly these tool-call ids, in order.
    #[serde(default)]
    pub answers: Option<Vec<String>>,
}

/// An error injected into a turn.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Injected {
    /// The error is returned in place of the event at this index (after chunking); past the end, after the last.
    pub before: usize,
    /// The error.
    pub error: LlmError,
}

impl Script {
    /// A script for the model `model` with no turns.
    pub fn new(model: impl Into<String>) -> Self {
        Self { model: model.into(), turns: Vec::new() }
    }

    /// Adds one turn after the ones already there.
    #[must_use]
    pub fn with_turn(mut self, turn: ScriptTurn) -> Self {
        self.turns.push(turn);
        self
    }
}

impl ScriptTurn {
    /// A turn that sends `events`, with no expectation, chunking, error, retries or delay.
    pub fn new(events: Vec<LlmEvent>) -> Self {
        Self { events, ..Self::default() }
    }

    /// Requires the request to match `expect`.
    #[must_use]
    pub fn with_expect(mut self, expect: Expect) -> Self {
        self.expect = Some(expect);
        self
    }

    /// Splits text and tool-input deltas into pieces of about `bytes` bytes.
    #[must_use]
    pub fn with_chunk(mut self, bytes: usize) -> Self {
        self.chunk = Some(bytes);
        self
    }

    /// Fails the turn with `error`.
    #[must_use]
    pub fn with_error(mut self, error: Injected) -> Self {
        self.error = Some(error);
        self
    }

    /// Emits `retries` `Retrying` events before the first event.
    #[must_use]
    pub fn with_retries(mut self, retries: u8) -> Self {
        self.retries = retries;
        self
    }

    /// Moves the `FakeClock` by `ms` milliseconds between events.
    #[must_use]
    pub fn with_delay_ms(mut self, ms: u64) -> Self {
        self.delay_ms = ms;
        self
    }
}

impl Expect {
    /// An expectation that checks nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The last user message must contain `text`.
    #[must_use]
    pub fn with_user_contains(mut self, text: impl Into<String>) -> Self {
        self.user_contains = Some(text.into());
        self
    }

    /// The last user message must answer exactly these tool-call ids, in order.
    #[must_use]
    pub fn with_answers(mut self, ids: Vec<String>) -> Self {
        self.answers = Some(ids);
        self
    }
}

impl Injected {
    /// Returns `error` in place of the event at index `before`.
    pub fn new(before: usize, error: LlmError) -> Self {
        Self { before, error }
    }
}
