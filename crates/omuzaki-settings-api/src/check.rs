//! The test every crate that registers settings runs (the `check_crate!` pattern of l10n-ops §10.1).
//!
//! ```ignore
//! // crates/<crate>/tests/settings.rs
//! omuzaki_settings_api::check_crate!();
//! ```
//!
//! It fails a key with a bad name, a key that is a prefix of another, a default that fails its own `check` or does
//! not round-trip through TOML, a missing or multi-line summary, a `Machine` key marked `Member`, a version of 0, a
//! bad `RENAMED_FROM`, and a schema containing `format: "secret"`. The problems are plain strings, so
//! [`problems`] is also how this crate tests the checker itself.

use std::fmt::Write as _;

use omuzaki_doc_api::Value;

use crate::meta::{InProfile, SettingScope};
use crate::registry::{self, SettingRegistration};

/// Runs the checks for the settings registered by `package` (`env!("CARGO_PKG_NAME")`) and panics with every problem.
///
/// # Panics
/// When there is any problem: this is a test helper.
#[allow(clippy::panic)]
pub fn assert_crate(package: &str) {
    let all: Vec<_> = registry::registrations().collect();
    let found = problems(&all, package);
    assert!(found.is_empty(), "settings problems in `{package}`:\n  {}", found.join("\n  "));
}

/// Expands to the test that checks the settings registered by the calling crate.
#[macro_export]
macro_rules! check_crate {
    () => {
        #[test]
        fn registered_settings_are_well_formed() {
            $crate::check::assert_crate(::core::env!("CARGO_PKG_NAME"));
        }
    };
}

/// Every problem with the registrations of `package` among `all` (which must hold every registration, since a prefix
/// clash can be with another crate's key). Empty means well formed.
#[must_use]
pub fn problems(all: &[&SettingRegistration], package: &str) -> Vec<String> {
    let mut out = Vec::new();
    for reg in all.iter().filter(|r| r.package == package) {
        reg_problems(reg, all, &mut out);
    }
    out
}

fn valid_key(key: &str) -> bool {
    let segment = |s: &str| {
        let mut chars = s.chars();
        chars.next().is_some_and(|c| c.is_ascii_lowercase())
            && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
    };
    key.split('.').all(segment) && key.contains('.')
}

fn reg_problems(reg: &SettingRegistration, all: &[&SettingRegistration], out: &mut Vec<String>) {
    let meta = &reg.meta;
    let key = meta.key;
    let mut say = |msg: &str| out.push(format!("{key}: {msg}"));

    if !valid_key(key) {
        say("the key must be `<area>.<name>` in lowercase snake segments, at least two");
    }
    for other in all.iter().filter(|o| !std::ptr::eq(**o, reg)) {
        let o = other.meta.key;
        if o == key {
            say("registered twice");
        } else if o.strip_prefix(key).is_some_and(|rest| rest.starts_with('.')) {
            say(&format!("is a prefix of `{o}`"));
        }
    }
    if meta.summary.trim().is_empty() || meta.summary.contains('\n') {
        say("the summary must be one non-empty line");
    }
    if meta.version == 0 {
        say("the version starts at 1");
    }
    if meta.scope == SettingScope::Machine && meta.profile == InProfile::Member {
        say("a Machine key cannot be a profile member: profiles roam, machines do not");
    }
    for old in reg.renamed_from {
        if !valid_key(old) || all.iter().any(|o| o.meta.key == *old) {
            say(&format!("RENAMED_FROM `{old}` is not a valid retired key"));
        }
    }
    let default = reg.default_json();
    if let Err(e) = reg.validate(&default) {
        say(&format!("the default fails its own check: {e}"));
    }
    if let Err(e) = toml_round_trip(key, &default) {
        say(&e);
    }
    if has_secret_format(&serde_json::to_value(reg.schema()).unwrap_or(Value::Null)) {
        say("the schema has `format: \"secret\"`: hold a SecretRef, never a secret");
    }
}

/// The default, written as TOML the way the store nests it and read back, must be the same value. `null` (an
/// `Option::None` default) cannot be written, which is the right answer: TOML has no null, so absent means default.
fn toml_round_trip(key: &str, default: &Value) -> Result<(), String> {
    let mut wrapped = serde_json::Map::new();
    wrapped.insert("v".into(), default.clone());
    let text = toml::to_string(&Value::Object(wrapped.clone())).map_err(|e| {
        let mut m = String::new();
        let _ = write!(m, "the default cannot be written as TOML ({e})");
        m
    })?;
    let back: Value = toml::from_str(&text).map_err(|e| format!("the default does not read back from TOML ({e})"))?;
    if back == Value::Object(wrapped) {
        Ok(())
    } else {
        Err(format!("the default does not round-trip through TOML (`{key}` wrote `{}`)", text.trim()))
    }
}

fn has_secret_format(schema: &Value) -> bool {
    match schema {
        Value::Object(map) => {
            map.get("format").and_then(Value::as_str) == Some("secret") || map.values().any(has_secret_format)
        }
        Value::Array(items) => items.iter().any(has_secret_format),
        _ => false,
    }
}
