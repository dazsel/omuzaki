//! `State` keeps what it does not know, `Watermark` orders by epoch first, and the serde shapes and JSON Schemas of the
//! types that cross an op are pinned (`UPDATE_SCHEMAS=1` regenerates `tests/schemas.json`).

// Helpers outside `#[test]` functions may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;

use omuzaki_update_api::{
    Channel, CheckFailure, HoldReason, PackageKind, Release, RollbackReason, State, Timestamp, UpdateError,
    UpdateEvent, UpdateStatus, Version, Watermark,
};
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
fn a_huge_sequence_is_written_saturated() {
    let mut state = State::from_toml("current = \"1.0.0\"").unwrap();
    state.watermarks.insert("stable".into(), Watermark { epoch: 0, sequence: u64::MAX });
    let again = State::from_toml(&state.to_toml()).unwrap();
    assert_eq!(again.watermarks["stable"].sequence, i64::MAX as u64);
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
