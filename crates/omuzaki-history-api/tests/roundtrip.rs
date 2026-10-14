//! Serde round-trips for every journal entry and for the history export (the journal writes them as JSON).

use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{Change, DocPath, DocVersion, Patch, Value};
use omuzaki_history_api::{
    ActorHistory, HISTORY_EXPORT_FORMAT, HistoryEntry, HistoryExport, Lsn, OpenTxExport, Stack, StepExport, WallTime,
};
use omuzaki_ops_api::{Actor, ApprovalId, ApprovalRef, Cause, CauseKind, OpRecord, Scope, TxId};
use proptest::prelude::*;

fn seconds() -> impl Strategy<Value = Seconds> {
    // Thousandths are exact in the shortest round-trip form the journal writes.
    (0u32..10_000_000).prop_map(|ms| Seconds(f64::from(ms) / 1000.0))
}

fn tx() -> impl Strategy<Value = TxId> {
    any::<u64>().prop_map(TxId)
}

fn actor() -> impl Strategy<Value = Actor> {
    prop_oneof![
        "[a-z ]{0,8}".prop_map(|name| Actor::Person { name }),
        ("[a-z0-9-]{1,8}", proptest::option::of("[a-z0-9-]{1,8}")).prop_map(|(id, model)| Actor::Agent { id, model }),
        "[a-z]{1,8}".prop_map(|id| Actor::Plugin { id }),
        Just(Actor::System),
    ]
}

/// Documents are null-free (a `omuzaki-doc` invariant), and JSON reads `Some(Null)` back as `None`, so a
/// patch never holds a null and the generators never make one.
fn value() -> impl Strategy<Value = Value> {
    prop_oneof![
        any::<bool>().prop_map(Value::from),
        any::<i64>().prop_map(Value::from),
        "[ -~]{0,12}".prop_map(Value::from),
        proptest::collection::vec(any::<i32>(), 0..4).prop_map(Value::from),
    ]
}

fn patch() -> impl Strategy<Value = Patch> {
    let change = ("[a-z]{1,6}(/[a-z0-9]{1,6}){0,3}", proptest::option::of(value()), proptest::option::of(value()))
        .prop_map(|(path, before, after)| Change { path: DocPath::parse(&path).unwrap_or_default(), before, after });
    proptest::collection::vec(change, 0..4).prop_map(|changes| Patch { changes })
}

fn scopes() -> impl Strategy<Value = std::collections::BTreeSet<Scope>> {
    proptest::collection::btree_set(proptest::sample::select(Scope::ALL.to_vec()), 0..4)
}

fn record() -> impl Strategy<Value = OpRecord> {
    let approval = (any::<u64>(), actor()).prop_map(|(id, by)| ApprovalRef { id: ApprovalId(id), by });
    (
        any::<u64>(),
        "[a-z.]{1,10}",
        value(),
        actor(),
        tx(),
        seconds(),
        patch(),
        any::<u64>(),
        proptest::option::of(actor()),
        proptest::option::of(approval),
    )
        .prop_map(|(seq, op, args, actor, tx, at, patch, version, on_behalf_of, approval)| OpRecord {
            seq,
            op,
            args,
            actor,
            cause: Some(Cause { kind: CauseKind::Prompt, text: Some("make it rounder".into()) }),
            tx,
            at,
            patch,
            version: DocVersion(version),
            on_behalf_of,
            approval,
            effect: None,
        })
}

fn step() -> impl Strategy<Value = StepExport> {
    (tx(), "[a-z ]{0,10}", patch(), scopes())
        .prop_map(|(tx, label, patch, scopes)| StepExport::new(tx, label, Arc::new(patch), scopes))
}

fn export() -> impl Strategy<Value = HistoryExport> {
    let actor_history = (
        actor(),
        proptest::collection::vec(step(), 0..3),
        proptest::collection::vec(step(), 0..3),
        any::<u64>().prop_map(WallTime),
    )
        .prop_map(|(actor, undo, redo, last_seen)| {
            let mut history = ActorHistory::new(actor, last_seen);
            history.undo = undo;
            history.redo = redo;
            history
        });
    let open = (actor(), tx(), "[a-z ]{0,10}", patch(), scopes())
        .prop_map(|(owner, tx, label, patch, scopes)| OpenTxExport::new(owner, tx, label, patch, scopes));
    (proptest::collection::vec(actor_history, 0..3), proptest::collection::vec(open, 0..3), tx(), any::<u64>())
        .prop_map(|(actors, open, next_tx, next_seq)| {
            let mut export = HistoryExport::empty();
            (export.actors, export.open, export.next_tx, export.next_seq) = (actors, open, next_tx, next_seq);
            export
        })
}

fn stack() -> impl Strategy<Value = Stack> {
    prop_oneof![Just(Stack::Undo), Just(Stack::Redo)]
}

fn entry() -> impl Strategy<Value = HistoryEntry> {
    prop_oneof![
        (tx(), actor(), "[a-z ]{0,10}", seconds()).prop_map(|(tx, actor, label, at)| HistoryEntry::Begin {
            tx,
            actor,
            label,
            at
        }),
        (record(), any::<bool>()).prop_map(|(r, commits)| HistoryEntry::Op { record: Arc::new(r), commits }),
        ("[a-f0-9]{8}", any::<u64>())
            .prop_map(|(session, ms)| HistoryEntry::Session { session, started: WallTime(ms) }),
        tx().prop_map(|tx| HistoryEntry::Commit { tx }),
        (tx(), patch()).prop_map(|(tx, patch)| HistoryEntry::Rollback { tx, patch }),
        (actor(), tx(), patch()).prop_map(|(actor, tx, patch)| HistoryEntry::Undo { actor, tx, patch }),
        (actor(), tx(), patch()).prop_map(|(actor, tx, patch)| HistoryEntry::Redo { actor, tx, patch }),
        (actor(), tx(), stack()).prop_map(|(actor, tx, stack)| HistoryEntry::Dropped { actor, tx, stack }),
        export().prop_map(|h| HistoryEntry::Mark { history: Arc::new(h) }),
    ]
}

proptest! {
    #[test]
    fn every_entry_round_trips_through_json(entry in entry()) {
        let text = serde_json::to_string(&entry).unwrap();
        let back: HistoryEntry = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(back, entry);
    }

    #[test]
    fn the_history_export_round_trips_through_json(export in export()) {
        let text = serde_json::to_string(&export).unwrap();
        let back: HistoryExport = serde_json::from_str(&text).unwrap();
        prop_assert_eq!(back, export);
    }

    #[test]
    fn lsn_is_a_bare_number(n in any::<u64>()) {
        prop_assert_eq!(serde_json::to_string(&Lsn(n)).unwrap(), n.to_string());
    }
}

#[test]
fn an_empty_export_names_its_format_and_starts_ids_at_one() {
    let json = serde_json::to_value(HistoryExport::empty()).unwrap();
    assert_eq!(json["format"], HISTORY_EXPORT_FORMAT);
    assert_eq!(json["next_tx"], 1);
    assert_eq!(json["next_seq"], 1);
}

#[test]
fn entries_are_tagged_by_kind() {
    let json = serde_json::to_value(HistoryEntry::Commit { tx: TxId(7) }).unwrap();
    assert_eq!(json, serde_json::json!({"entry": "commit", "tx": 7}));
}

#[test]
fn a_non_finite_time_does_not_round_trip_so_writers_must_refuse_it() {
    let entry = HistoryEntry::Begin { tx: TxId(1), actor: Actor::System, label: String::new(), at: Seconds(f64::NAN) };
    let text = serde_json::to_string(&entry).unwrap();
    assert!(text.contains("\"at\":null"), "{text}");
    assert!(serde_json::from_str::<HistoryEntry>(&text).is_err());
}
