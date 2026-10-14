//! [`LlmError`]: the ways a turn fails (llm-api D5).

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a call or a turn failed. Refusal is not here: it is [`StopReason::Refused`](crate::StopReason::Refused),
/// because a refused turn still costs money. No variant ever carries a key or a request header.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum LlmError {
    /// The provider throttled the request. `None` means it gave no `retry-after`, which is not worth retrying.
    #[error("rate limited{}", .retry_after.map(|d| format!(", retry after {} s", d.as_secs())).unwrap_or_default())]
    RateLimited {
        /// How long the provider asked to wait.
        retry_after: Option<Duration>,
    },
    /// The provider is overloaded.
    #[error("the provider is overloaded")]
    Overloaded,
    /// The key was refused or is missing.
    #[error("the provider refused the key")]
    Auth,
    /// The prompt does not fit the model's context window.
    #[error("the prompt is longer than the {max}-token context window")]
    ContextTooLong {
        /// The model's context window, in tokens.
        max: u64,
    },
    /// The request needs something this client or model cannot do.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The connection failed, dropped or went idle.
    #[error("network error: {0}")]
    Network(String),
    /// The request or the answer is not what the contract allows.
    #[error("malformed: {0}")]
    Malformed(String),
    /// The caller cancelled the turn.
    #[error("cancelled")]
    Cancelled,
}
