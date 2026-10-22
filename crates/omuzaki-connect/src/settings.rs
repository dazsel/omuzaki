//! The connectors setting (D12): one key, `connectors.entries`, a map from connector id to the person's
//! [`ConnectorConfig`], and [`SettingsConfig`], the host's [`ConfigSource`] over the settings service.

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_connect_api::net::Endpoint;
use omuzaki_connect_api::{ConfigSource, ConnectorConfig, ConnectorId};
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Settings, SettingsExt};
use omuzaki_settings_api::setting::Setting;

/// The key.
pub const KEY: &str = "connectors.entries";

/// `connectors.entries`: each connector's settings, by id (enabled, endpoint, plan tier, a program's command line,
/// declared model licences, an LLM connector's provider).
///
/// `Machine` scope: an endpoint, a command line and a plan tier are about this computer and this account. Agents
/// never write it (`AgentWrite::Never`): an endpoint decides where data goes, a command line what runs, and a licence
/// declaration what the outputs may be used for, so only a person sets them (D5). It never holds a key: keys live in
/// the keychain under `connector/<id>`.
pub struct ConnectorSettings;

impl Setting for ConnectorSettings {
    type Value = BTreeMap<String, ConnectorConfig>;
    const META: SettingMeta = SettingMeta::new(
        KEY,
        "Each connector's settings: enabled, endpoint, plan tier, command line, declared model licences",
        SettingScope::Machine,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Self::Value {
        BTreeMap::new()
    }

    fn check(value: &Self::Value) -> Result<(), SettingError> {
        for (id, config) in value {
            check_entry(id, config).map_err(|why| SettingError::invalid(KEY, format!("{id}: {why}")))?;
        }
        Ok(())
    }
}

register_setting!(ConnectorSettings);

/// The first rule `config` breaks, in words.
fn check_entry(id: &str, config: &ConnectorConfig) -> Result<(), String> {
    ConnectorId::new(id).map_err(|e| format!("not a connector id ({e})"))?;
    if let Some(url) = &config.endpoint
        && Endpoint::parse(url).is_none()
    {
        return Err(format!("the endpoint {url:?} is not an http:// or https:// address with a host"));
    }
    if let Some(command) = &config.command
        && command.first().is_none_or(|program| program.trim().is_empty())
    {
        return Err("the command line names no program".to_owned());
    }
    if let Some(plan) = &config.plan
        && !is_slug(plan)
    {
        return Err(format!("the plan {plan:?} is not a short lower-case name like \"pro\""));
    }
    if let Some(provider) = &config.provider
        && provider.trim().is_empty()
    {
        return Err("the provider is empty".to_owned());
    }
    for (model, declared) in &config.models {
        if model.trim().is_empty() {
            return Err("a declared model has no name".to_owned());
        }
        if declared.spdx.trim().is_empty() {
            return Err(format!("the model {model:?} declares no licence"));
        }
    }
    Ok(())
}

/// Lower-case letters, digits, `-`, `_` and `.`, 1 to 64 of them.
fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 64
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.".contains(&b))
}

/// A [`ConfigSource`] over the settings service: each read takes the current snapshot (cheap: an `Arc`), so a
/// change is seen by the next run. An id with no entry is the default: disabled.
pub struct SettingsConfig {
    settings: Arc<dyn Settings>,
}

impl SettingsConfig {
    /// Reads `settings`.
    pub fn new(settings: Arc<dyn Settings>) -> Self {
        Self { settings }
    }
}

impl ConfigSource for SettingsConfig {
    fn get(&self, id: &ConnectorId) -> ConnectorConfig {
        match self.settings.get::<ConnectorSettings>() {
            Ok(all) => all.get(id.as_str()).cloned().unwrap_or_default(),
            Err(error) => {
                tracing::warn!(%error, "the connectors setting did not read; every connector counts as disabled");
                ConnectorConfig::default()
            }
        }
    }
}
