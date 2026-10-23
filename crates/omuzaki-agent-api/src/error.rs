//! [`HostError`]: why the assistant refuses or fails (D1).

use omuzaki_llm_api::error::LlmError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Why the assistant cannot do what the panel asked, or what went wrong during an answer. No variant carries a key or
/// a request header.
#[derive(Clone, Debug, PartialEq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum HostError {
    /// An answer is running; wait for `Done` or Stop.
    #[error("an answer is already running")]
    Busy,
    /// No model is set up. The panel offers Settings > Models.
    #[error("no model is set up")]
    NoModel,
    /// The provider needs a key and the secret store holds none. The panel offers the key dialog.
    #[error("the model needs a key")]
    NeedsKey,
    /// The person has not yet consented to what leaves the machine (llm-api D7).
    #[error("the model needs the person's consent first")]
    NeedsConsent,
    /// The session ended (revoked or closed); a new conversation starts a new one.
    #[error("the session has ended")]
    SessionEnded,
    /// The conversation cannot be made to fit the model's window even after compaction (D5).
    #[error("the conversation does not fit the model's context window")]
    ContextFull,
    /// The model call failed.
    #[error("model error: {0}")]
    Llm(LlmError),
    /// The MCP link or the server refused. The text is the server's own, hint included.
    #[error("mcp error: {0}")]
    Mcp(String),
    /// The conversation store failed.
    #[error("conversation store error: {0}")]
    Store(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_variant_survives_json() {
        let all = [
            HostError::Busy,
            HostError::NoModel,
            HostError::NeedsKey,
            HostError::NeedsConsent,
            HostError::SessionEnded,
            HostError::ContextFull,
            HostError::Llm(LlmError::Overloaded),
            HostError::Mcp("timed out".into()),
            HostError::Store("disk full".into()),
        ];
        for e in all {
            let json = serde_json::to_string(&e).unwrap();
            assert_eq!(serde_json::from_str::<HostError>(&json).unwrap(), e);
        }
    }
}
