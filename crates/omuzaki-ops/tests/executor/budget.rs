//! Session budgets (#403, design `agent-sessions.md` §8.1): money charged at apply in exact micro-dollars, tokens
//! charged from outside, active time on the runner's clock; passing a cap pauses the session and asks a person.

use omuzaki_core::time::Seconds;
use omuzaki_ops_api::{
    Budget, Decision, ErrorCode, ErrorDetail, Event, Grants, Need, PauseReason, Request, Resolved, Runner, Scope,
    SessionChange, SessionId, SessionInfo, SessionSpec, SessionState, Sessions, Spend,
};
use serde_json::json;

use super::asks::{queued, wide};
use super::{Rig, drain, person, rig, session_spec};

const DOLLAR: u64 = 1_000_000;

/// A session that may spend, with `budget`.
fn spender(rig: &Rig, budget: Budget) -> SessionInfo {
    let spec = SessionSpec { grants: Grants::agent_default().with(Scope::Spend), budget, ..session_spec("claude") };
    rig.exec.open(spec).unwrap()
}

fn paid(session: &SessionInfo, path: &str, usd: f32) -> Request {
    wide(&session.actor, "test.paid", json!({"path": path, "value": path, "usd": usd}))
}

fn dollars(usd: u64) -> Budget {
    Budget { usd_micros: usd, ..Budget::default() }
}

fn state(rig: &Rig, id: SessionId) -> SessionState {
    rig.exec.session(id).unwrap().state
}

/// Two $0.40 ops fit a $1.00 cap, the third pauses the session with a `Budget` approval.
fn over_the_cap(rig: &Rig) -> (SessionInfo, omuzaki_ops_api::ApprovalId) {
    let session = spender(rig, dollars(DOLLAR));
    rig.exec.run(paid(&session, "a", 0.4)).unwrap();
    rig.exec.run(paid(&session, "b", 0.4)).unwrap();
    let (id, need) = queued(&rig.exec.run(paid(&session, "c", 0.4)).unwrap_err());
    let left = Spend { usd_micros: 200_000, tokens: u64::MAX, seconds: u64::MAX };
    assert_eq!(need, Need::Budget { needed: Spend { usd_micros: 400_000, ..Spend::default() }, left });
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 800_000, "charged exactly");
    assert!(matches!(state(rig, session.id), SessionState::Paused { reason: PauseReason::Budget, .. }));
    assert_eq!(rig.at("c"), None);
    (session, id)
}

#[test]
fn raising_the_budget_resumes_the_session_and_runs_nothing_by_itself() {
    let rig = rig(json!({}));
    let (session, id) = over_the_cap(&rig);
    // The agent sees the pause.
    let e = rig.exec.run(paid(&session, "d", 0.1)).unwrap_err();
    assert!(matches!(e.detail.as_deref(), Some(ErrorDetail::Paused { reason: PauseReason::Budget, .. })), "{e:?}");

    let raise = Decision::RaiseBudget { budget: dollars(2 * DOLLAR) };
    let still = rig.exec.resolve(id, raise, &person()).unwrap_err();
    assert_eq!(queued(&still).0, id, "the request still waits for Approve or Reject");
    assert_eq!(state(&rig, session.id), SessionState::Active);
    assert_eq!((rig.at("c"), rig.exec.session(session.id).unwrap().spent.usd_micros), (None, 800_000));

    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
    assert_eq!(rig.at("c"), Some(json!("c")));
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 1_200_000);
}

#[test]
fn approving_runs_the_one_request_over_the_cap_and_the_session_stays_paused() {
    let rig = rig(json!({}));
    let (session, id) = over_the_cap(&rig);
    let resolution = rig.exec.resolve(id, Decision::Approve, &person()).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }), "{:?}", resolution.result);
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 1_200_000);
    assert!(matches!(state(&rig, session.id), SessionState::Paused { reason: PauseReason::Budget, .. }));
}

#[test]
fn a_rejection_leaves_the_session_paused_and_a_new_budget_resumes_it() {
    let rig = rig(json!({}));
    let (session, id) = over_the_cap(&rig);
    rig.exec.resolve(id, Decision::Reject { comment: None }, &person()).unwrap();
    assert_eq!(rig.at("c"), None);
    assert!(matches!(state(&rig, session.id), SessionState::Paused { reason: PauseReason::Budget, .. }));
    rig.exec.set_budget(session.id, dollars(DOLLAR / 2), &person()).unwrap();
    assert!(matches!(state(&rig, session.id), SessionState::Paused { .. }), "$0.50 does not cover the $0.80 spent");
    assert_eq!(rig.exec.resume(session.id, &person()).unwrap_err().code, ErrorCode::Denied, "still past the cap");
    rig.exec.set_budget(session.id, dollars(DOLLAR), &person()).unwrap();
    assert_eq!(state(&rig, session.id), SessionState::Active, "$1.00 covers what was spent");
}

#[test]
fn a_charge_of_tokens_past_the_cap_pauses_with_a_request_less_approval() {
    let rig = rig(json!({}));
    let session = spender(&rig, Budget { tokens: Some(1_000), ..Budget::default() });
    let rx = rig.subscribe();
    let charged = rig.exec.charge(session.id, Spend { tokens: 600, ..Spend::default() }).unwrap();
    assert_eq!((charged.spent.tokens, charged.state), (600, SessionState::Active));
    let after = rig.exec.charge(session.id, Spend { tokens: 600, usd_micros: 7, seconds: 0 }).unwrap();
    assert!(matches!(after.state, SessionState::Paused { reason: PauseReason::Budget, .. }));
    assert_eq!((after.spent.tokens, after.spent.usd_micros), (1_200, 7));
    let pending = rig.exec.approvals();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].request.is_none() && matches!(pending[0].need, Need::Budget { .. }));
    let spent =
        drain(&rx).iter().filter(|e| matches!(e, Event::SessionChanged { change: SessionChange::Spent, .. })).count();
    assert_eq!(spent, 2, "one Spent per charge");
    // A third charge adds, and asks nothing more: one approval per pause.
    rig.exec.charge(session.id, Spend { tokens: 1, ..Spend::default() }).unwrap();
    assert_eq!(rig.exec.approvals().len(), 1);

    // With no request, Approve is not offered; raising the budget resolves it and resumes.
    let id = pending[0].id;
    assert_eq!(rig.exec.resolve(id, Decision::Approve, &person()).unwrap_err().code, ErrorCode::Invalid);
    let raise = Decision::RaiseBudget { budget: Budget { usd_micros: 1_000, tokens: Some(5_000), seconds: None } };
    let resolution = rig.exec.resolve(id, raise, &person()).unwrap();
    assert!(matches!(resolution.result, Resolved::Applied { .. }));
    assert_eq!(state(&rig, session.id), SessionState::Active);

    let time = rig.exec.charge(session.id, Spend { seconds: 1, ..Spend::default() }).unwrap_err();
    assert_eq!(time.field.as_deref(), Some("seconds"), "time is the executor's to count");
}

#[test]
fn active_time_past_the_cap_pauses_at_the_next_request_and_paused_time_does_not_count() {
    let rig = rig(json!({}));
    let session = spender(&rig, Budget { seconds: Some(60), ..Budget::default() });
    let free = |path: &str| wide(&session.actor, "test.free", json!({"path": path, "value": 1}));
    rig.clock.advance(Seconds(10.0));
    rig.exec.pause(session.id, &person()).unwrap();
    rig.clock.advance(Seconds(500.0));
    rig.exec.resume(session.id, &person()).unwrap();
    rig.clock.advance(Seconds(50.0));
    rig.exec.run(free("a")).unwrap();
    assert_eq!(rig.exec.session(session.id).unwrap().spent.seconds, 60, "500 paused seconds are left out");
    rig.clock.advance(Seconds(1.5));
    let (_, need) = queued(&rig.exec.run(free("b")).unwrap_err());
    assert!(matches!(need, Need::Budget { left: Spend { seconds: 0, .. }, .. }), "{need:?}");
    assert!(matches!(state(&rig, session.id), SessionState::Paused { reason: PauseReason::Budget, .. }));
}

#[test]
fn float_costs_sum_exactly_in_micro_dollars() {
    let rig = rig(json!({}));
    let session = spender(&rig, dollars(DOLLAR));
    rig.exec.run(paid(&session, "a", 0.1)).unwrap();
    rig.exec.run(paid(&session, "b", 0.2)).unwrap();
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 300_000);
}

#[test]
fn persons_have_no_budget() {
    let rig = rig(json!({}));
    let req = Request { actor: person(), ..paid(&spender(&rig, Budget::default()), "a", 99.0) };
    rig.exec.run(req).unwrap();
    assert_eq!(rig.exec.approvals().len(), 0);
}
