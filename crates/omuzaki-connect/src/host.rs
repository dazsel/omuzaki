//! [`Host`]: the one implementation of the `Connectors` service, over the parts an App wires ([`HostParts`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_ai_api::{ConsentLookup, Progress};
use omuzaki_connect_api::{
    Capability, ConfigSource, ConnectError, ConnectorConfig, ConnectorId, Pace, PreviewError, SecretLabel, Status,
    Target,
};
use omuzaki_core::time::Clock;
use omuzaki_http_api::HttpTransport;
use omuzaki_ops_api::Services;
use omuzaki_secrets_api::prelude::{SecretStore, SecretValue};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::set::{ConnectorSet, Entry};
use crate::status::status;

/// What the host needs from the App: every field is something the App already has or wires once.
pub struct HostParts {
    /// The connectors.
    pub set: ConnectorSet,
    /// The person's settings for each ([`SettingsConfig`](crate::SettingsConfig) in the App).
    pub config: Arc<dyn ConfigSource>,
    /// The keychain.
    pub secrets: Arc<dyn SecretStore>,
    /// Where outputs go, and where inputs are read from.
    pub store: Arc<dyn Store>,
    /// The HTTP client (`omuzaki-http`'s `ConnectHttp`); the host wraps it in [`AuthedHttp`](crate::AuthedHttp).
    pub http: Arc<dyn HttpTransport>,
    /// How connectors wait between polls ([`ThreadPace`] in the App).
    pub pace: Arc<dyn Pace>,
    /// The clock candidates are stamped with.
    pub clock: Arc<dyn Clock>,
    /// Consent records, for voice clones.
    pub consent: Arc<dyn ConsentLookup>,
    /// The App's services, passed to connectors that need one (an LLM provider's settings, a registry slot).
    pub services: Services,
}

/// The connector host. Cold: every method runs on a job worker or answers a query.
pub struct Host {
    pub(crate) parts: HostParts,
}

/// Whether a connector's key is present (`connect.key_status`). Never the key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct KeyStatus {
    /// Whether the keychain holds its key.
    pub present: bool,
    /// Whether it needs a key at all.
    pub needs_key: bool,
    /// Its settings are incomplete, so which key it would use is not known yet (an LLM connector with no provider).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unconfigured: bool,
}

/// A connector resolved for one call: its entry, the settings snapshot and the target, resolved once.
pub(crate) struct Resolved<'a> {
    pub(crate) entry: &'a Entry,
    pub(crate) config: ConnectorConfig,
    pub(crate) target: Result<Target, ConnectError>,
}

impl Host {
    /// A host over `parts`.
    pub fn new(parts: HostParts) -> Self {
        Self { parts }
    }

    /// The connectors.
    pub fn set(&self) -> &ConnectorSet {
        &self.parts.set
    }

    /// Whether `id`'s key is present, read from the effective label ([`Target::secret`]) with `contains` only; `None`
    /// for an unknown id.
    pub fn key_status(&self, id: &ConnectorId) -> Option<KeyStatus> {
        let resolved = self.resolve(self.parts.set.get(id)?);
        Some(match &resolved.target {
            Err(_) => KeyStatus { present: false, needs_key: false, unconfigured: true },
            Ok(target) => match &target.secret {
                None => KeyStatus { present: false, needs_key: false, unconfigured: false },
                Some(label) => KeyStatus { present: self.has_key(label), needs_key: true, unconfigured: false },
            },
        })
    }

    /// The settings snapshot and the target of `entry`, resolved now.
    pub(crate) fn resolve<'a>(&self, entry: &'a Entry) -> Resolved<'a> {
        let config = self.parts.config.get(entry.id());
        let target = entry.connector.target(&entry.manifest, &config, &self.parts.services);
        Resolved { entry, config, target }
    }

    /// The status of a resolved connector.
    pub(crate) fn status_of(&self, r: &Resolved<'_>) -> Status {
        let target = r.target.as_ref().ok();
        let key = target.and_then(|t| t.secret.as_ref()).is_some_and(|label| self.has_key(label));
        status(r.entry, &r.config, target, key)
    }

    /// Whether the keychain holds `label`. A keychain that fails counts as missing.
    pub(crate) fn has_key(&self, label: &SecretLabel) -> bool {
        keychain_label(label).is_some_and(|l| self.parts.secrets.contains(&l).unwrap_or(false))
    }

    /// The key behind `label`.
    pub(crate) fn key(&self, label: &SecretLabel) -> Result<SecretValue, ConnectError> {
        let l = keychain_label(label).ok_or_else(|| {
            ConnectError::auth(format!("the key label {} is not one the keychain takes", label.label()))
        })?;
        match self.parts.secrets.get(&l) {
            Ok(Some(value)) => Ok(value),
            Ok(None) => Err(ConnectError::auth("the key is not in the keychain; add it on the Connectors page")),
            Err(e) => Err(ConnectError::unavailable(format!("the keychain did not answer: {e}"))),
        }
    }

    /// The connector a call uses: the named one, or the one enabled connector that declares `capability`.
    pub(crate) fn choose(
        &self,
        capability: Capability,
        connector: Option<&ConnectorId>,
    ) -> Result<&Entry, PreviewError> {
        if let Some(id) = connector {
            let entry = self.parts.set.get(id).ok_or_else(|| PreviewError::UnknownConnector(id.to_string()))?;
            if entry.manifest.declares(capability).is_none() {
                return Err(PreviewError::Unsupported(format!("{id} does not make {capability}")));
            }
            return Ok(entry);
        }
        let enabled: Vec<&Entry> =
            self.parts.set.offering(capability).filter(|e| self.parts.config.get(e.id()).enabled).collect();
        match enabled.as_slice() {
            [] => Err(PreviewError::NoConnector(capability)),
            [one] => Ok(one),
            many => Err(PreviewError::ambiguous(capability, many.iter().map(|e| e.id().clone()).collect())),
        }
    }
}

/// The keychain's label for a settings label: the same text, validated by the keychain's own rules.
fn keychain_label(label: &SecretLabel) -> Option<omuzaki_secrets_api::prelude::SecretLabel> {
    omuzaki_secrets_api::prelude::SecretLabel::new(label.label()).ok()
}

/// The App's [`Pace`]: waits in real time, in slices short enough that a cancel stops the wait within 50 ms.
#[derive(Copy, Clone, Debug, Default)]
pub struct ThreadPace;

/// The longest slice a wait sleeps before it checks for a cancel.
const PACE_SLICE: Duration = Duration::from_millis(50);

impl Pace for ThreadPace {
    fn wait(&self, seconds: f64, progress: &dyn Progress) {
        if !(seconds.is_finite() && seconds > 0.0) {
            return;
        }
        let Some(until) = Instant::now().checked_add(Duration::from_secs_f64(seconds.min(86_400.0))) else {
            return;
        };
        while !progress.is_cancelled() {
            let now = Instant::now();
            if now >= until {
                return;
            }
            std::thread::sleep((until - now).min(PACE_SLICE));
        }
    }
}
