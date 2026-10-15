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

impl Privacy {
    /// The class of a provider whose requests go to `endpoint`: `Loopback` only if the endpoint's host is a loopback
    /// address or `localhost`, `Hosted` for any other host. An endpoint this cannot parse is `Hosted` too: when it
    /// cannot be told, the stricter class applies (llm-api D7).
    pub fn for_endpoint(endpoint: &str) -> Self {
        if is_loopback_endpoint(endpoint) { Self::Loopback } else { Self::Hosted }
    }
}

/// The host of an `http` or `https` endpoint URL, without brackets, userinfo or port; `None` if `endpoint` is not
/// such a URL. Written here because this crate takes no URL dependency and the decision must be the same everywhere.
pub fn endpoint_host(endpoint: &str) -> Option<&str> {
    let rest = endpoint.trim().strip_prefix("https://").or_else(|| endpoint.trim().strip_prefix("http://"))?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    // Anything before the last `@` is userinfo: `http://127.0.0.1@evil.example/` is a request to evil.example.
    let host_port = authority.rsplit('@').next().unwrap_or_default();
    let host = if let Some(bracketed) = host_port.strip_prefix('[') {
        bracketed.split_once(']').filter(|(_, after)| after.is_empty() || after.starts_with(':')).map(|(h, _)| h)?
    } else {
        host_port.split(':').next().unwrap_or_default()
    };
    (!host.is_empty()).then_some(host)
}

/// True if `endpoint` is an `http` or `https` URL whose host is `localhost`, an address in `127.0.0.0/8`, or `::1`
/// (an IPv4-mapped loopback counts). False for a remote host and for anything that does not parse.
pub fn is_loopback_endpoint(endpoint: &str) -> bool {
    use std::net::IpAddr;
    let Some(host) = endpoint_host(endpoint) else { return false };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>().is_ok_and(|ip| ip.to_canonical().is_loopback())
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
    /// What the connector framework reads when no configuration is known: the stricter case.
    pub manifest: fn() -> ProviderManifest,
    /// The privacy class of a provider built from this configuration, if it depends on the configuration (the
    /// OpenAI-compatible provider on a local or a remote endpoint). `None` means [`manifest`](Self::manifest) is
    /// always right.
    pub privacy_for: Option<fn(&ProviderConfig) -> Privacy>,
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
    /// The manifest of the provider that `config` builds: [`manifest`](Self::manifest) with the privacy class
    /// [`privacy_for`](Self::privacy_for) gives for it, so a local endpoint is never declared hosted and a remote one
    /// is never declared local.
    pub fn manifest_for(&self, config: &ProviderConfig) -> ProviderManifest {
        let mut manifest = (self.manifest)();
        if let Some(privacy_for) = self.privacy_for {
            manifest.privacy = privacy_for(config);
        }
        manifest
    }

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
/// A provider whose privacy depends on its configuration adds `privacy_for: fn(&ProviderConfig) -> Privacy` after
/// `manifest`; [`LlmProvider::manifest_for`] then reads the class from it.
///
/// ```ignore
/// omuzaki_llm_api::register_llm_provider! {
///     kind: "anthropic", manifest: manifest, build: build, list_models: list_models,
/// }
/// ```
#[macro_export]
macro_rules! register_llm_provider {
    (kind: $kind:literal, manifest: $manifest:expr, build: $build:expr, list_models: $list:expr $(,)?) => {
        $crate::register_llm_provider!(@submit $kind, $manifest, ::core::option::Option::None, $build, $list);
    };
    (kind: $kind:literal, manifest: $manifest:expr, privacy_for: $privacy:expr, build: $build:expr, list_models: $list:expr $(,)?) => {
        $crate::register_llm_provider!(@submit $kind, $manifest, ::core::option::Option::Some($privacy), $build, $list);
    };
    (@submit $kind:literal, $manifest:expr, $privacy:expr, $build:expr, $list:expr) => {
        $crate::__inventory::submit! {
            $crate::provider::LlmProvider {
                kind: $kind,
                manifest: $manifest,
                privacy_for: $privacy,
                build: $build,
                list_models: $list,
                package: ::core::env!("CARGO_PKG_NAME"),
                site: ::core::concat!(::core::file!(), ":", ::core::line!()),
            }
        }
        $crate::__record_registration!("llm_provider", $kind);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hosted_manifest() -> ProviderManifest {
        ProviderManifest::new(vec![LlmCapability::Llm], Privacy::Hosted, CostModel::Metered, "terms")
    }

    fn follows_the_endpoint(config: &ProviderConfig) -> Privacy {
        Privacy::for_endpoint(&config.endpoint)
    }

    fn build(_: &ProviderConfig, _: &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError> {
        Err(LlmError::Unsupported("test".into()))
    }

    #[allow(clippy::unnecessary_wraps)] // the signature of `ListModelsFn`
    fn list(_: &ProviderConfig, _: &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError> {
        Ok(Vec::new())
    }

    fn provider_with(privacy_for: Option<fn(&ProviderConfig) -> Privacy>) -> LlmProvider {
        LlmProvider {
            kind: "t",
            manifest: hosted_manifest,
            privacy_for,
            build,
            list_models: list,
            package: "test",
            site: "provider.rs:1",
        }
    }

    fn privacy(endpoint: &str) -> Privacy {
        let config = ProviderConfig::new("p", "t", endpoint);
        provider_with(Some(follows_the_endpoint)).manifest_for(&config).privacy
    }

    #[test]
    fn a_loopback_endpoint_is_declared_loopback() {
        for endpoint in [
            "http://127.0.0.1:11434",
            "http://127.1.2.3/v1",
            "http://localhost:8080/v1",
            "http://LOCALHOST/",
            "http://[::1]:8000",
            "https://[::ffff:127.0.0.1]/v1",
            "http://user:pw@127.0.0.1:9/",
        ] {
            assert_eq!(privacy(endpoint), Privacy::Loopback, "{endpoint}");
        }
    }

    #[test]
    fn a_remote_endpoint_is_declared_hosted() {
        for endpoint in [
            "https://api.example.com/v1",
            "http://192.168.1.20:8000",
            "http://10.0.0.1",
            "http://127.0.0.1@evil.example/",
            "http://127.0.0.1.evil.example/",
            "http://localhost.evil.example/",
            "http://[2001:db8::1]/",
        ] {
            assert_eq!(privacy(endpoint), Privacy::Hosted, "{endpoint}");
        }
    }

    #[test]
    fn an_endpoint_that_does_not_parse_fails_closed_to_hosted() {
        for endpoint in ["", "localhost:8080", "ftp://127.0.0.1/", "http://", "http://[::1", "http://:80/", "not a url"]
        {
            assert_eq!(privacy(endpoint), Privacy::Hosted, "{endpoint:?}");
        }
    }

    #[test]
    fn a_provider_without_a_privacy_hook_keeps_its_manifest() {
        let config = ProviderConfig::new("p", "t", "http://127.0.0.1:1");
        assert_eq!(provider_with(None).manifest_for(&config).privacy, Privacy::Hosted);
    }
}
