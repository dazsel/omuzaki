//! Providers: configuration, the manifest and `register_llm_provider!` (llm-api D6, D8).

use std::any::Any;
use std::sync::Arc;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::LlmClient;
use crate::error::LlmError;
use crate::usage::{ModelInfo, PriceTable};

/// The name of a secret in the secret store (`connector/llm/<provider id>`). It names a secret; it never holds one.
///
/// A local stand-in: the design puts this in `omuzaki-secrets-api` (#784). It moves there, and this crate
/// re-exports it, when that crate lands.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SecretLabel(pub String);

/// A setting's reference to a secret (settings design): the label, with no way to read the value.
///
/// A local stand-in for `omuzaki-settings-api::SecretRef` (#281); same move as [`SecretLabel`].
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub struct SecretRef(pub SecretLabel);

/// A secret's value. `Debug` never shows it.
///
/// A local stand-in for the secret store's `SecretValue` (#784).
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wraps a value.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The value, for building one request header and nothing else.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretValue(<redacted>)")
    }
}

/// The read side of the secret store a provider needs. A local stand-in for `SecretStore` (#784).
pub trait SecretStore: Send + Sync {
    /// The secret named `label`, if the store has one.
    fn get(&self, label: &SecretLabel) -> Option<SecretValue>;
}

/// An opaque handle to the host's async runtime. The API crate has no runtime; a provider downcasts the handle to
/// the one it expects (`omuzaki-llm`: a tokio handle).
#[derive(Clone)]
pub struct RuntimeHandle(Arc<dyn Any + Send + Sync>);

impl RuntimeHandle {
    /// Wraps the host's runtime handle.
    pub fn new<T: Any + Send + Sync>(handle: T) -> Self {
        Self(Arc::new(handle))
    }

    /// A handle for providers that need no runtime, such as the fake.
    pub fn none() -> Self {
        Self::new(())
    }

    /// The wrapped handle, if it is a `T`.
    pub fn downcast_ref<T: Any>(&self) -> Option<&T> {
        self.0.downcast_ref()
    }
}

/// What a provider is built from; one entry of the `llm.providers` setting.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderConfig {
    /// The entry's id, also the key label's last part.
    pub id: String,
    /// The registered provider kind (`anthropic`, `fake`...).
    pub kind: String,
    /// Where the provider is.
    pub endpoint: String,
    /// The key, by reference.
    pub key: Option<SecretRef>,
    /// Allow a non-loopback OpenAI-compatible endpoint.
    pub allow_remote: bool,
    /// Prices that replace the shipped table.
    pub prices: Option<PriceTable>,
    /// A context window that replaces the probed one.
    pub context_window: Option<u64>,
    /// How often to retry.
    pub retry: RetryPolicy,
}

/// How often the client retries before the first event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RetryPolicy {
    /// At most this many retries; 0 turns retrying off.
    pub max_retries: u8,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self { max_retries: 4 }
    }
}

/// What a provider is built with besides its configuration.
#[derive(Clone)]
pub struct ProviderEnv {
    /// Where keys are resolved, per request.
    pub secrets: Arc<dyn SecretStore>,
    /// The host's runtime.
    pub runtime: RuntimeHandle,
}

/// What a provider can do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum LlmCapability {
    /// Text in, text and tool calls out.
    Llm,
    /// Also accepts images.
    Vision,
}

/// What leaves the machine (llm-api D7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Privacy {
    /// A hosted service gets the prompt, tool results and the key.
    Hosted,
    /// A model on this machine; nothing leaves.
    Loopback,
}

/// How a provider charges.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum CostModel {
    /// Per token, from a price table.
    Metered,
    /// No charge.
    Free,
}

/// What the connector framework reads about a provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderManifest {
    /// What it can do.
    pub capabilities: Vec<LlmCapability>,
    /// What leaves the machine.
    pub privacy: Privacy,
    /// How it charges.
    pub cost: CostModel,
    /// A link to the provider's terms for its output.
    pub output_terms: String,
}

/// Builds a client from its configuration.
pub type BuildFn = fn(&ProviderConfig, &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError>;
/// Lists the models a provider serves; blocking, for the Settings page only.
pub type ListModelsFn = fn(&ProviderConfig, &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError>;

/// One registered provider, made by [`register_llm_provider!`](crate::register_llm_provider).
pub struct LlmProvider {
    /// The kind a [`ProviderConfig`] names.
    pub kind: &'static str,
    /// What the connector framework reads.
    pub manifest: fn() -> ProviderManifest,
    /// Builds a client.
    pub build: BuildFn,
    /// Lists models.
    pub list_models: ListModelsFn,
}

inventory::collect!(LlmProvider);

/// Every provider linked into this binary, sorted by kind so the order never depends on link order.
pub fn providers() -> Vec<&'static LlmProvider> {
    let mut all: Vec<&'static LlmProvider> = inventory::iter::<LlmProvider>.into_iter().collect();
    all.sort_by_key(|p| p.kind);
    all
}

/// The provider registered under `kind`.
pub fn provider(kind: &str) -> Option<&'static LlmProvider> {
    providers().into_iter().find(|p| p.kind == kind)
}

/// Registers an [`LlmProvider`] so [`providers`] finds it, and records it for the link gate (ADR 0015).
///
/// ```ignore
/// omuzaki_llm_api::register_llm_provider! {
///     kind: "anthropic", manifest: manifest, build: build, list_models: list_models,
/// }
/// ```
#[macro_export]
macro_rules! register_llm_provider {
    (kind: $kind:literal, manifest: $manifest:expr, build: $build:expr, list_models: $list:expr $(,)?) => {
        $crate::__inventory::submit! {
            $crate::provider::LlmProvider { kind: $kind, manifest: $manifest, build: $build, list_models: $list }
        }
        $crate::__record_registration!("llm_provider", $kind);
    };
}
