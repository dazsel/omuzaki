//! A person's settings for a connector (D12) and the [`Target`] a run resolves from them once (D4): where and how
//! the call goes, shared by the preview, the price, the allowed hosts, the key and the client that sends.

use std::collections::BTreeMap;

use omuzaki_store_api::Commercial;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::SecretLabel;
use crate::id::ConnectorId;
use crate::manifest::{AuthSpec, ConnectorManifest};
use crate::net::Endpoint;
use crate::output::ConnectError;

/// A person's choices for one connector: one entry of the `connectors.entries` setting (Machine scope, agents never
/// write it, never in a profile or the project). Every field may be left out; a connector starts disabled.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct ConnectorConfig {
    /// Whether it may run at all.
    pub enabled: bool,
    /// Overrides the manifest's endpoint (a local `ComfyUI` address). A host off this computer means data leaves it;
    /// a plain `http://` one is allowed with a warning and never carries a key (D8, D12).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// The plan tier the person is on, recorded verbatim in provenance.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// For a program the host launches: its argument vector, set only by the person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<Vec<String>>,
    /// Licences the person declares for models a local connector runs (ADR 0016: theirs to state).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub models: BTreeMap<String, DeclaredModel>,
    /// For an LLM connector: the `llm.providers` entry it uses (D4).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// The licence a person declares for a model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DeclaredModel {
    /// SPDX or `LicenseRef-...`.
    pub spdx: String,
    /// Its commercial status.
    pub commercial: Commercial,
}

/// Where connector settings come from: the settings service in the app, `MemoryConfig` in tests.
pub trait ConfigSource: Send + Sync {
    /// The settings of `id`; the default (disabled) when the person has set none.
    fn get(&self, id: &ConnectorId) -> ConnectorConfig;
}

/// Where and how a call goes, resolved once from settings. Built by the connector
/// ([`Connector::target`](crate::Connector::target)), read by the host, and the one value a run uses throughout: a
/// settings change during a run affects the next run only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Target {
    /// The `connectors.entries` snapshot.
    pub config: ConnectorConfig,
    /// The effective endpoint.
    pub endpoint: Option<String>,
    /// Decided on the effective endpoint (D12), or the provider's remoteness for an LLM connector.
    pub leaves_machine: bool,
    /// Where calls go: the manifest's destinations, plus the effective endpoint's host.
    pub destinations: Vec<String>,
    /// The one key label this run uses: `auth.secret` by default, the selected provider's label for an LLM
    /// connector, `None` without auth. Status, `key_status`, the host's transport and scrubbing all read this field.
    pub secret: Option<SecretLabel>,
    /// Adapter-specific resolved settings, opaque to the host.
    pub detail: Value,
}

impl Target {
    /// The default target of an HTTP, MCP, local or fake connector: the endpoint `config` sets, else the manifest's;
    /// an endpoint whose host is off this computer makes the call leave it and adds that host to the destinations;
    /// the key label is the manifest's `auth.secret`.
    ///
    /// # Errors
    /// [`ConnectError::Unavailable`] when the configured endpoint is not an `http://` or `https://` address, or the
    /// manifest's key label is not a valid label.
    pub fn from_manifest(m: &ConnectorManifest, config: &ConnectorConfig) -> Result<Self, ConnectError> {
        let endpoint = config.endpoint.clone().or_else(|| m.endpoint.clone());
        let mut leaves_machine = m.privacy.leaves_machine;
        let mut destinations = m.privacy.destinations.clone();
        if let Some(url) = &endpoint {
            let parsed = Endpoint::parse(url).ok_or_else(|| {
                ConnectError::unavailable(format!(
                    "the endpoint {url:?} of {} is not an http:// or https:// address; fix it in its settings",
                    m.id
                ))
            })?;
            if !parsed.is_loopback() {
                leaves_machine = true;
            }
            if !destinations.iter().any(|d| d.eq_ignore_ascii_case(&parsed.host)) {
                destinations.push(parsed.host);
            }
        }
        let secret = match &m.auth {
            AuthSpec::ApiKey { secret: Some(label), .. } => Some(SecretLabel::new(label.as_str()).map_err(|_| {
                ConnectError::unavailable(format!("the key label {label:?} of {} is not a valid label", m.id))
            })?),
            AuthSpec::ApiKey { secret: None, .. } | AuthSpec::None => None,
        };
        Ok(Self { config: config.clone(), endpoint, leaves_machine, destinations, secret, detail: Value::Null })
    }
}
