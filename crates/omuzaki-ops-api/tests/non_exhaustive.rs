//! The contract types features extend are `#[non_exhaustive]` with constructors (ADR 0054 §4). An integration test is
//! a separate crate, so it sees them as every other crate does: it builds them only through constructors, matches them
//! only with wildcard arms, and handles events it does not know through their serialised form. If a field or variant
//! is added to one of them, nothing here needs editing: the fixtures below play a newer producer and a newer consumer.
//! The `compile_fail` doctests on `Event` and `Outcome` show that the literal forms are refused.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocVersion, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, ErrorCode, Event, OpCtx, OpRecord, Outcome, Plan, TxId};
use serde_json::{Value, json};

fn record() -> OpRecord {
    OpRecord::new(1, "x.y", json!({}), Actor::System, TxId(1), Seconds(0.0), Patch::new(), DocVersion(1))
}

#[test]
fn every_contract_type_is_built_through_constructors() {
    let doc = MemDoc::from_value(json!({}));
    let clock = FakeClock::new(Seconds(0.0));
    let ctx = OpCtx::new(&doc, &Actor::System, &clock).with_dry_run(true);
    assert!(ctx.dry_run);

    let record = record().with_cause(None);
    let outcome = Outcome::new(Plan::default()).with_applied(record.clone());
    assert_eq!(outcome.applied, Some(record.clone()));
    assert_eq!(Event::op_applied(record.clone()), Event::op_applied(record));
}

/// A consumer that handles a subset of events: the wildcard arm is what keeps a new variant from editing it.
fn describe(event: &Event) -> &'static str {
    match event {
        Event::OpApplied { .. } => "applied",
        Event::TxCommitted { .. } => "committed",
        _ => "other",
    }
}

#[test]
fn a_consumer_of_a_subset_ignores_what_it_does_not_know() {
    assert_eq!(describe(&Event::op_applied(record())), "applied");
    assert_eq!(describe(&Event::job_progress(omuzaki_ops_api::JobId(1), "x", 0.5)), "other");
}

/// A consumer of error codes maps the ones it knows and has a default for the rest.
fn retryable(code: ErrorCode) -> bool {
    matches!(code, ErrorCode::Conflict | ErrorCode::Limited)
}

#[test]
fn error_codes_are_matched_with_a_default() {
    assert!(retryable(ErrorCode::Conflict) && !retryable(ErrorCode::Invalid));
}

/// Forwarding goes through the serialised form: a newer producer's event with a field this version has never heard of
/// still reads, and the event tag and the known fields survive a round trip with no code naming the new field.
#[test]
fn a_newer_producer_adds_a_field_and_the_consumer_is_unchanged() {
    let mut json: Value = serde_json::to_value(Event::tx_committed(TxId(3), "label", Actor::System)).unwrap();
    json["brand_new_field"] = json!({"anything": [1, 2, 3]});
    let event: Event = serde_json::from_value(json).unwrap();
    assert_eq!(event, Event::tx_committed(TxId(3), "label", Actor::System));
    assert_eq!(describe(&event), "committed");

    // The same for a record inside an event.
    let mut record = serde_json::to_value(record()).unwrap();
    record["brand_new_field"] = json!(true);
    assert_eq!(serde_json::from_value::<OpRecord>(record).unwrap(), self::record());
}

/// A generic forwarder reads the tag and the payload from the serialised event, whatever the variant.
#[test]
fn events_forward_generically_through_their_serialised_form() {
    let events = [
        Event::op_applied(record()),
        Event::tx_committed(TxId(1), "l", Actor::System),
        Event::job_progress(omuzaki_ops_api::JobId(2), "stage", 0.25),
    ];
    let tags: Vec<String> =
        events.iter().map(|e| serde_json::to_value(e).unwrap()["event"].as_str().unwrap().to_owned()).collect();
    assert_eq!(tags, ["op_applied", "tx_committed", "job_progress"]);
}
