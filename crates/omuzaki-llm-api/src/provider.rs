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
#[non_exhaustive]
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

impl ProviderConfig {
    /// A configuration for the provider `kind` at `endpoint`, with no key, no remote endpoint allowed, the shipped
    /// prices and context window, and the default retries.
    pub fn new(id: impl Into<String>, kind: impl Into<String>, endpoint: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            kind: kind.into(),
            endpoint: endpoint.into(),
            key: None,
            allow_remote: false,
            prices: None,
            context_window: None,
            retry: RetryPolicy::default(),
        }
    }

    /// Sets the key, by reference.
    #[must_use]
    pub fn with_key(mut self, key: SecretRef) -> Self {
        self.key = Some(key);
        self
    }

    /// Sets whether a non-loopback OpenAI-compatible endpoint is allowed.
    #[must_use]
    pub fn with_allow_remote(mut self, allow_remote: bool) -> Self {
        self.allow_remote = allow_remote;
        self
    }

    /// Sets prices that replace the shipped table.
    #[must_use]
    pub fn with_prices(mut self, prices: PriceTable) -> Self {
        self.prices = Some(prices);
        self
    }

    /// Sets a context window that replaces the probed one.
    #[must_use]
    pub fn with_context_window(mut self, context_window: u64) -> Self {
        self.context_window = Some(context_window);
        self
    }

    /// Sets how often to retry.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

/// How often the client retries before the first event.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct RetryPolicy {
    /// At most this many retries; 0 turns retrying off.
    pub max_retries: u8,
}

impl RetryPolicy {
    /// A policy of at most `max_retries` retries; 0 turns retrying off.
    pub fn new(max_retries: u8) -> Self {
        Self { max_retries }
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(4)
    }
}

/// What a provider is built with besides its configuration.
#[derive(Clone)]
#[non_exhaustive]
pub struct ProviderEnv {
    /// Where keys are resolved, per request.
    pub secrets: Arc<dyn SecretStore>,
    /// The host's runtime.
    pub runtime: RuntimeHandle,
}

impl ProviderEnv {
    /// An environment that resolves keys through `secrets` and runs on `runtime`.
    pub fn new(secrets: Arc<dyn SecretStore>, runtime: RuntimeHandle) -> Self {
        Self { secrets, runtime }
    }
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
#[non_exhaustive]
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

impl ProviderManifest {
    /// A manifest for a provider with these `capabilities`, `privacy` and `cost`, whose output terms are at
    /// `output_terms`.
    pub fn new(
        capabilities: Vec<LlmCapability>,
        privacy: Privacy,
        cost: CostModel,
        output_terms: impl Into<String>,
    ) -> Self {
        Self { capabilities, privacy, cost, output_terms: output_terms.into() }
    }
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
    /// The package that registered it; set by [`register_llm_provider!`](crate::register_llm_provider).
    pub package: &'static str,
    /// Where it was registered, as `file:line`; set by the macro.
    pub site: &'static str,
}

impl LlmProvider {
    /// `package (file:line)`, for errors that name a registration.
    fn origin(&self) -> String {
        format!("{} ({})", self.package, self.site)
    }
}

/// Why the provider registry cannot be used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    /// Two registrations share a kind, so a [`ProviderConfig`] naming it would pick one by link order.
    #[error("two LLM providers are registered as kind \"{kind}\": {first} and {second}")]
    #[non_exhaustive]
    DuplicateKind {
        /// The kind both registered.
        kind: &'static str,
        /// The first registration (by package, then place), as `package (file:line)`.
        first: String,
        /// The second.
        second: String,
    },
}

inventory::collect!(LlmProvider);

/// Every provider linked into this binary, sorted by kind, then package and place, so the order never depends on link
/// order. A kind registered twice appears twice here; [`check_providers`] and [`provider`] refuse it.
pub fn providers() -> Vec<&'static LlmProvider> {
    let mut all: Vec<&'static LlmProvider> = inventory::iter::<LlmProvider>.into_iter().collect();
    all.sort_by_key(|p| (p.kind, p.package, p.site));
    all
}

impl RegistryError {
    /// Two registrations, `first` and `second` (each `package (file:line)`), share the kind `kind`.
    pub fn duplicate_kind(kind: &'static str, first: impl Into<String>, second: impl Into<String>) -> Self {
        Self::DuplicateKind { kind, first: first.into(), second: second.into() }
    }
}

/// Checks that no kind is registered twice. An App calls it once at startup.
///
/// # Errors
/// [`RegistryError::DuplicateKind`], naming the two registrations, for the first kind (in sorted order) that has two.
pub fn check_providers() -> Result<(), RegistryError> {
    first_duplicate(&providers())
}

fn first_duplicate(sorted: &[&LlmProvider]) -> Result<(), RegistryError> {
    match sorted.windows(2).find(|w| w[0].kind == w[1].kind) {
        Some(w) => Err(RegistryError::duplicate_kind(w[0].kind, w[0].origin(), w[1].origin())),
        None => Ok(()),
    }
}

/// The provider registered under `kind`, or `None` if there is none.
///
/// # Errors
/// [`RegistryError::DuplicateKind`] if `kind` is registered twice: it never picks one.
pub fn provider(kind: &str) -> Result<Option<&'static LlmProvider>, RegistryError> {
    let same: Vec<&'static LlmProvider> = providers().into_iter().filter(|p| p.kind == kind).collect();
    first_duplicate(&same)?;
    Ok(same.first().copied())
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
            $crate::provider::LlmProvider {
                kind: $kind,
                manifest: $manifest,
                build: $build,
                list_models: $list,
                package: ::core::env!("CARGO_PKG_NAME"),
                site: ::core::concat!(::core::file!(), ":", ::core::line!()),
            }
        }
        $crate::__record_registration!("llm_provider", $kind);
    };
}
