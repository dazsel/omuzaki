//! Serde round-trips for every journal entry and for the history export (the journal writes them as JSON).

use std::sync::Arc;

use omuzaki_core::time::Seconds;
use omuzaki_doc_api::{Change, DocPath, DocVersion, Patch, Value};
use omuzaki_history_api::{
    ActorHistory, HISTORY_EXPORT_FORMAT, HistoryEntry, HistoryExport, Lsn, OpenTxExport, Stack, StepExport, WallTime,
};
use omuzaki_ops_api::{
    Actor, ApprovalId, ApprovalRef, CancelReason, Cause, CauseKind, Effect, JobId, JobState, OpCall, OpError, OpRecord,
    Scope, TxId,
};
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
    let approval = (any::<u64>(), actor()).prop_map(|(id, by)| ApprovalRef::new(ApprovalId(id), by));
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
        .prop_map(|(seq, op, args, actor, tx, at, patch, version, on_behalf_of, approval)| {
            let mut record = OpRecord::new(seq, op, args, actor, tx, at, patch, DocVersion(version))
                .with_cause(Cause { kind: CauseKind::Prompt, text: Some("make it rounder".into()) });
            if let Some(person) = on_behalf_of {
                record = record.with_on_behalf_of(person);
            }
            if let Some(approval) = approval {
                record = record.with_approval(approval);
            }
            record
        })
}

fn effect() -> impl Strategy<Value = Effect> {
    ("[a-z ]{0,10}", "[a-z.]{1,10}", value(), "[a-z.]{1,10}", value()).prop_map(
        |(summary, undo_op, undo_args, redo_op, redo_args)| Effect {
            summary,
            undo: OpCall { op: undo_op, args: undo_args },
            redo: OpCall { op: redo_op, args: redo_args },
        },
    )
}

fn effects() -> impl Strategy<Value = Vec<Effect>> {
    proptest::collection::vec(effect(), 0..4)
}

fn job_state() -> impl Strategy<Value = JobState> {
    prop_oneof![
        Just(JobState::Queued),
        Just(JobState::Running),
        Just(JobState::Held),
        (tx(), proptest::option::of(any::<u64>())).prop_map(|(tx, seq)| JobState::done(tx, seq)),
        "[a-z ]{0,10}".prop_map(|message| JobState::Failed { error: OpError::unsupported(message) }),
        (
            proptest::option::of(actor()),
            proptest::sample::select(vec![
                CancelReason::Requested,
                CancelReason::Stop,
                CancelReason::SessionEnded,
                CancelReason::Timeout,
                CancelReason::Budget,
                CancelReason::Shutdown,
            ]),
        )
            .prop_map(|(by, reason)| JobState::Cancelled { by, reason }),
        Just(JobState::Interrupted),
    ]
}

fn step() -> impl Strategy<Value = StepExport> {
    (tx(), "[a-z ]{0,10}", patch(), scopes(), effects()).prop_map(|(tx, label, patch, scopes, effects)| {
        StepExport::new(tx, label, Arc::new(patch), scopes).with_effects(effects)
    })
}

fn open_tx() -> impl Strategy<Value = OpenTxExport> {
    (actor(), tx(), "[a-z ]{0,10}", patch(), scopes(), effects()).prop_map(
        |(owner, tx, label, patch, scopes, effects)| {
            OpenTxExport::new(owner, tx, label, patch, scopes).with_effects(effects)
        },
    )
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
    (proptest::collection::vec(actor_history, 0..3), proptest::collection::vec(open_tx(), 0..3), tx(), any::<u64>())
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
        (any::<u64>(), "[a-z.]{1,10}", value(), actor(), proptest::option::of(actor()), seconds()).prop_map(
            |(job, op, args, actor, on_behalf_of, at)| HistoryEntry::JobStarted {
                job: JobId(job),
                op,
                args,
                actor,
                on_behalf_of,
                at,
            }
        ),
        (any::<u64>(), job_state()).prop_map(|(job, state)| HistoryEntry::JobEnded { job: JobId(job), state }),
        (tx(), proptest::collection::vec(any::<u64>(), 0..4), patch(), open_tx())
            .prop_map(|(tx, seqs, patch, remaining)| HistoryEntry::TxRevert { tx, seqs, patch, remaining }),
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

fn effect_named(summary: &str) -> Effect {
    Effect {
        summary: summary.into(),
        undo: OpCall { op: "fs.remove".into(), args: Value::from("a.txt") },
        redo: OpCall { op: "fs.write".into(), args: Value::from("a.txt") },
    }
}

#[test]
fn a_tx_revert_keeps_its_remaining_transaction_exactly() {
    let patch = Patch {
        changes: vec![Change { path: DocPath::parse("layers/a").unwrap(), before: None, after: Some(Value::from(3)) }],
    };
    let remaining = OpenTxExport::new(
        Actor::System,
        TxId(4),
        "open",
        patch.clone(),
        std::collections::BTreeSet::from([Scope::Read]),
    )
    .with_effects(vec![effect_named("wrote a.txt"), effect_named("made a.txt")]);
    let entry = HistoryEntry::TxRevert { tx: TxId(4), seqs: vec![7, 9], patch, remaining: remaining.clone() };
    let text = serde_json::to_string(&entry).unwrap();
    let back: HistoryEntry = serde_json::from_str(&text).unwrap();
    assert_eq!(back, entry);
    let HistoryEntry::TxRevert { remaining: back_remaining, .. } = back else { panic!("not a tx_revert") };
    assert_eq!(back_remaining, remaining);
}

#[test]
fn entries_for_jobs_and_reverts_are_tagged_by_kind() {
    let started = HistoryEntry::JobStarted {
        job: JobId(3),
        op: "render".into(),
        args: Value::from(1),
        actor: Actor::System,
        on_behalf_of: None,
        at: Seconds(1.5),
    };
    let json = serde_json::to_value(&started).unwrap();
    assert_eq!(json["entry"], "job_started");
    assert_eq!(json["job"], 3);
    assert_eq!(
        serde_json::to_value(HistoryEntry::JobEnded { job: JobId(3), state: JobState::Interrupted }).unwrap()["entry"],
        "job_ended"
    );
}

#[test]
fn an_export_with_several_effects_round_trips_in_apply_order() {
    let mut export = HistoryExport::empty();
    let step = StepExport::new(TxId(2), "make", Arc::new(Patch::default()), std::collections::BTreeSet::new())
        .with_effects(vec![effect_named("first"), effect_named("second"), effect_named("third")]);
    let mut history = ActorHistory::new(Actor::System, WallTime(1));
    history.undo.push(step);
    export.actors.push(history);
    let text = serde_json::to_string(&export).unwrap();
    let back: HistoryExport = serde_json::from_str(&text).unwrap();
    assert_eq!(back, export);
    let summaries: Vec<&str> = back.actors[0].undo[0].effects.iter().map(|e| e.summary.as_str()).collect();
    assert_eq!(summaries, ["first", "second", "third"]);
}

#[test]
fn an_export_written_before_effects_reads_with_empty_effects() {
    let text = include_str!("fixtures/history_export_format1_before_effects.json");
    let export: HistoryExport = serde_json::from_str(text).unwrap();
    assert_eq!(export.format, HISTORY_EXPORT_FORMAT);
    assert_eq!(HISTORY_EXPORT_FORMAT, 1);
    assert_eq!(export.actors[0].undo.len(), 1);
    assert!(export.actors.iter().flat_map(|a| a.undo.iter().chain(&a.redo)).all(|step| step.effects.is_empty()));
    assert!(export.open.iter().all(|open| open.effects.is_empty()));
}
