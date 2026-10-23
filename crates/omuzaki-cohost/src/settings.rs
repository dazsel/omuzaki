//! The co-host's settings key: which LLM writes its replies.
//!
//! The default is a model on this machine (an OpenAI-compatible server on loopback: llama.cpp's `llama-server`,
//! Ollama, LM Studio, vLLM), so nothing leaves the computer and nothing costs money. A person may point it at a hosted
//! service with their own key (bring your own key, stored in the keychain under a label); `performer.start` then asks
//! the person before it sends anything there. No agent may change this key: it decides where the person's words go
//! and what they pay.

use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Where a local OpenAI-compatible server listens by default (`llama-server`'s port).
pub const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8080/v1";
/// The provider kind of an OpenAI-compatible server, local or hosted.
pub const OPENAI_COMPATIBLE: &str = "openai-compatible";

fn openai_compatible() -> String {
    OPENAI_COMPATIBLE.to_owned()
}

/// The value of `performer.llm`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LlmChoice {
    /// The provider kind: `openai-compatible` (any OpenAI-compatible server) or another registered kind such as
    /// `anthropic`.
    #[serde(default = "openai_compatible")]
    pub kind: String,
    /// Where it is: `http://127.0.0.1:8080/v1` for a local server.
    pub endpoint: String,
    /// The model to ask for, as the server names it. A local server that serves one model may leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The keychain label of the key, for a hosted service. Never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Allow an endpoint that is not this machine. Off by default.
    #[serde(default)]
    pub allow_remote: bool,
    /// Let a reasoning model think before it answers. Off by default: a co-host answers at once, and thinking
    /// first costs seconds of silence on stream.
    #[serde(default)]
    pub reasoning: bool,
}

impl Default for LlmChoice {
    fn default() -> Self {
        Self {
            kind: openai_compatible(),
            endpoint: DEFAULT_ENDPOINT.to_owned(),
            model: None,
            key: None,
            allow_remote: false,
            reasoning: false,
        }
    }
}

/// `performer.llm`: the LLM the AI co-host answers with.
pub struct PerformerLlm;

impl Setting for PerformerLlm {
    type Value = LlmChoice;
    const META: SettingMeta = SettingMeta::new(
        "performer.llm",
        "The LLM the AI co-host answers with: a server on this machine by default, or a hosted one with your own key",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> LlmChoice {
        LlmChoice::default()
    }

    fn check(choice: &LlmChoice) -> Result<(), SettingError> {
        let key = Self::META.key;
        if choice.kind.trim().is_empty() {
            return Err(SettingError::invalid(key, "`kind` names a provider, such as `openai-compatible`"));
        }
        if !(choice.endpoint.starts_with("http://") || choice.endpoint.starts_with("https://")) {
            return Err(SettingError::invalid(key, "`endpoint` is an http or https URL"));
        }
        Ok(())
    }
}
register_setting!(PerformerLlm);
