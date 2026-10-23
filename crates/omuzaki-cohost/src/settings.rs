//! The co-host's settings keys: which LLM writes its replies (`performer.llm`), and what chooses its faces and
//! gestures (`performer.decider`).
//!
//! The LLM's default is a model on this machine (an OpenAI-compatible server on loopback: llama.cpp's `llama-server`,
//! Ollama, LM Studio, vLLM), so nothing leaves the computer and nothing costs money. A person may point it at a hosted
//! service with their own key (bring your own key, stored in the keychain under a label); `performer.start` then asks
//! the person before it sends anything there. No agent may change this key: it decides where the person's words go
//! and what they pay.
//!
//! The decider's default is the rule floor, which needs nothing. A local model (`llm`) or Jev (`systemone`,
//! the person's own key) can answer first; whatever they miss, the rules answer, so the character never freezes.

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

/// The decision layer's rule floor alone: deterministic, instant, nothing to install.
pub const DECIDER_RULES: &str = "rules";
/// A model through an OpenAI-compatible server: the co-host's own LLM, or another server.
pub const DECIDER_LLM: &str = "llm";
/// Jev's System One API, with the person's own key.
pub const DECIDER_SYSTEM_ONE: &str = omuzaki_performer::systemone::KIND;

fn rules() -> String {
    DECIDER_RULES.to_owned()
}

/// The value of `performer.decider`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeciderChoice {
    /// `rules` (the default: the rule floor alone), `llm` (a model through an OpenAI-compatible server) or
    /// `systemone` (Jev, by `TypeSafe`, with your own key). The rule floor answers whatever a model misses.
    #[serde(default = "rules")]
    pub kind: String,
    /// Where it is. For `llm`, leave it out to use the co-host's own LLM (`performer.llm`); for `systemone` it
    /// defaults to Jev's hosted endpoint.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The model to ask for (`jev-latest` for `systemone`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The keychain label of the key, for a hosted service. Never the key itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Allow an endpoint that is not this machine. Off by default; a hosted provider needs it on.
    #[serde(default)]
    pub allow_remote: bool,
}

impl Default for DeciderChoice {
    fn default() -> Self {
        Self { kind: rules(), endpoint: None, model: None, key: None, allow_remote: false }
    }
}

impl DeciderChoice {
    /// The endpoint requests go to, if any: its own, Jev's for `systemone`, or `None` (the rule floor, or the
    /// co-host's own LLM).
    pub fn endpoint(&self) -> Option<&str> {
        match (self.kind.as_str(), self.endpoint.as_deref()) {
            (DECIDER_RULES, _) => None,
            (_, Some(endpoint)) => Some(endpoint),
            (DECIDER_SYSTEM_ONE, None) => Some(omuzaki_performer::systemone::DEFAULT_ENDPOINT),
            _ => None,
        }
    }
}

/// `performer.decider`: what reads the live transcript and chooses the co-host's faces and gestures.
pub struct PerformerDecider;

impl Setting for PerformerDecider {
    type Value = DeciderChoice;
    const META: SettingMeta = SettingMeta::new(
        "performer.decider",
        "What chooses the AI co-host's faces and gestures as it talks and listens: built-in rules by default, a local \
         model, or a hosted provider with your own key",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> DeciderChoice {
        DeciderChoice::default()
    }

    fn check(choice: &DeciderChoice) -> Result<(), SettingError> {
        let key = Self::META.key;
        if ![DECIDER_RULES, DECIDER_LLM, DECIDER_SYSTEM_ONE].contains(&choice.kind.as_str()) {
            return Err(SettingError::invalid(key, "`kind` is `rules`, `llm` or `systemone`"));
        }
        if let Some(endpoint) = &choice.endpoint
            && !(endpoint.starts_with("http://") || endpoint.starts_with("https://"))
        {
            return Err(SettingError::invalid(key, "`endpoint` is an http or https URL"));
        }
        Ok(())
    }
}
register_setting!(PerformerDecider);
