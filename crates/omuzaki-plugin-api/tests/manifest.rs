//! `plugin.toml` (#1211, design `plugins.md` §7): a full manifest parses clean, every rule fires with its id and path,
//! the shape errors name their path, and the licence table classifies as the policy says.

use std::fmt::Write as _;

use omuzaki_ops_api::Scope;
use omuzaki_plugin_api::manifest::{AgentRule, LicenceClass, Manifest, SettingScope, SettingType, Severity};

/// Every field the design names, valid.
const FULL: &str = r#"
id = "example"
version = "0.1.0"
api = "^0.1"
licence = "MIT"
name = "Example"
description = "Waves when the project opens."
worlds = ["plugin"]
component = "plugin.wasm"
publisher = { name = "Example", key = "ed25519:4b1f00aa11bb22cc" }

[requests]
scopes = ["read", "perform", "publish", "net"]
net_hosts = ["example.com"]
data = true
data_quota_mb = 64

[[settings]]
key = "greeting"
type = "string"
default = "hello"
scope = "user"
agent = "confirm"
"#;

/// `FULL` with `from` replaced by `to` (which must occur exactly once).
fn edited(from: &str, to: &str) -> String {
    assert_eq!(FULL.matches(from).count(), 1, "{from}");
    FULL.replacen(from, to, 1)
}

#[test]
fn a_full_manifest_parses_and_checks_clean() {
    let manifest = Manifest::parse(FULL).unwrap();
    assert_eq!(manifest.id.as_str(), "example");
    assert_eq!(manifest.requests.scopes, [Scope::Read, Scope::Perform, Scope::Publish, Scope::Net]);
    assert_eq!(manifest.settings[0].kind, Some(SettingType::String));
    assert_eq!((manifest.settings[0].scope, manifest.settings[0].agent), (SettingScope::User, AgentRule::Confirm));
    let report = manifest.check();
    assert!(report.is_clean(), "{:?}", report.findings);
    assert_eq!(manifest.licence_class(), LicenceClass::Allowed);
}

#[test]
fn defaults_fill_what_a_small_manifest_leaves_out() {
    let small = "id = \"tiny\"\nversion = \"1.0.0\"\napi = \"^0.1\"\nlicence = \"MIT\"\nname = \"Tiny\"\ncomponent = \"t.wasm\"\n";
    let manifest = Manifest::parse(small).unwrap();
    assert_eq!(manifest.worlds, ["plugin"]);
    assert_eq!(manifest.requests.data_quota_mb, 64);
    assert!(manifest.requests.scopes.is_empty() && !manifest.requests.data);
    let ids: Vec<String> = manifest.check().findings.into_iter().map(|f| f.id).collect();
    assert_eq!(ids, ["plugin.unsigned"], "only the unsigned warning");
}

#[test]
fn the_json_schema_is_generated() {
    let schema = serde_json::to_value(schemars::schema_for!(Manifest)).unwrap();
    for key in ["id", "version", "api", "licence", "name", "component", "requests", "publisher", "settings"] {
        assert!(schema["properties"][key].is_object(), "{key}");
    }
}

#[test]
fn shape_errors_name_their_path() {
    let cases = [
        (edited("data = true", "data = true\ncolour = \"red\""), "requests/colour"),
        (edited("name = \"Example\"\n", "name = \"Example\"\nhomepage = \"x\"\n"), "homepage"),
        (edited(r#""perform", "publish""#, r#""perform", "teleport""#), "requests/scopes/2"),
        (edited(r#"id = "example""#, r#"id = "Not A Slug""#), "id"),
        (edited("data = true", "data = \"yes\""), "requests/data"),
        (edited("licence = \"MIT\"\n", ""), "licence"),
    ];
    for (text, path) in cases {
        let error = Manifest::parse(&text).unwrap_err();
        assert_eq!(error.path, path, "{error}");
    }
    assert!(Manifest::parse("id = ").unwrap_err().path.is_empty(), "a syntax error is the whole file");
}

#[test]
fn every_error_rule_fires_with_its_id_and_path() {
    let cases: Vec<(String, &str, &str)> = vec![
        (edited(r#"id = "example""#, r#"id = "plugin""#), "plugin.id_reserved", "id"),
        (edited(r#"version = "0.1.0""#, r#"version = "one""#), "plugin.version", "version"),
        (edited(r#"api = "^0.1""#, r#"api = "about 0.1""#), "plugin.api", "api"),
        (edited(r#"licence = "MIT""#, r#"licence = " ""#), "plugin.licence_missing", "licence"),
        (edited("name = \"Example\"\n", "name = \"\"\n"), "plugin.name_missing", "name"),
        (edited(r#"worlds = ["plugin"]"#, "worlds = []"), "plugin.worlds_empty", "worlds"),
        (edited(r#"worlds = ["plugin"]"#, r#"worlds = ["plugin", "teleporter"]"#), "plugin.world_unknown", "worlds/1"),
        (edited(r#"component = "plugin.wasm""#, r#"component = "../x.wasm""#), "plugin.component_path", "component"),
        (edited("ed25519:4b1f00aa11bb22cc", "rsa:1234"), "plugin.publisher_key", "publisher/key"),
        (edited(r#""publish", "net""#, r#""publish", "native", "net""#), "plugin.native_refused", "requests/scopes/3"),
        (edited(r#""read", "perform""#, r#""read", "fs""#), "plugin.scope_refused", "requests/scopes/1"),
        (edited(r#""read", "perform""#, r#""read", "script""#), "plugin.scope_refused", "requests/scopes/1"),
        (edited(r#"net_hosts = ["example.com"]"#, "net_hosts = []"), "plugin.net_without_hosts", "requests/net_hosts"),
        (edited(r#", "net"]"#, "]"), "plugin.hosts_without_net", "requests/scopes"),
        (edited(r#"["example.com"]"#, r#"["example.com", "10.0.0.1"]"#), "plugin.net_host", "requests/net_hosts/1"),
        (edited(r#"["example.com"]"#, r#"["*.example.com"]"#), "plugin.net_host", "requests/net_hosts/0"),
        (edited(r#"["example.com"]"#, r#"["https://example.com"]"#), "plugin.net_host", "requests/net_hosts/0"),
        (edited("data_quota_mb = 64", "data_quota_mb = 2048"), "plugin.data_quota", "requests/data_quota_mb"),
        (edited("data_quota_mb = 64", "data_quota_mb = 0"), "plugin.data_quota", "requests/data_quota_mb"),
        (edited(r#"key = "greeting""#, r#"key = "Greeting!""#), "plugin.setting_key", "settings/0/key"),
        (edited(r#"default = "hello""#, "default = 3"), "plugin.setting_default", "settings/0/default"),
        (edited(r#"type = "string""#, ""), "plugin.setting_type", "settings/0"),
    ];
    for (text, id, path) in cases {
        let report = Manifest::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}")).check();
        let finding = report.findings.iter().find(|f| f.id == id).unwrap_or_else(|| panic!("{id}: {report:?}"));
        assert_eq!((finding.path.as_str(), finding.severity), (path, Severity::Error), "{id}");
        assert!(report.has_errors());
    }
}

#[test]
fn a_schema_setting_skips_the_simple_type_check() {
    let text = edited(r#"type = "string""#, r#"schema = { type = "string" }"#);
    assert!(Manifest::parse(&text).unwrap().check().is_clean());
}

#[test]
fn too_many_settings_and_a_duplicate_key_are_errors() {
    let mut text = FULL.replace("[[settings]]", "[[ignored]]").split("[[ignored]]").next().unwrap().to_owned();
    for i in 0..=omuzaki_plugin_api::manifest::MAX_SETTINGS {
        writeln!(text, "\n[[settings]]\nkey = \"s{i}\"\ntype = \"bool\"\ndefault = true").unwrap();
    }
    text += "\n[[settings]]\nkey = \"s0\"\ntype = \"bool\"\ndefault = false\n";
    let report = Manifest::parse(&text).unwrap().check();
    let ids: Vec<(&str, &str)> = report.errors().map(|f| (f.id.as_str(), f.path.as_str())).collect();
    assert_eq!(ids, [("plugin.settings_too_many", "settings"), ("plugin.setting_duplicate", "settings/65/key")]);
}

#[test]
fn licences_are_classified_and_warn_when_they_should() {
    let cases = [
        ("MIT", LicenceClass::Allowed, None),
        ("Apache-2.0 OR MIT", LicenceClass::Allowed, None),
        ("LGPL-3.0-only", LicenceClass::Conditions, None),
        ("CC-BY-NC-4.0", LicenceClass::NonCommercial, Some("plugin.licence_noncommercial")),
        ("PolyForm-Noncommercial-1.0.0", LicenceClass::NonCommercial, Some("plugin.licence_noncommercial")),
        ("BUSL-1.1", LicenceClass::NonCommercial, Some("plugin.licence_noncommercial")),
        ("LicenseRef-Mine", LicenceClass::Unknown, Some("plugin.licence_unknown")),
    ];
    for (licence, class, warning) in cases {
        let manifest = Manifest::parse(&edited(r#"licence = "MIT""#, &format!("licence = \"{licence}\""))).unwrap();
        assert_eq!(manifest.licence_class(), class, "{licence}");
        let report = manifest.check();
        let warnings: Vec<&str> = report.warnings().map(|f| f.id.as_str()).collect();
        assert_eq!(warnings, warning.into_iter().collect::<Vec<_>>(), "{licence}");
        assert!(!report.has_errors(), "a licence never refuses a plugin the person installs: {licence}");
    }
}

#[test]
fn no_publisher_is_a_warning_not_an_error() {
    let text = edited("publisher = { name = \"Example\", key = \"ed25519:4b1f00aa11bb22cc\" }\n", "");
    let report = Manifest::parse(&text).unwrap().check();
    let finding = report.findings.iter().find(|f| f.id == "plugin.unsigned").unwrap();
    assert_eq!(finding.severity, Severity::Warning);
    assert!(!report.has_errors());
}
