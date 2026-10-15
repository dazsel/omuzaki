//! Serde round trips of the JSON records, each against a golden file: the JSON is the on-disk format (T2, T4), so a
//! field renamed by accident fails here. Goldens are compared as values (key order is not part of the format) and
//! the struct must read back equal.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::fmt::Debug;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::DocVersion;
use omuzaki_history_api::{HistoryEntry, Lsn, SnapshotId};
use omuzaki_stage_api::SceneId;
use omuzaki_take_api::header::{SnapshotAt, TakeHeader, TakeKind};
use omuzaki_take_api::record::{EventRecord, TakeEntry};
use omuzaki_take_api::state::{CastEntry, SourceEntry, StateRecord, VoiceEntry, Withheld, WithheldCause};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

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
    assert!(matches!(event.entry, TakeEntry::Full(HistoryEntry::Op { .. })));
}

#[test]
fn event_placeholder_round_trips() {
    let event: EventRecord = round_trip(include_str!("golden/event_placeholder.json"));
    assert_eq!(event.entry, TakeEntry::Placeholder { kind: "mark".into() });
}

#[test]
fn the_minimal_state_as_t2_writes_it_reads() {
    // The overflow fallback writes only these four fields; it must stay readable, not become a JSON error.
    let state: StateRecord =
        serde_json::from_str(r#"{"doc_version":17,"after_lsn":41,"withheld":[],"overflow":true}"#).expect("reads");
    assert!(state.overflow && state.project.is_empty() && state.cast.is_empty());
}

#[test]
fn unknown_state_fields_are_ignored() {
    let state: StateRecord =
        serde_json::from_str(r#"{"doc_version":1,"project":"p","after_lsn":0,"withheld":[],"future":true}"#)
            .expect("a newer minor's extra field is ignored");
    assert_eq!(state.project, "p");
}

#[test]
fn the_minimal_state_serialises_exactly_the_overflow_fields() {
    let built =
        StateRecord::minimal(DocVersion(17), Lsn(41), vec![Withheld::new(0, WithheldCause::Muted, Seconds(1.0))]);
    let value = serde_json::to_value(&built).expect("serialises");
    let mut keys: Vec<&str> = value.as_object().expect("an object").keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, ["after_lsn", "doc_version", "overflow", "withheld"]);
    let back: StateRecord = serde_json::from_value(value).expect("reads back");
    assert_eq!(back, built);
}

#[test]
fn the_minimal_state_builds_through_its_constructor() {
    // The overflow fallback is built by `StateRecord::minimal`, so the constructor and the golden must agree.
    let built = StateRecord::minimal(DocVersion(17), Lsn(41), Vec::new());
    let expected: Value = serde_json::from_str(include_str!("golden/state_minimal.json")).expect("golden is JSON");
    assert_eq!(serde_json::to_value(&built).expect("serialises"), expected);
}

#[test]
fn the_full_state_builds_through_its_constructors() {
    let built = StateRecord::new(DocVersion(17), Lsn(41), vec![Withheld::new(0, WithheldCause::Muted, Seconds(12.5))])
        .with_project("demo")
        .with_scene(SceneId::new("main").expect("a slug"))
        .with_camera("bust")
        .with_cast(vec![CastEntry::new("mira", "mira", "ab12")])
        .with_sources(vec![SourceEntry::new(0, "mira", "arkit", "iphone")])
        .with_voices(vec![VoiceEntry::new(0, "mira", "mic")])
        .with_models(BTreeMap::from([
            ("tracker".to_owned(), "tracker-v2".to_owned()),
            ("lipsync".to_owned(), "lipsync-v1".to_owned()),
        ]))
        .with_delay_s(0.25)
        .with_omitted(vec!["obs".to_owned()], 3)
        .with_live(BTreeMap::from([("expressions".to_owned(), json!({ "held": ["smile"] }))]));
    let expected: Value = serde_json::from_str(include_str!("golden/state_full.json")).expect("golden is JSON");
    assert_eq!(serde_json::to_value(&built).expect("serialises"), expected);
}

#[test]
fn the_placeholder_event_builds_through_its_constructor() {
    let built = EventRecord::new(Lsn(43), 91, TakeEntry::Placeholder { kind: "mark".into() });
    let expected: Value = serde_json::from_str(include_str!("golden/event_placeholder.json")).expect("golden is JSON");
    assert_eq!(serde_json::to_value(&built).expect("serialises"), expected);
}

#[test]
fn the_take_header_builds_through_its_constructor() {
    let snapshot: SnapshotId =
        "0000000000000000000000000000000000000000000000000000000000000001".parse().expect("a hash");
    let built = TakeHeader::new(
        "2026-10-13-stream-a",
        TakeKind::Recording,
        (30, 1),
        0,
        "2026-10-13T18:00:00Z",
        "0.0.0",
        Lsn(41),
        SnapshotAt::Start,
        DocVersion(17),
    )
    .with_snapshot(snapshot)
    .with_video("2026-10-13-stream-a.mp4")
    .with_audio(BTreeMap::from([("mic".to_owned(), "2026-10-13-stream-a.mic.wav".to_owned())]));
    let expected: Value = serde_json::from_str(include_str!("golden/take_header.json")).expect("golden is JSON");
    assert_eq!(serde_json::to_value(&built).expect("serialises"), expected);
}

#[test]
fn an_unknown_entry_form_reads_as_unknown() {
    // A newer minor's `form` is skipped, not a read error: the LSN still counts.
    let event: EventRecord = serde_json::from_str(include_str!("golden/event_unknown_form.json")).expect("reads");
    assert_eq!((event.lsn, event.entry), (Lsn(44), TakeEntry::Unknown));
}

#[test]
fn an_unknown_withheld_cause_reads_as_unknown() {
    let withheld: Withheld = serde_json::from_str(include_str!("golden/withheld_unknown_cause.json")).expect("reads");
    assert_eq!(withheld.cause, WithheldCause::Unknown);
    assert_eq!(withheld.to, None);
}
