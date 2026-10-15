//! Two registrations of one provider kind are a configuration error: lookup and the startup check refuse them and
//! name both, instead of returning whichever came first in link order.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{
    CostModel, LlmCapability, Privacy, ProviderConfig, ProviderEnv, ProviderManifest, RegistryError, check_providers,
    provider,
};
use omuzaki_llm_api::register_llm_provider;
use omuzaki_llm_api::usage::ModelInfo;

fn manifest() -> ProviderManifest {
    ProviderManifest::new(vec![LlmCapability::Llm], Privacy::Loopback, CostModel::Free, "")
}

fn build(_: &ProviderConfig, _: &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError> {
    Err(LlmError::Unsupported("test".into()))
}

#[allow(clippy::unnecessary_wraps)] // the registered fn type returns a Result
fn list_models(_: &ProviderConfig, _: &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError> {
    Ok(Vec::new())
}

register_llm_provider! { kind: "twin", manifest: manifest, build: build, list_models: list_models }
register_llm_provider! { kind: "twin", manifest: manifest, build: build, list_models: list_models }
register_llm_provider! { kind: "solo", manifest: manifest, build: build, list_models: list_models }

#[test]
fn a_kind_registered_twice_is_refused_at_lookup_naming_both() {
    let Err(RegistryError::DuplicateKind { kind, first, second }) = provider("twin") else {
        panic!("a duplicate kind must not resolve");
    };
    assert_eq!(kind, "twin");
    assert!(first.starts_with("omuzaki-llm-api (") && first.contains("duplicate_kinds.rs:"), "{first}");
    assert!(second.contains("duplicate_kinds.rs:"), "{second}");
    assert_ne!(first, second);
}

#[test]
fn the_startup_check_refuses_the_duplicate_and_says_which() {
    let message = check_providers().unwrap_err().to_string();
    assert!(message.contains("\"twin\"") && message.contains("duplicate_kinds.rs:"), "{message}");
}

#[test]
fn other_kinds_still_resolve_and_unknown_kinds_are_none() {
    assert_eq!(provider("solo").unwrap().unwrap().kind, "solo");
    assert!(provider("missing").unwrap().is_none());
}
