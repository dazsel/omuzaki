//! The contract end to end: a declared setting appears in the registry, values are validated with the key named,
//! the wire forms round-trip, and `check_crate!` catches each kind of bad key.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used, clippy::assert_is_empty)]

use omuzaki_settings_api::check::problems;
use omuzaki_settings_api::error::{SettingError, SettingErrorKind};
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::registry::{self, SettingRegistration};
use omuzaki_settings_api::secret::SecretRef;
use omuzaki_settings_api::service::Snapshot;
use omuzaki_settings_api::setting::Setting;
use omuzaki_settings_api::{check_crate, register_setting};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

macro_rules! key {
    ($name:ident, $ty:ty, $key:literal, $default:expr) => {
        key!($name, $ty, $key, $default, "A test key", SettingScope::User, InProfile::Member);
    };
    ($name:ident, $ty:ty, $key:literal, $default:expr, $summary:literal, $scope:expr, $profile:expr) => {
        struct $name;
        impl Setting for $name {
            type Value = $ty;
            const META: SettingMeta =
                SettingMeta::new($key, $summary, $scope, AgentWrite::Free, $profile, Apply::Live, 1);
            fn default() -> $ty {
                $default
            }
        }
    };
}

/// UI scale in percent, 75 to 200.
struct Scale;
impl Setting for Scale {
    type Value = u16;
    const META: SettingMeta = SettingMeta::new(
        "ui.scale",
        "UI scale in percent",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        2,
    );
    const RENAMED_FROM: &'static [&'static str] = &["ui.zoom"];
    fn default() -> u16 {
        100
    }
    fn check(v: &u16) -> Result<(), SettingError> {
        SettingError::range("ui.scale", *v, 75, 200)
    }
    #[allow(clippy::cast_possible_truncation)]
    fn migrate(from: u32, old: serde_json::Value) -> Result<serde_json::Value, SettingError> {
        // Version 1 stored a fraction (1.25); version 2 stores percent.
        match (from, old.as_f64()) {
            (1, Some(f)) => Ok(json!(u16::try_from((f * 100.0).round() as i64).unwrap_or(u16::MAX))),
            _ => Ok(old),
        }
    }
}
register_setting!(Scale);

/// A key with a reference to a secret, to show it is a plain string in the schema.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
struct Connector {
    api_key: Option<SecretRef>,
}
struct Conn;
impl Setting for Conn {
    type Value = Connector;
    const META: SettingMeta = SettingMeta::new(
        "llm.connector",
        "LLM connector",
        SettingScope::User,
        AgentWrite::Never,
        InProfile::Excluded,
        Apply::Live,
        1,
    );
    fn default() -> Connector {
        Connector { api_key: Some(SecretRef::new("connector/openai").unwrap()) }
    }
}
register_setting!(Conn);

check_crate!();

#[test]
fn a_declared_setting_appears_in_the_registry_sorted_by_key() {
    let keys: Vec<_> = registry::registrations().map(|r| r.meta.key).collect();
    assert_eq!(keys, ["llm.connector", "ui.scale"]);
    let reg = registry::find("ui.scale").unwrap();
    assert_eq!((reg.package, reg.meta.version, reg.renamed_from), ("omuzaki-settings-api", 2, &["ui.zoom"][..]));
    assert!(registry::find("ui").is_none() && registry::find("ui.scale.x").is_none());
    assert_eq!(reg.default_json(), json!(100));
}

#[test]
fn a_wrong_type_and_an_out_of_range_value_are_rejected_with_the_key_named() {
    let reg = registry::find("ui.scale").unwrap();
    assert!(reg.validate(&json!(125)).is_ok());
    let wrong = reg.validate(&json!("big")).unwrap_err();
    assert!(matches!(wrong.kind, SettingErrorKind::WrongType { .. }));
    assert!(wrong.to_string().contains("`ui.scale`"), "{wrong}");
    let range = reg.validate(&json!(900)).unwrap_err();
    assert_eq!(range.to_string(), "`ui.scale` = 900 is out of range 75 to 200");
    assert!(reg.validate(&json!(-1)).is_err() && reg.validate(&json!(70000)).is_err());
}

#[test]
fn migration_runs_through_the_registration() {
    let reg = registry::find("ui.scale").unwrap();
    assert_eq!(reg.migrate(1, json!(1.25)).unwrap(), json!(125));
    assert_eq!(reg.migrate(2, json!(125)).unwrap(), json!(125));
}

#[test]
fn the_schema_is_derived_and_a_secret_ref_is_a_plain_string() {
    let schema = serde_json::to_string(&registry::find("llm.connector").unwrap().schema()).unwrap();
    assert!(schema.contains("SecretRef") && !schema.contains("\"secret\""), "{schema}");
    let reg = registry::find("ui.scale").unwrap();
    assert_eq!(serde_json::to_value(reg.schema()).unwrap()["type"], "integer");
}

#[test]
fn a_snapshot_of_defaults_reads_typed_values() {
    let snap = Snapshot::defaults();
    assert_eq!(snap.get::<Scale>().unwrap(), 100);
    assert_eq!(snap.get::<Conn>().unwrap().api_key.unwrap().label(), "connector/openai");
    assert_eq!(snap.iter().count(), 2);
}

// --- the checker -------------------------------------------------------------------------------------------------

key!(BadName, u8, "Scale", 1);
key!(OneSegment, u8, "scale", 1);
key!(EmptySegment, u8, "ui..x", 1);
key!(Parent, u8, "mascot.size", 1);
key!(Child, u8, "mascot.size.max", 1);
key!(NoSummary, u8, "ui.nosummary", 1, "", SettingScope::User, InProfile::Member);
key!(MachineMember, u8, "audio.device", 1, "Device", SettingScope::Machine, InProfile::Member);
key!(NoneDefault, Option<u8>, "ui.maybe", None);
key!(Fine, u8, "ui.fine", 1);

struct BadDefault;
impl Setting for BadDefault {
    type Value = u8;
    const META: SettingMeta = SettingMeta::new(
        "ui.bad_default",
        "Bad",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    fn default() -> u8 {
        9
    }
    fn check(v: &u8) -> Result<(), SettingError> {
        SettingError::range("ui.bad_default", *v, 0, 5)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("format" = "secret"))]
struct Leaky(String);
key!(Secretive, Leaky, "ui.token", Leaky(String::new()));

struct BadRename;
impl Setting for BadRename {
    type Value = u8;
    const META: SettingMeta = SettingMeta::new(
        "ui.renamed",
        "Renamed",
        SettingScope::User,
        AgentWrite::Free,
        InProfile::Member,
        Apply::Live,
        1,
    );
    const RENAMED_FROM: &'static [&'static str] = &["ui.fine"];
    fn default() -> u8 {
        1
    }
}

fn found<S: Setting>(others: &[&SettingRegistration]) -> Vec<String> {
    let me = SettingRegistration::of::<S>("t");
    let mut all = vec![&me];
    all.extend(others);
    problems(&all, "t")
}

#[test]
fn the_checker_catches_each_bad_key_case() {
    let has = |p: Vec<String>, needle: &str| {
        assert!(p.iter().any(|m| m.contains(needle)), "expected `{needle}` in {p:?}");
    };
    has(found::<BadName>(&[]), "lowercase snake segments");
    has(found::<OneSegment>(&[]), "at least two");
    has(found::<EmptySegment>(&[]), "lowercase snake segments");
    has(found::<NoSummary>(&[]), "summary");
    has(found::<MachineMember>(&[]), "Machine key cannot be a profile member");
    has(found::<BadDefault>(&[]), "default fails its own check");
    has(found::<NoneDefault>(&[]), "TOML");
    has(found::<Secretive>(&[]), "format: \"secret\"");
    let child = SettingRegistration::of::<Child>("other");
    has(found::<Parent>(&[&child]), "is a prefix of `mascot.size.max`");
    let fine = SettingRegistration::of::<Fine>("other");
    has(found::<BadRename>(&[&fine]), "RENAMED_FROM");
}

#[test]
fn the_checker_passes_a_good_key_and_looks_only_at_its_own_package() {
    let none: Vec<String> = Vec::new();
    assert_eq!(found::<Fine>(&[]), none);
    let bad = SettingRegistration::of::<BadName>("other");
    let me = SettingRegistration::of::<Fine>("t");
    assert_eq!(problems(&[&me, &bad], "t"), none);
}

#[test]
fn registering_twice_is_a_problem() {
    let a = SettingRegistration::of::<Fine>("t");
    let b = SettingRegistration::of::<Fine>("t");
    assert!(problems(&[&a, &b], "t").iter().any(|m| m.contains("registered twice")));
}
