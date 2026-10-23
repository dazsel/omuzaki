//! The audit trail in the journal (#1059, `agent-sessions.md` §9.1): every session change and every approval asked or
//! answered goes to the sink as `HistoryEntry::Audit`, in the same lock hold as the change, so journal order is the
//! order of effect. No secret is ever in a fact.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_history_api::{AUDIT_TEXT_MAX, AuditDecision, AuditFact, CUT_MARKER, HistoryEntry};
use omuzaki_history_mock::RecordingSink;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{Decision, EndReason, Grants, PauseReason, Runner, Scope, SessionInfo, Sessions};
use serde_json::json;

use super::asks::{queued, wide};
use super::{open_agents, person, request, session_spec};

fn journalled() -> (Executor, Arc<RecordingSink>, Arc<FakeClock>) {
    let sink = Arc::new(RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec =
        Executor::new(Box::new(MemDoc::from_value(json!({}))), clock.clone()).unwrap().with_journal(sink.clone());
    open_agents(&exec);
    (exec, sink, clock)
}

fn narrow(exec: &Executor) -> SessionInfo {
    exec.open(omuzaki_ops_api::SessionSpec { grants: Grants::agent_default(), ..session_spec("helper") }).unwrap()
}

fn person_set(exec: &Executor, at: &str, value: i64) {
    exec.run(request("test.set", json!({"path": at, "value": value}))).unwrap();
}

/// The audit facts the sink holds after `from` entries, with their times.
fn facts(sink: &RecordingSink, from: usize) -> Vec<(Seconds, AuditFact)> {
    sink.entries()
        .into_iter()
        .skip(from)
        .filter_map(|entry| match entry {
            HistoryEntry::Audit { at, fact } => Some((at, AuditFact::clone(&fact))),
            _ => None,
        })
        .collect()
}

#[test]
fn a_scripted_session_leaves_its_facts_in_order() {
    let (exec, sink, clock) = journalled();
    let start = sink.entries().len();
    clock.set(Seconds(1.0));
    let info = narrow(&exec);
    clock.set(Seconds(2.0));
    exec.set_grants(info.id, Grants::agent_default().without(Scope::Render), &person()).unwrap();
    clock.set(Seconds(3.0));
    exec.pause(info.id, &person()).unwrap();
    exec.resume(info.id, &person()).unwrap();
    // A publish needs a scope the session lacks: queued, then rejected with a comment.
    let (approval, _) =
        queued(&exec.run(wide(&info.actor, "test.publish", json!({"path": "p", "value": 1}))).unwrap_err());
    exec.resolve(approval, Decision::Reject { comment: Some("not this one".into()) }, &person()).unwrap();
    clock.set(Seconds(4.0));
    exec.end(info.id, EndReason::Revoked { by: person(), credential: true }).unwrap();

    let got = facts(&sink, start);
    let kinds: Vec<&str> = got
        .iter()
        .map(|(_, fact)| match fact {
            AuditFact::SessionOpened { .. } => "opened",
            AuditFact::GrantsChanged { .. } => "grants",
            AuditFact::Paused { .. } => "paused",
            AuditFact::Resumed { .. } => "resumed",
            AuditFact::ApprovalRequested { .. } => "requested",
            AuditFact::ApprovalResolved { .. } => "resolved",
            AuditFact::SessionEnded { .. } => "ended",
            _ => "other",
        })
        .collect();
    assert_eq!(kinds, ["opened", "grants", "paused", "resumed", "requested", "resolved", "ended"]);

    let AuditFact::SessionOpened { session, actor, on_behalf_of, client, grants, .. } = &got[0].1 else {
        unreachable!()
    };
    assert_eq!((*session, actor, on_behalf_of, client.as_str()), (info.id, &info.actor, &person(), "helper"));
    assert_eq!(*grants, info.grants);
    assert_eq!(got[0].0, Seconds(1.0), "stamped by the runner's clock");
    let AuditFact::GrantsChanged { by, grants, .. } = &got[1].1 else { unreachable!() };
    assert_eq!((by, grants.allows(Scope::Render)), (&person(), false));
    let AuditFact::Paused { by, reason, .. } = &got[2].1 else { unreachable!() };
    assert_eq!((by, *reason), (&person(), PauseReason::Person));
    let AuditFact::ApprovalRequested { approval: asked, op, .. } = &got[4].1 else { unreachable!() };
    assert_eq!((*asked, op.as_deref()), (approval, Some("test.publish")));
    let AuditFact::ApprovalResolved { by, decision, comment, .. } = &got[5].1 else { unreachable!() };
    assert_eq!(
        (by.as_ref(), *decision, comment.as_deref()),
        (Some(&person()), AuditDecision::Rejected, Some("not this one"))
    );
    let AuditFact::SessionEnded { reason, .. } = &got[6].1 else { unreachable!() };
    assert_eq!(*reason, EndReason::Revoked { by: person(), credential: true });
    assert_eq!(got[6].0, Seconds(4.0));
}

#[test]
fn ending_a_session_withdraws_its_approvals_before_the_end() {
    let (exec, sink, _) = journalled();
    let info = narrow(&exec);
    for value in [1, 2] {
        queued(&exec.run(wide(&info.actor, "test.publish", json!({"path": "p", "value": value}))).unwrap_err());
    }
    let start = sink.entries().len();
    exec.end(info.id, EndReason::Closed).unwrap();
    let got: Vec<AuditFact> = facts(&sink, start).into_iter().map(|(_, fact)| fact).collect();
    assert_eq!(got.len(), 3, "{got:#?}");
    for fact in &got[..2] {
        assert!(
            matches!(fact, AuditFact::ApprovalResolved { by: None, decision: AuditDecision::Withdrawn, .. }),
            "{fact:?}"
        );
    }
    assert!(matches!(got[2], AuditFact::SessionEnded { reason: EndReason::Closed, .. }));
}

#[test]
fn audit_facts_and_ops_are_journalled_in_the_order_they_took_effect() {
    let (exec, sink, _) = journalled();
    let info = narrow(&exec);
    let start = sink.entries().len();
    exec.run(wide(&info.actor, "test.set", json!({"path": "a", "value": 1}))).unwrap();
    exec.pause(info.id, &person()).unwrap();
    person_set(&exec, "b", 2);
    exec.end(info.id, EndReason::Revoked { by: person(), credential: false }).unwrap();
    person_set(&exec, "c", 3);
    let order: Vec<&str> = sink.entries()[start..]
        .iter()
        .filter_map(|entry| match entry {
            HistoryEntry::Op { .. } => Some("op"),
            HistoryEntry::Audit { fact, .. } => match **fact {
                AuditFact::Paused { .. } => Some("paused"),
                AuditFact::SessionEnded { .. } => Some("ended"),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(order, ["op", "paused", "op", "ended", "op"]);
}

#[test]
fn a_long_comment_is_cut_and_no_credential_reaches_a_fact() {
    let (exec, sink, _) = journalled();
    // A secret an agent put in a request's arguments: the request is recorded by its op's name, never its arguments.
    let token = "omz_tok_5f2c9a0d1e7b4c3a";
    let info = narrow(&exec);
    let publish = wide(&info.actor, "test.publish", json!({"path": "p", "value": token}));
    let (approval, _) = queued(&exec.run(publish).unwrap_err());
    let long = "x".repeat(AUDIT_TEXT_MAX * 3);
    exec.resolve(approval, Decision::Reject { comment: Some(long) }, &person()).unwrap();
    exec.end(info.id, EndReason::Revoked { by: person(), credential: true }).unwrap();

    let got = facts(&sink, 0);
    let comment = got
        .iter()
        .find_map(|(_, fact)| match fact {
            AuditFact::ApprovalResolved { comment: Some(comment), .. } => Some(comment.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(comment.chars().count(), AUDIT_TEXT_MAX + CUT_MARKER.chars().count());
    assert!(comment.ends_with(CUT_MARKER));
    for (_, fact) in &got {
        let text = serde_json::to_string(fact).unwrap();
        assert!(!text.contains(token), "{text}");
        assert!(text.len() <= 2048, "an audit payload is at most 2 KiB: {}", text.len());
    }
}
