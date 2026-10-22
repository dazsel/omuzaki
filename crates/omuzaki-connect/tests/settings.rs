//! `connectors.entries`: well formed, checked rule by rule, kept through the TOML the settings store writes, never
//! writable by an agent, and read by the host through [`SettingsConfig`] on every call.
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_connect::{ConnectorSettings, SettingsConfig};
use omuzaki_connect_api::{ConfigSource, ConnectorConfig, ConnectorId, DeclaredModel};
use omuzaki_ops_api::{Actor, Mode};
use omuzaki_settings_api::meta::{AgentWrite, SettingScope};
use omuzaki_settings_api::service::{Layer, SettingsExt};
use omuzaki_settings_api::setting::Setting as _;
use omuzaki_settings_mock::MemorySettings;
use omuzaki_store_api::Commercial;
use serde_json::{Value, json};

omuzaki_settings_api::check_crate!(omuzaki_connect);

fn person() -> Actor {
    Actor::Person { name: "test".into() }
}

fn one(id: &str, config: ConnectorConfig) -> BTreeMap<String, ConnectorConfig> {
    BTreeMap::from([(id.to_owned(), config)])
}

fn full() -> ConnectorConfig {
    ConnectorConfig {
        enabled: true,
        endpoint: Some("http://127.0.0.1:8188".into()),
        plan: Some("pro".into()),
        command: Some(vec!["comfy".into(), "--port".into(), "8188".into()]),
        models: BTreeMap::from([(
            "sdxl-anime".to_owned(),
            DeclaredModel { spdx: "OpenRAIL-M".into(), commercial: Commercial::Ok },
        )]),
        provider: Some("local".into()),
    }
}

#[test]
fn only_a_person_sets_it_and_it_stays_on_this_computer() {
    assert_eq!(ConnectorSettings::META.agent, AgentWrite::Never);
    assert_eq!(ConnectorSettings::META.scope, SettingScope::Machine);
    assert!(ConnectorSettings::default().is_empty());
}

#[test]
fn each_rule_passes_its_good_value_and_refuses_its_bad_one() {
    assert!(ConnectorSettings::check(&one("comfyui", full())).is_ok());
    assert!(ConnectorSettings::check(&one("comfyui", ConnectorConfig::default())).is_ok());
    let bad: Vec<(&str, ConnectorConfig, &str)> = vec![
        ("Not An Id", ConnectorConfig::default(), "not a connector id"),
        ("comfyui", ConnectorConfig { endpoint: Some("ftp://host".into()), ..full() }, "endpoint"),
        ("comfyui", ConnectorConfig { endpoint: Some("http://".into()), ..full() }, "endpoint"),
        ("comfyui", ConnectorConfig { command: Some(vec![]), ..full() }, "names no program"),
        ("comfyui", ConnectorConfig { command: Some(vec!["  ".into()]), ..full() }, "names no program"),
        ("comfyui", ConnectorConfig { plan: Some("Pro Plan".into()), ..full() }, "plan"),
        ("comfyui", ConnectorConfig { provider: Some(" ".into()), ..full() }, "provider is empty"),
        (
            "comfyui",
            ConnectorConfig {
                models: BTreeMap::from([(
                    "m".to_owned(),
                    DeclaredModel { spdx: String::new(), commercial: Commercial::Unknown },
                )]),
                ..full()
            },
            "declares no licence",
        ),
        (
            "comfyui",
            ConnectorConfig {
                models: BTreeMap::from([(
                    String::new(),
                    DeclaredModel { spdx: "MIT".into(), commercial: Commercial::Ok },
                )]),
                ..full()
            },
            "has no name",
        ),
    ];
    for (id, config, why) in bad {
        let e = ConnectorSettings::check(&one(id, config.clone())).expect_err(why);
        assert!(e.to_string().contains(why), "{id} {config:?}: {e}");
    }
}

#[test]
fn it_round_trips_through_toml() {
    let value = BTreeMap::from([("comfyui".to_owned(), full()), ("test-echo".to_owned(), ConnectorConfig::default())]);
    let json = serde_json::to_value(&value).unwrap();
    let text = toml::to_string(&json!({ ConnectorSettings::META.key: json })).expect("no nulls");
    let back: Value = toml::from_str(&text).unwrap();
    let read: BTreeMap<String, ConnectorConfig> =
        serde_json::from_value(back[ConnectorSettings::META.key].clone()).unwrap();
    assert_eq!(read, value);
}

#[test]
fn the_host_reads_the_current_value_on_every_call() {
    let settings = Arc::new(MemorySettings::default());
    let config = SettingsConfig::new(settings.clone());
    let id = ConnectorId::new("comfyui").unwrap();
    assert_eq!(config.get(&id), ConnectorConfig::default(), "no entry: disabled");
    settings.set::<ConnectorSettings>(&one("comfyui", full()), Layer::Auto, Mode::Apply, &person()).unwrap();
    assert_eq!(config.get(&id), full());
    assert_eq!(config.get(&ConnectorId::new("other").unwrap()), ConnectorConfig::default());
    let refused = settings.set::<ConnectorSettings>(
        &one("comfyui", ConnectorConfig { endpoint: Some("nope".into()), ..full() }),
        Layer::Auto,
        Mode::Apply,
        &person(),
    );
    assert!(refused.is_err());
    assert_eq!(config.get(&id), full(), "a refused write changes nothing");
}
