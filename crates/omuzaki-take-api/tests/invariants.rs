//! The take's rules that hold by construction (#1855, #1597): a full State always names its project, `Mark` and
//! `Session` are only ever placeholders, a full entry never carries cause text, and a reader tells live state it
//! cannot read apart from providers the writer left out.

#![allow(clippy::expect_used)]

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_doc_api::DocVersion;
use omuzaki_history_api::{HistoryEntry, HistoryExport, Lsn, WallTime};
use omuzaki_take_api::record::{EventRecord, TakeEntry};
use omuzaki_take_api::source::TakeReport;
use omuzaki_take_api::state::{ProjectName, StateRecord};
use serde_json::{Value, json};

/// The golden op entry, with a chat message as its cause text.
fn op_with_cause_text(text: &str) -> HistoryEntry {
    let mut value: Value = serde_json::from_str(include_str!("golden/event_full.json")).expect("golden is JSON");
    let entry = value["entry"].as_object_mut().expect("an entry");
    entry.remove("form");
    entry["record"]["cause"]["text"] = json!(text);
    serde_json::from_value(Value::Object(entry.clone())).expect("an op entry")
}

#[test]
fn a_full_entry_never_serialises_cause_text() {
    let secret = "say hi to everyone in chat";
    let entry = TakeEntry::from_history(op_with_cause_text(secret));
    let json = serde_json::to_string(&EventRecord::new(Lsn(42), 90, entry)).expect("serialises");
    assert!(!json.contains(secret), "{json}");
    // What remains is exactly the golden: the cause kind is kept, its text is null.
    let golden: Value = serde_json::from_str(include_str!("golden/event_full.json")).expect("golden is JSON");
    assert_eq!(serde_json::from_str::<Value>(&json).expect("JSON"), golden);
}

#[test]
fn reading_an_entry_with_cause_text_drops_it() {
    // An older or careless writer kept the text: the reader never hands it on.
    let mut value: Value = serde_json::from_str(include_str!("golden/event_full.json")).expect("golden is JSON");
    value["entry"]["record"]["cause"]["text"] = json!("a private prompt");
    let event: EventRecord = serde_json::from_value(value).expect("reads");
    let json = serde_json::to_string(&event).expect("serialises");
    assert!(!json.contains("a private prompt"), "{json}");
}

#[test]
fn mark_and_session_become_placeholders() {
    let mark = HistoryEntry::Mark { history: Arc::new(HistoryExport::empty()) };
    let session = HistoryEntry::Session { session: "s1".into(), started: WallTime(1_700_000_000_000) };
    assert_eq!(TakeEntry::from_history(mark), TakeEntry::Placeholder { kind: "mark".into() });
    assert_eq!(TakeEntry::from_history(session), TakeEntry::Placeholder { kind: "session".into() });
}

#[test]
fn mark_or_session_in_full_is_refused_when_read() {
    for entry in [
        json!({ "form": "full", "entry": "session", "session": "s1", "started": 1 }),
        json!({ "form": "full", "entry": "mark", "history": serde_json::to_value(HistoryExport::empty()).expect("JSON") }),
    ] {
        let err = serde_json::from_value::<TakeEntry>(entry).expect_err("only ever a placeholder");
        assert!(err.to_string().contains("placeholder"), "{err}");
    }
}

#[test]
fn every_other_entry_round_trips_in_full() {
    let entry = TakeEntry::from_history(op_with_cause_text("x"));
    let TakeEntry::Full(full) = &entry else { panic!("an op is carried in full: {entry:?}") };
    assert!(matches!(full.entry(), HistoryEntry::Op { .. }));
    let back: TakeEntry = serde_json::from_str(&serde_json::to_string(&entry).expect("serialises")).expect("reads");
    assert_eq!(back, entry);
}

#[test]
fn a_full_state_cannot_lose_its_project() {
    // `project` and `overflow` are private: the only States are the constructors', and each one reads back.
    let full = StateRecord::new(DocVersion(1), ProjectName::new("demo").expect("a name"), Lsn(0), Vec::new());
    let minimal = StateRecord::minimal(DocVersion(1), Lsn(0), Vec::new());
    assert_eq!((full.project().map(ProjectName::as_str), full.overflow()), (Some("demo"), false));
    assert_eq!((minimal.project(), minimal.overflow()), (None, true));
    for state in [full, minimal] {
        let back: StateRecord =
            serde_json::from_str(&serde_json::to_string(&state).expect("serialises")).expect("reads");
        assert_eq!(back, state);
    }
}

#[test]
fn unknown_live_providers_are_reported_apart_from_omitted_ones() {
    let state = StateRecord::new(DocVersion(1), ProjectName::new("demo").expect("a name"), Lsn(0), Vec::new())
        .with_omitted(vec!["obs".to_owned()], 0)
        .with_live(BTreeMap::from([
            ("expressions".to_owned(), json!({ "held": [] })),
            ("from_a_newer_build".to_owned(), json!(1)),
        ]));
    let mut report = TakeReport::new();
    for _ in 0..2 {
        report.note_state(&state, |name| name == "expressions");
    }
    report.note_state(&StateRecord::minimal(DocVersion(1), Lsn(0), Vec::new()), |_| true);
    assert_eq!(report.unknown_live, BTreeMap::from([("from_a_newer_build".to_owned(), 2)]));
    assert_eq!(report.omitted, BTreeMap::from([("obs".to_owned(), 2)]), "the writer's own cut, and only that");
    assert_eq!(report.state_overflow, 1);
}
