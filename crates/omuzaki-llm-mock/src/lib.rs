//! The fake LLM (llm-api D10): [`FakeLlm`] plays a JSON [`Script`] of turns through the real `LlmClient` contract, so
//! the assistant, the performer, connectors and the eval driver test against one fake.
//!
//! - **Deterministic:** the same script gives the same events, always. Delays move a `FakeClock`; nothing sleeps.
//! - **Checked:** a turn's `expect` must match the request, and a request with no turn left is `Malformed`, naming the
//!   turn (counted from 1).
//! - **A provider:** it registers as `kind: "fake"`, CN-01's mock connector for the LLM capability. Its endpoint is
//!   the script, as JSON (anything else gives an empty script, which fails every turn).
#![deny(missing_docs)]

pub mod conformance;
mod fake;
mod script;

pub use fake::FakeLlm;
pub use script::{Expect, Injected, Script, ScriptTurn};

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{CostModel, LlmCapability, Privacy, ProviderConfig, ProviderEnv, ProviderManifest};
use omuzaki_llm_api::usage::ModelInfo;
use omuzaki_llm_api::{LlmClient, register_llm_provider};

omuzaki_core::define_link!();

fn manifest() -> ProviderManifest {
    ProviderManifest::new(vec![LlmCapability::Llm, LlmCapability::Vision], Privacy::Loopback, CostModel::Free, "")
}

fn fake(config: &ProviderConfig) -> Result<FakeLlm, LlmError> {
    if config.endpoint.trim_start().starts_with('{') {
        FakeLlm::from_json(&config.endpoint)
    } else {
        Ok(FakeLlm::new(Script::new(config.id.clone())))
    }
}

fn build(config: &ProviderConfig, _env: &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError> {
    Ok(Box::new(fake(config)?))
}

fn list_models(config: &ProviderConfig, _env: &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError> {
    Ok(vec![fake(config)?.info().clone()])
}

register_llm_provider! { kind: "fake", manifest: manifest, build: build, list_models: list_models }
