//! [`LlmError`]: the ways a turn fails (llm-api D5).

use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why a call or a turn failed. Refusal is not here: it is [`StopReason::Refused`](crate::stream::StopReason::Refused),
/// because a refused turn still costs money. No variant ever carries a key or a request header.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[non_exhaustive]
pub enum LlmError {
    /// The provider throttled the request. `None` means it gave no `retry-after`, which is not worth retrying.
    #[error("rate limited{}", .retry_after.map(|d| format!(", retry after {} s", d.as_secs())).unwrap_or_default())]
    #[non_exhaustive]
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
    #[non_exhaustive]
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

impl LlmError {
    /// The provider throttled the request and asked to wait `retry_after`, if it said how long.
    pub fn rate_limited(retry_after: Option<Duration>) -> Self {
        Self::RateLimited { retry_after }
    }

    /// The prompt does not fit the model's context window of `max` tokens.
    pub fn context_too_long(max: u64) -> Self {
        Self::ContextTooLong { max }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limited_keeps_the_wait_and_says_it() {
        let waits = LlmError::rate_limited(Some(Duration::from_secs(7)));
        assert!(matches!(&waits, LlmError::RateLimited { retry_after: Some(d), .. } if *d == Duration::from_secs(7)));
        assert_eq!(waits.to_string(), "rate limited, retry after 7 s");
        let none = LlmError::rate_limited(None);
        assert!(matches!(none, LlmError::RateLimited { retry_after: None, .. }));
        assert_eq!(none.to_string(), "rate limited");
    }

    #[test]
    fn context_too_long_keeps_the_window() {
        let e = LlmError::context_too_long(200_000);
        assert!(matches!(e, LlmError::ContextTooLong { max: 200_000, .. }));
        assert_eq!(e.to_string(), "the prompt is longer than the 200000-token context window");
    }

    #[test]
    fn the_constructed_variants_survive_json() {
        for e in [LlmError::rate_limited(Some(Duration::from_secs(1))), LlmError::context_too_long(9)] {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<LlmError>(&json).unwrap(), e);
        }
    }
}
