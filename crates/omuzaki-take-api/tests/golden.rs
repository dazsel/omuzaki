//! Serde round trips of the JSON records, each against a golden file: the JSON is the on-disk format (T2, T4), so a
//! field renamed by accident fails here. Goldens are compared as values (key order is not part of the format) and
//! the struct must read back equal.

#![allow(clippy::expect_used)]

use std::fmt::Debug;

use omuzaki_take_api::{EventRecord, StateRecord, TakeEntry, TakeHeader};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + Debug>(golden: &str) -> T {
    let expected: Value = serde_json::from_str(golden).expect("golden is JSON");
    let value: T = serde_json::from_str(golden).expect("golden reads into the type");
    assert_eq!(serde_json::to_value(&value).expect("serialises"), expected, "serialised form drifted from the golden");
    let again: T = serde_json::from_str(&serde_json::to_string(&value).expect("serialises")).expect("reads back");
    assert_eq!(again, value);
    value
}

#[test]
fn take_header_round_trips() {
    let header: TakeHeader = round_trip(include_str!("golden/take_header.json"));
    assert_eq!(header.fps, (30, 1));
    assert_eq!(header.audio["mic"], "2026-10-13-stream-a.mic.wav");
}

#[test]
fn state_full_round_trips() {
    let state: StateRecord = round_trip(include_str!("golden/state_full.json"));
    assert_eq!((state.cast.len(), state.omitted_more, state.overflow), (1, 3, false));
    assert_eq!(state.withheld[0].to, None);
}

#[test]
fn state_minimal_round_trips() {
    let state: StateRecord = round_trip(include_str!("golden/state_minimal.json"));
    assert!(state.overflow && state.cast.is_empty() && state.live.is_empty());
}

#[test]
fn a_state_with_only_the_required_fields_reads() {
    // Later minors may add fields and an older writer may omit optional ones: the defaults must hold.
    let state: StateRecord =
        serde_json::from_str(r#"{"doc_version":1,"project":"p","after_lsn":0,"withheld":[]}"#).expect("reads");
    assert!(state.cast.is_empty() && !state.overflow && state.delay_s == 0.0);
}

#[test]
fn event_full_round_trips_with_the_cause_text_cleared() {
    let event: EventRecord = round_trip(include_str!("golden/event_full.json"));
    assert!(matches!(event.entry, TakeEntry::Full { .. }));
}

#[test]
fn event_placeholder_round_trips() {
    let event: EventRecord = round_trip(include_str!("golden/event_placeholder.json"));
    assert_eq!(event.entry, TakeEntry::Placeholder { kind: "mark".into() });
}

#[test]
fn unknown_state_fields_are_ignored() {
    let state: StateRecord =
        serde_json::from_str(r#"{"doc_version":1,"project":"p","after_lsn":0,"withheld":[],"future":true}"#)
            .expect("a newer minor's extra field is ignored");
    assert_eq!(state.project, "p");
}
