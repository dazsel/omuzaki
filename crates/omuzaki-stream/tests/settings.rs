//! The settings this crate registers (`output.window`) are well formed, and what they keep survives the TOML files the
//! settings store writes (`docs/design/settings.md`).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_settings_api::setting::Setting as _;
use omuzaki_stream::ops::window::{Open, OutputWindow};
use serde_json::{Value, json};

omuzaki_settings_api::check_crate!(omuzaki_stream);

/// The value written as TOML under its key, the way the store nests it, and read back.
fn through_toml(value: &Open) -> Open {
    let json = serde_json::to_value(value).unwrap();
    let wrapped = json!({ OutputWindow::META.key: json });
    let text = toml::to_string(&wrapped).expect("an `Open` request must be writable as TOML: TOML has no null");
    let back: Value = toml::from_str(&text).unwrap();
    serde_json::from_value(back[OutputWindow::META.key].clone())
        .unwrap_or_else(|e| panic!("`{text}` does not read back: {e}"))
}

#[test]
fn the_default_request_round_trips_through_toml() {
    assert_eq!(through_toml(&OutputWindow::default()), Open::default());
}

#[test]
fn a_partial_request_round_trips_through_toml() {
    let partial: Open = serde_json::from_value(json!({ "backdrop": "transparent", "scale": 0.5 })).unwrap();
    assert_eq!(through_toml(&partial), partial);
    let full: Open = serde_json::from_value(
        json!({ "backdrop": "key", "key": "#ff00ff", "scale": 0.5, "title": "Cast", "always_on_top": true }),
    )
    .unwrap();
    assert_eq!(through_toml(&full), full);
}

#[test]
fn absent_fields_are_omitted_not_null() {
    assert_eq!(serde_json::to_value(Open::default()).unwrap(), json!({}));
}
