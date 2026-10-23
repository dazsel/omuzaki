//! The approval queue (#316, design `agent-sessions.md` §7): a request a person could allow is queued, not refused;
//! it is resolved exactly once; ending the session withdraws it; nothing resolves on a timer; with no approver the
//! same requests are refused with a hint.

use std::sync::Arc;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    Actor, ApprovalRef, Budget, Decision, EndReason, ErrorCode, ErrorDetail, Event, Grants, Limits, Mode, Need,
    Request, Resolved, Runner, Scope, SessionSpec, SessionState, Sessions,
};
use serde_json::json;

use super::asks::{queued, wide};
use super::{drain, person, rig, session_spec};

fn bob() -> Actor {
    Actor::Person { name: "bob".into() }
}

fn publish(actor: &Actor, value: i64) -> Request {
    wide(actor, "test.publish", json!({"path": "p", "value": value}))
}

#[test]
fn a_missing_scope_queues_once_and_resolves_exactly_once() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let rx = rig.subscribe();

    let waiting = rig.exec.run(publish(&agent, 1)).unwrap_err();
    let (id, need) = queued(&waiting);
    assert_eq!(need, Need::Scope { scope: Scope::Publish });
    assert_eq!(rig.at("p"), None, "nothing ran");
    let requested: Vec<_> = drain(&rx).into_iter().filter(|e| matches!(e, Event::ApprovalRequested { .. })).collect();
    assert_eq!(requested.len(), 1, "one event");
    let pending = rig.exec.approvals();
    assert_eq!(pending.len(), 1);
    let card = pending[0].request.as_ref().unwrap();
    assert_eq!((card.op.as_str(), card.preview.patch.changes.len()), ("test.publish", 1), "the dry-run is the preview");
    assert_eq!(rig.exec.session_of(&agent).unwrap().pending, vec![id]);

    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    let Resolved::Applied { outcome } = resolution.result else { panic!("{:?}", resolution.result) };
    let record = outcome.applied.unwrap();
    assert_eq!(record.approval, Some(ApprovalRef::new(id, person())), "the record names the approver");
    assert_eq!((record.actor.clone(), record.on_behalf_of.clone()), (agent.clone(), Some(person())));
    assert_eq!(rig.at("p"), Some(json!(1)));
    assert_eq!(rig.exec.resolve(id, Decision::Approve, &person()).unwrap_err().code, ErrorCode::NotFound);
    assert_eq!(rig.log().len(), 1, "applied exactly once");
    assert_eq!(rig.exec.approvals().len(), 0);

    // Approved for this request only: the next one asks again.
    assert_eq!(rig.exec.run(publish(&agent, 2)).unwrap_err().code, ErrorCode::Waiting);
}

#[test]
fn approving_for_the_session_widens_it_until_the_session_ends() {
    let rig = rig(json!({}));
    let info = rig.open_narrow();
    let (id, _) = queued(&rig.exec.run(publish(&info.actor, 1)).unwrap_err());
    rig.exec.resolve(id, Decision::ApproveForSession, &person()).unwrap();
    assert!(rig.exec.session(info.id).unwrap().grants.allows(Scope::Publish));
    rig.exec.run(publish(&info.actor, 2)).unwrap();
    assert_eq!(rig.at("p"), Some(json!(2)), "no card the second time");
    assert_eq!(rig.exec.approvals().len(), 0);

    rig.exec.end(info.id, EndReason::Closed).unwrap();
    let e = rig.exec.run(publish(&info.actor, 3)).unwrap_err();
    assert!(matches!(e.detail.as_deref(), Some(ErrorDetail::Ended { .. })), "{e:?}");
    // The grant was the session's: a new session from the same client starts without it.
    let again = rig.open_narrow();
    assert_eq!(rig.exec.run(publish(&again.actor, 4)).unwrap_err().code, ErrorCode::Waiting);
}

#[test]
fn a_rejection_carries_the_comment_and_nothing_runs() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let (id, _) = queued(&rig.exec.run(publish(&agent, 1)).unwrap_err());
    let rx = rig.subscribe();
    let comment = Some("not yet".to_owned());
    let resolution = rig.exec.resolve(id, Decision::Reject { comment: comment.clone() }, &person()).unwrap();
    assert_eq!(resolution.result, Resolved::Rejected { comment });
    assert_eq!(rig.at("p"), None);
    assert!(drain(&rx).iter().any(|e| matches!(e, Event::ApprovalResolved { resolution: r, .. } if r.id == id)));
}

#[test]
fn an_op_that_needs_a_person_queues_and_runs_confirmed_while_person_only_is_refused() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let (id, need) = queued(&rig.exec.run(wide(&agent, "test.confirm", json!({"path": "m", "value": 1}))).unwrap_err());
    assert_eq!(need, Need::Person { reason: "this switches the model".into() });
    let preview = rig.exec.approvals()[0].request.clone().unwrap().preview;
    assert_eq!(preview.patch.changes.len(), 1, "previewed as if confirmed");
    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
    assert_eq!(rig.at("m"), Some(json!(1)));

    let refused = rig.exec.run(wide(&agent, "test.person_only", json!({"path": "d"}))).unwrap_err();
    assert_eq!(refused.code, ErrorCode::Denied);
    assert!(matches!(refused.detail.as_deref(), Some(ErrorDetail::PersonOnly { .. })), "{refused:?}");
    assert!(rig.exec.approvals().is_empty(), "never queued");
}

#[test]
fn a_request_whose_dry_run_fails_is_refused_and_not_queued() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let e = rig.exec.run(wide(&agent, "test.publish", json!({"path": "missing/p", "value": 1}))).unwrap_err();
    assert_eq!(e.code, ErrorCode::NotFound, "the dry-run's own error: {e:?}");
    assert_eq!(rig.exec.approvals().len(), 0);
}

#[test]
fn ending_the_session_withdraws_its_approvals_and_no_timer_resolves_one() {
    let rig = rig(json!({}));
    let info = rig.open_narrow();
    let (id, _) = queued(&rig.exec.run(publish(&info.actor, 1)).unwrap_err());
    rig.clock.advance(Seconds(24.0 * 3600.0));
    assert_eq!(rig.exec.approvals().len(), 1, "a day later it still waits");

    let rx = rig.subscribe();
    let ended = rig.exec.end(info.id, EndReason::Disconnected).unwrap();
    assert_eq!(ended.withdrawn, vec![id]);
    assert_eq!(rig.exec.approvals().len(), 0);
    let withdrawn = drain(&rx).into_iter().find_map(|e| match e {
        Event::ApprovalResolved { resolution, .. } => Some(resolution),
        _ => None,
    });
    let withdrawn = withdrawn.unwrap();
    assert_eq!(
        (withdrawn.id, withdrawn.by, withdrawn.result),
        (id, None, Resolved::Withdrawn { reason: EndReason::Disconnected })
    );
    assert_eq!(rig.exec.resolve(id, Decision::Approve, &person()).unwrap_err().code, ErrorCode::NotFound);
}

#[test]
fn only_the_sessions_person_decides_and_only_with_a_decision_the_need_offers() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let (id, _) = queued(&rig.exec.run(publish(&agent, 1)).unwrap_err());
    assert_eq!(rig.exec.resolve(id, Decision::Approve, &agent).unwrap_err().code, ErrorCode::Denied, "an agent");
    assert_eq!(rig.exec.resolve(id, Decision::Approve, &bob()).unwrap_err().code, ErrorCode::Denied, "another person");
    let raise = Decision::RaiseBudget { budget: Budget::default() };
    assert_eq!(rig.exec.resolve(id, raise, &person()).unwrap_err().code, ErrorCode::Invalid);
    assert_eq!(rig.exec.approvals().len(), 1, "a refused decision leaves it queued");
}

#[test]
fn a_second_need_of_the_approved_run_becomes_a_new_approval() {
    let rig = rig(json!({}));
    let agent = rig.open_narrow().actor;
    let req = wide(&agent, "test.publish_confirm", json!({"path": "p", "value": 1}));
    let (first, need) = queued(&rig.exec.run(req).unwrap_err());
    assert_eq!(need, Need::Scope { scope: Scope::Publish });
    let resolution = rig.exec.resolve(first, Decision::Approve, &person()).unwrap();
    let Resolved::Failed { error } = resolution.result else { panic!("{:?}", resolution.result) };
    let (second, need) = queued(&error);
    assert!(matches!(need, Need::Person { .. }), "{need:?}");
    assert_ne!(first, second);
    assert_eq!(rig.exec.approvals().len(), 1);
    // The scope approved first is carried into the second approval: the person is asked once for each need.
    let resolution = rig.exec.resolve(second, Decision::Approve, &person()).unwrap();
    let Resolved::Applied { outcome } = resolution.result else { panic!("{:?}", resolution.result) };
    assert_eq!(outcome.applied.unwrap().approval, Some(ApprovalRef::new(second, person())));
    assert_eq!(rig.at("p"), Some(json!(1)));
}

#[test]
fn a_dry_run_that_needs_a_person_queues_too_and_is_answered_once_approved() {
    let rig = rig(json!({"p": 0}));
    let agent = rig.open_narrow().actor;
    let dry = Request { mode: Mode::DryRun, ..wide(&agent, "test.confirm", json!({"path": "p", "value": 5})) };
    let (id, _) = queued(&rig.exec.run(dry).unwrap_err());
    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    let Resolved::Applied { outcome } = resolution.result else { panic!("{:?}", resolution.result) };
    assert!(outcome.applied.is_none(), "a dry-run answered, nothing applied");
    assert_eq!(rig.at("p"), Some(json!(0)));
}

#[test]
fn a_session_holds_at_most_max_pending_approvals() {
    let rig = rig(json!({}));
    let spec = SessionSpec {
        grants: Grants::agent_default(),
        limits: Limits { max_pending: 2, ..Limits::default() },
        ..session_spec("claude")
    };
    let agent = rig.exec.open(spec).unwrap().actor;
    for value in [1, 2] {
        queued(&rig.exec.run(publish(&agent, value)).unwrap_err());
    }
    let e = rig.exec.run(publish(&agent, 3)).unwrap_err();
    assert_eq!(e.code, ErrorCode::Limited);
    let Some(ErrorDetail::RetryAfter { limit, .. }) = e.detail.as_deref() else { panic!("{e:?}") };
    assert_eq!(limit, "max_pending = 2");
}

#[test]
fn with_no_approver_the_same_requests_are_refused_with_a_hint() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_approver(false);
    let agent = exec.open(SessionSpec { grants: Grants::agent_default(), ..session_spec("headless") }).unwrap().actor;
    let missing = exec.run(publish(&agent, 1)).unwrap_err();
    assert_eq!(missing.code, ErrorCode::Denied);
    assert_eq!(missing.detail.as_deref(), Some(&ErrorDetail::MissingScope { scope: Scope::Publish }));
    assert!(
        missing.hint.as_deref().is_some_and(|h| h.contains("credential") && h.contains("Omuzaki Studio")),
        "{missing:?}"
    );
    let person_needed = exec.run(wide(&agent, "test.confirm", json!({"path": "m", "value": 1}))).unwrap_err();
    assert_eq!(person_needed.code, ErrorCode::Denied);
    assert!(matches!(person_needed.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{person_needed:?}");
    assert_eq!(exec.approvals().len(), 0);
    assert_eq!(exec.session_of(&agent).unwrap().state, SessionState::Active);
}
