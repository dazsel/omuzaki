//! `State` keeps what it does not know, `Watermark` orders by epoch first, and the serde shapes and JSON Schemas of the
//! types that cross an op are pinned (`UPDATE_SCHEMAS=1` regenerates `tests/schemas.json`).

// Helpers outside `#[test]` functions may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;

use omuzaki_update_api::Version;
use omuzaki_update_api::clock::Timestamp;
use omuzaki_update_api::error::UpdateError;
use omuzaki_update_api::event::UpdateEvent;
use omuzaki_update_api::hold::HoldReason;
use omuzaki_update_api::release::{Channel, PackageKind, Release};
use omuzaki_update_api::state::{State, Watermark};
use omuzaki_update_api::status::{CheckFailure, RollbackReason, UpdateStatus};
use schemars::schema_for;
use serde::Serialize;
use serde::de::DeserializeOwned;

const STATE: &str = r#"
current = "1.4.0"
previous = "1.3.2"
attempts = 1
trial = true
future_top_level = "kept"
rolled_back_from = ["1.5.0", "failed_to_start_twice"]

[watermarks.stable]
epoch = 1
sequence = 42
future_inside = 7

[future_table]
a = 1
"#;

#[test]
fn unknown_keys_survive_a_round_trip() {
    let state = State::from_toml(STATE).unwrap();
    assert_eq!(state.current, Version::new(1, 4, 0));
    assert_eq!(state.rolled_back_from, Some((Version::new(1, 5, 0), RollbackReason::FailedToStartTwice)));
    let again = State::from_toml(&state.to_toml()).unwrap();
    assert_eq!(again, state);
    assert_eq!(again.unknown["future_top_level"].as_str(), Some("kept"));
    assert_eq!(again.unknown["future_table"]["a"].as_integer(), Some(1));
    assert_eq!(again.unknown["watermarks"]["stable"]["future_inside"].as_integer(), Some(7));
    assert_eq!(again.watermarks["stable"], Watermark { epoch: 1, sequence: 42 });
}

#[test]
fn a_changed_known_key_wins_over_the_text_it_came_from() {
    let mut state = State::from_toml(STATE).unwrap();
    state.pending = Some(Version::new(1, 5, 0));
    state.attempts = 0;
    let again = State::from_toml(&state.to_toml()).unwrap();
    assert_eq!((again.pending, again.attempts), (Some(Version::new(1, 5, 0)), 0));
    assert_eq!(again.unknown, state.unknown);
}

#[test]
fn bad_text_is_an_error_not_a_default() {
    assert!(State::from_toml("current = ").is_err());
    assert!(State::from_toml("previous = \"1.0.0\"").is_err());
}

#[test]
fn every_sequence_round_trips_exactly() {
    let edges = [0, 1, i64::MAX as u64 - 1, i64::MAX as u64, i64::MAX as u64 + 1, u64::MAX - 1, u64::MAX];
    for n in edges {
        let mut state = State::from_toml(STATE).unwrap();
        state.keys_sequence = n;
        state.watermarks.insert("stable".into(), Watermark { epoch: u32::MAX, sequence: n });
        state.watermarks.insert("beta".into(), Watermark { epoch: 0, sequence: n });
        let text = state.to_toml();
        let again = State::from_toml(&text).unwrap();
        assert_eq!(again, state, "{n}: {text}");
        assert_eq!(State::from_toml(&again.to_toml()).unwrap(), state, "{n}");
    }
}

#[test]
fn a_sequence_above_i64_is_a_string_and_a_bad_one_is_an_error() {
    let text = "current = \"1.0.0\"\nkeys_sequence = \"18446744073709551615\"\n";
    assert_eq!(State::from_toml(text).unwrap().keys_sequence, u64::MAX);
    let mut state = State::from_toml("current = \"1.0.0\"").unwrap();
    state.keys_sequence = u64::MAX;
    assert!(state.to_toml().contains("keys_sequence = \"18446744073709551615\""));
    state.keys_sequence = 7;
    assert!(state.to_toml().contains("keys_sequence = 7"));
    assert!(State::from_toml("current = \"1.0.0\"\nkeys_sequence = \"18446744073709551616\"").is_err());
    assert!(State::from_toml("current = \"1.0.0\"\nkeys_sequence = \"-1\"").is_err());
    assert!(State::from_toml("current = \"1.0.0\"\n[watermarks.stable]\nepoch = 1\nsequence = \"x\"").is_err());
}

#[test]
fn clearing_the_watermarks_drops_their_unknown_keys_and_reloads() {
    let mut state = State::from_toml(STATE).unwrap();
    state.watermarks.clear();
    let again = State::from_toml(&state.to_toml()).unwrap();
    assert!(again.watermarks.is_empty());
    assert!(!again.unknown.contains_key("watermarks"));
    assert_eq!(again.unknown["future_top_level"].as_str(), Some("kept"));
    assert_eq!(again.unknown["future_table"]["a"].as_integer(), Some(1));
}

#[test]
fn removing_one_feed_keeps_the_unknown_keys_of_another() {
    let mut state =
        State::from_toml(&format!("{STATE}\n[watermarks.beta]\nepoch = 2\nsequence = 3\nfuture_beta = 1\n")).unwrap();
    state.watermarks.remove("stable");
    let again = State::from_toml(&state.to_toml()).unwrap();
    assert_eq!(again.watermarks.keys().collect::<Vec<_>>(), ["beta"]);
    assert_eq!(again.unknown["watermarks"]["beta"]["future_beta"].as_integer(), Some(1));
    assert!(again.unknown["watermarks"].get("stable").is_none());
}

#[test]
fn watermarks_order_by_epoch_first() {
    assert!(Watermark { epoch: 1, sequence: u64::MAX } < Watermark { epoch: 2, sequence: 1 });
    assert!(Watermark { epoch: 2, sequence: 1 } < Watermark { epoch: 2, sequence: 2 });
}

fn release() -> Release {
    Release {
        version: Version::new(1, 4, 0),
        channel: Channel::Stable,
        sequence: 9,
        package: PackageKind::Delta { from: Version::new(1, 3, 2) },
        download_bytes: 1000,
        installed_bytes: 5000,
        notes_url: None,
        recommended: true,
    }
}

fn round_trips<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) {
    let json = serde_json::to_string(value).unwrap();
    assert_eq!(&serde_json::from_str::<T>(&json).unwrap(), value, "{json}");
}

#[test]
fn the_op_types_round_trip_through_json() {
    round_trips(&release());
    round_trips(&UpdateStatus::Available { release: release() });
    round_trips(&UpdateStatus::Staged { version: Version::new(1, 4, 0), deferred_by: vec![HoldReason::Recording] });
    round_trips(&UpdateStatus::CheckFailed { at: Timestamp(5), reason: CheckFailure::Replay });
    round_trips(&UpdateError::Replay {
        have: Watermark { epoch: 1, sequence: 2 },
        got: Watermark { epoch: 1, sequence: 1 },
    });
    round_trips(&UpdateEvent::Status(UpdateStatus::Idle { last_checked: None }));
    round_trips(&UpdateError::Http(503));
    round_trips(&UpdateError::Busy(vec![HoldReason::Recording, HoldReason::Streaming]));
}

#[test]
fn the_json_schemas_match_the_snapshot() {
    // One line per type keeps the generated file small; the diff still names the type that changed.
    let schemas = [
        ("UpdateStatus", schema_for!(UpdateStatus)),
        ("Release", schema_for!(Release)),
        ("UpdateError", schema_for!(UpdateError)),
        ("State", schema_for!(State)),
    ];
    let mut text = String::new();
    for (name, schema) in &schemas {
        writeln!(text, "{name} {}", serde_json::to_string(schema).unwrap()).unwrap();
    }
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/schemas.json");
    if std::env::var_os("UPDATE_SCHEMAS").is_some() {
        std::fs::write(&path, &text).unwrap();
        return;
    }
    let before = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(before == text, "a schema changed: review it, then regenerate with UPDATE_SCHEMAS=1");
}
