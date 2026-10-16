//! Jobs and sessions (#1218, `ops-api.md` §8.3, ADR 0036 decisions 5, 6 and 11, agent-runtime D4): a stop and an
//! end cancel a session's jobs and discard their results, a session runs at most 4 jobs and has at most 16 waiting,
//! its jobs reserve its money, `request_budget` asks before running out, and the person's Stop closes the open
//! transaction as one step.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, LazyLock, Mutex};

use omuzaki_ops_api::{
    Actor, Budget, CancelReason, CostHint, Decision, ErrorCode, ErrorDetail, Grants, JobCtx, JobId, JobState, Jobs,
    Limits, Need, Op, OpCtx, OpError, OpKind, OpMeta, PauseReason, Plan, Request, Runner, Scope, SessionInfo,
    SessionSpec, SessionState, Sessions, Spend, StopStep, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::asks::{queued, wide};
use super::{Rig, path, person, rig, session_spec};

type Gate = (Sender<()>, Arc<Mutex<Receiver<()>>>);

static GATES: LazyLock<Mutex<HashMap<String, Gate>>> = LazyLock::new(Mutex::default);

/// The test's side of a job's gate: `entered` hears `work` start, `release` lets it finish.
struct Held {
    entered: Receiver<()>,
    release: Sender<()>,
}

impl Held {
    fn new(key: &str) -> Self {
        let (entered_tx, entered) = channel();
        let (release, release_rx) = channel();
        GATES.lock().unwrap().insert(key.to_owned(), (entered_tx, Arc::new(Mutex::new(release_rx))));
        Self { entered, release }
    }
}

/// A job that spends `spend` micro-dollars, signals, waits at its gate, then writes `true` at `key`. `usd` is its
/// estimate (which puts it on the network lane); `net` puts it there without one.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SessionJob {
    key: String,
    #[serde(default)]
    usd: Option<f32>,
    #[serde(default)]
    spend: u64,
    #[serde(default)]
    net: bool,
}

impl Op for SessionJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.session_job",
            summary: "A job a session runs.",
            kind: OpKind::Job,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let cost =
            (self.usd.is_some() || self.net).then(|| CostHint { usd: self.usd.or(Some(0.0)), ..CostHint::default() });
        Ok(Plan { cost, ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        if self.spend > 0 {
            job.spend(self.spend)?;
        }
        // Taken out first: the map's lock must not be held while this job waits at its gate.
        let gate = GATES.lock().unwrap().get(&self.key).cloned();
        if let Some((entered, release)) = gate {
            entered.send(()).unwrap();
            let _ = release.lock().unwrap().recv();
        }
        job.checkpoint("done", 1.0)?;
        Ok(json!(true))
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&path(&self.key)).cloned();
        Ok(Plan::edit(omuzaki_doc_api::Patch::new().with(path(&self.key), before, Some(result))))
    }
}
register_op!(SessionJob);

/// A session for alice that may spend `usd_micros`, with every scope.
fn spender(rig: &Rig, usd_micros: u64) -> SessionInfo {
    let spec = SessionSpec { budget: Budget { usd_micros, ..Budget::default() }, ..session_spec("worker") };
    rig.exec.open(spec).unwrap()
}

fn job_of(actor: &Actor, key: &str, extra: Value) -> Request {
    let mut args = json!({"key": key});
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    wide(actor, "test.session_job", args)
}

fn state(rig: &Rig, id: JobId) -> JobState {
    rig.exec.job(id, &Grants::person()).unwrap().unwrap().state
}

#[test]
fn ending_a_session_cancels_its_jobs_and_keeps_the_money_they_spent() {
    let rig = rig(json!({}));
    let session = spender(&rig, 1_000_000);
    let held = Held::new("paid");
    let info = rig.exec.start(job_of(&session.actor, "paid", json!({"usd": 0.5, "spend": 300_000}))).unwrap();
    held.entered.recv().unwrap();
    let ended = rig.exec.end(session.id, omuzaki_ops_api::EndReason::Closed).unwrap();
    assert_eq!(ended.cancelled, vec![info.id]);
    held.release.send(()).unwrap();
    let job = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(job.state, JobState::Cancelled { reason: CancelReason::SessionEnded, .. }), "{:?}", job.state);
    assert_eq!(rig.at("paid"), None, "its change is discarded");
    assert_eq!(job.spent_usd_micros, 300_000, "a paid loss stays visible");
    assert_eq!(rig.exec.session(session.id).unwrap().spent.usd_micros, 300_000, "and the session's spend keeps it");
}

#[test]
fn a_session_runs_at_most_four_jobs_and_has_at_most_sixteen_waiting() {
    let rig = rig(json!({}));
    let session = spender(&rig, 0);
    let held: Vec<Held> = (0..4).map(|n| Held::new(&format!("r{n}"))).collect();
    for n in 0..4 {
        rig.exec.start(job_of(&session.actor, &format!("r{n}"), json!({"net": true}))).unwrap();
    }
    for gate in &held {
        gate.entered.recv().unwrap();
    }
    let fifth = rig.exec.start(job_of(&session.actor, "r4", json!({"net": true}))).unwrap_err();
    assert_eq!(fifth.code, ErrorCode::Limited);
    let Some(ErrorDetail::RetryAfter { limit, .. }) = fifth.detail.as_deref() else { panic!("{fifth:?}") };
    assert_eq!(limit, "session_running_jobs = 4");
    // Another session is unaffected.
    let other = spender(&rig, 0);
    let free = rig.exec.start(job_of(&other.actor, "o", json!({"net": true}))).unwrap();
    rig.exec.wait(free.id, None).unwrap();
    for gate in held {
        gate.release.send(()).unwrap();
    }
}

#[test]
fn the_seventeenth_waiting_job_of_a_session_is_limited() {
    let rig = rig(json!({}));
    // The person fills the network lane, so the session's jobs wait.
    let lane: Vec<Held> = (0..8).map(|n| Held::new(&format!("lane{n}"))).collect();
    for n in 0..8 {
        rig.exec.start(job_of(&person(), &format!("lane{n}"), json!({"net": true}))).unwrap();
    }
    for gate in &lane {
        gate.entered.recv().unwrap();
    }
    let session = spender(&rig, 0);
    for n in 0..16 {
        let info = rig.exec.start(job_of(&session.actor, &format!("q{n}"), json!({"net": true}))).unwrap();
        assert_eq!(info.state, JobState::Queued);
    }
    let e = rig.exec.start(job_of(&session.actor, "q16", json!({"net": true}))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Limited);
    rig.exec.shutdown_jobs();
    drop(lane);
}

#[test]
fn a_job_start_counts_as_an_edit_against_the_rate_limit() {
    let rig = rig(json!({}));
    let spec = SessionSpec {
        limits: Limits { edit_burst: 1, edits_per_minute: 60, ..Limits::default() },
        ..session_spec("one")
    };
    let session = rig.exec.open(spec).unwrap();
    rig.exec.start(job_of(&session.actor, "j1", json!({}))).unwrap();
    let e = rig.exec.start(job_of(&session.actor, "j2", json!({}))).unwrap_err();
    assert_eq!(e.code, ErrorCode::Limited);
    assert!(
        matches!(e.detail.as_deref(), Some(ErrorDetail::RetryAfter { limit, .. }) if limit == "edits_per_minute = 60")
    );
}

#[test]
fn a_second_concurrent_job_that_does_not_fit_the_budget_waits_for_a_person() {
    let rig = rig(json!({}));
    let session = spender(&rig, 1_000_000);
    let held = Held::new("b1");
    rig.exec.start(job_of(&session.actor, "b1", json!({"usd": 0.6}))).unwrap();
    held.entered.recv().unwrap();
    let (_, need) = queued(&rig.exec.start(job_of(&session.actor, "b2", json!({"usd": 0.6}))).unwrap_err());
    assert!(matches!(need, Need::Budget { needed: Spend { usd_micros: 600_000, .. }, .. }), "{need:?}");
    held.release.send(()).unwrap();
}

#[test]
fn a_narrowed_session_fails_its_jobs_finish_and_only_its_agent_or_a_person_cancels() {
    let rig = rig(json!({}));
    let session = spender(&rig, 0);
    let held = Held::new("n1");
    let info = rig.exec.start(job_of(&session.actor, "n1", json!({}))).unwrap();
    held.entered.recv().unwrap();
    rig.exec.set_grants(session.id, Grants::of([Scope::Read]), &person()).unwrap();
    held.release.send(()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(&ended.state, JobState::Failed { error } if error.code == ErrorCode::Denied), "{:?}", ended.state);

    // A second session's job: another agent may not cancel it, its person may.
    let (owner, other) = (spender(&rig, 0), spender(&rig, 0));
    let held = Held::new("n2");
    let info = rig.exec.start(job_of(&owner.actor, "n2", json!({}))).unwrap();
    held.entered.recv().unwrap();
    assert_eq!(rig.exec.cancel_session(owner.id, &other.actor).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(rig.exec.cancel(info.id, &other.actor).unwrap_err().code, ErrorCode::Denied);
    assert_eq!(rig.exec.cancel_session(owner.id, &person()).unwrap(), vec![info.id]);
    held.release.send(()).unwrap();
    assert!(matches!(rig.exec.wait(info.id, None).unwrap().state, JobState::Cancelled { .. }));
}

#[test]
fn request_budget_asks_before_running_out() {
    let rig = rig(json!({"a": 0}));
    let session = spender(&rig, 1_000_000);
    rig.exec.request_budget(session.id, Spend { usd_micros: 400_000, ..Spend::default() }).unwrap();
    assert_eq!(rig.exec.approvals().len(), 0, "it fits: nothing changes");

    let (id, need) =
        queued(&rig.exec.request_budget(session.id, Spend { usd_micros: 2_000_000, ..Spend::default() }).unwrap_err());
    assert!(matches!(need, Need::Budget { .. }));
    let paused = rig.exec.run(wide(&session.actor, "test.set", json!({"path": "a", "value": 1}))).unwrap_err();
    assert!(
        matches!(paused.detail.as_deref(), Some(ErrorDetail::Paused { reason: PauseReason::Budget, .. })),
        "{paused:?}"
    );
    assert!(matches!(
        rig.exec.session(session.id).unwrap().state,
        SessionState::Paused { reason: PauseReason::Budget, .. }
    ));
    let raise = Decision::RaiseBudget { budget: Budget { usd_micros: 3_000_000, ..Budget::default() } };
    rig.exec.resolve(id, raise, &person()).unwrap();
    assert_eq!(rig.exec.session(session.id).unwrap().state, SessionState::Active);

    let (id, _) =
        queued(&rig.exec.request_budget(session.id, Spend { usd_micros: 9_000_000, ..Spend::default() }).unwrap_err());
    rig.exec.resolve(id, Decision::Reject { comment: None }, &person()).unwrap();
    assert!(matches!(
        rig.exec.session(session.id).unwrap().state,
        SessionState::Paused { reason: PauseReason::Budget, .. }
    ));
}

/// An agent session with an open transaction holding one edit (`a` set to `value`), labelled "work".
fn working(rig: &Rig, value: i64) -> (SessionInfo, omuzaki_ops_api::TxId) {
    let session = spender(rig, 0);
    let tx = rig.exec.begin("work", &session.actor);
    let edit = Request { tx: Some(tx), ..wide(&session.actor, "test.set", json!({"path": "a", "value": value})) };
    rig.exec.run(edit).unwrap();
    (session, tx)
}

#[test]
fn stop_commits_the_open_transaction_as_one_step_in_any_state() {
    // Running: the transaction is committed as "work (stopped)", and one undo reverts it whole.
    let rig = rig(json!({"a": 0, "p": 0}));
    let (session, tx) = working(&rig, 1);
    let stopped = rig.exec.stop(session.id, &person()).unwrap();
    assert_eq!(stopped.step, StopStep::Committed(tx));
    assert!(rig.exec.session(session.id).unwrap().open_tx.is_none());
    assert!(matches!(
        rig.exec.session(session.id).unwrap().state,
        SessionState::Paused { reason: PauseReason::Person, .. }
    ));
    assert_eq!(rig.undo().unwrap().as_deref(), Some("work (stopped)"));
    assert_eq!(rig.at("a"), Some(json!(0)));
    assert_eq!(rig.redo().unwrap().as_deref(), Some("work (stopped)"));

    // A second stop: no step, no event.
    let rx = rig.subscribe();
    let again = rig.exec.stop(session.id, &person()).unwrap();
    assert_eq!((again.step, again.jobs.len()), (StopStep::None, 0));
    assert_eq!(super::drain(&rx).len(), 0);

    // Idle: nothing to close.
    let idle = spender(&rig, 0);
    assert_eq!(rig.exec.stop(idle.id, &person()).unwrap().step, StopStep::None);

    // Waiting for approval: the approval stays queued; the transaction is committed.
    let (waiting, tx) = working(&rig, 2);
    rig.exec.set_grants(waiting.id, Grants::agent_default(), &person()).unwrap();
    let publish = Request { tx: Some(tx), ..wide(&waiting.actor, "test.publish", json!({"path": "p", "value": 1})) };
    queued(&rig.exec.run(publish).unwrap_err());
    assert_eq!(rig.exec.stop(waiting.id, &person()).unwrap().step, StopStep::Committed(tx));
    assert_eq!(rig.exec.approvals().len(), 1, "the approval stays queued");
}

#[test]
fn stop_after_a_budget_pause_keeps_the_budget_pause_and_leaves_one_step() {
    let rig = rig(json!({"a": 0}));
    let (session, tx) = working(&rig, 1);
    queued(&rig.exec.request_budget(session.id, Spend { usd_micros: 1, ..Spend::default() }).unwrap_err());
    let stopped = rig.exec.stop(session.id, &person()).unwrap();
    assert_eq!(stopped.step, StopStep::Committed(tx));
    assert!(matches!(
        rig.exec.session(session.id).unwrap().state,
        SessionState::Paused { reason: PauseReason::Budget, .. }
    ));
    assert_eq!(rig.undo().unwrap().as_deref(), Some("work (stopped)"));
    assert_eq!(rig.at("a"), Some(json!(0)));
    assert!(rig.redo().unwrap().is_some());
    assert_eq!(rig.at("a"), Some(json!(1)));
}

#[test]
fn stop_cancels_the_sessions_jobs_and_discards_a_plain_finish() {
    let rig = rig(json!({}));
    let session = spender(&rig, 0);
    let held = Held::new("s1");
    let running = rig.exec.start(job_of(&session.actor, "s1", json!({}))).unwrap();
    held.entered.recv().unwrap();
    let stopped = rig.exec.stop(session.id, &person()).unwrap();
    assert_eq!(stopped.jobs, vec![running.id]);
    held.release.send(()).unwrap();
    rig.exec.wait(running.id, None).unwrap();
    assert!(matches!(state(&rig, running.id), JobState::Cancelled { reason: CancelReason::Stop, .. }));
    assert_eq!(rig.at("s1"), None, "its finish was discarded");
}

#[test]
fn only_the_sessions_person_may_stop_it() {
    let rig = rig(json!({}));
    let session = spender(&rig, 0);
    let bob = Actor::Person { name: "bob".into() };
    for by in [session.actor.clone(), bob] {
        assert_eq!(rig.exec.stop(session.id, &by).unwrap_err().code, ErrorCode::Denied);
    }
    assert_eq!(rig.exec.session(session.id).unwrap().state, SessionState::Active, "nothing changed");
}
