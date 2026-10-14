//! The `FakeLlm` script format (llm-api D10): JSON, one entry per turn.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use omuzaki_llm_api::{LlmError, LlmEvent};

/// A whole script: the model the fake pretends to be, and its turns in order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Script {
    /// The model id reported by [`LlmClient::info`](omuzaki_llm_api::LlmClient::info).
    pub model: String,
    /// One entry per `stream()` call.
    pub turns: Vec<ScriptTurn>,
}

/// One scripted turn.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
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
pub struct Injected {
    /// The error is returned in place of the event at this index (after chunking); past the end, after the last.
    pub before: usize,
    /// The error.
    pub error: LlmError,
}
