//! Plain jobs through the executor: plan, work, finish; lanes, caps, cancel and the money reservation.
//!
//! Every test is driven by channels and the fake clock: a job's `work` waits on a gate the test opens, and time
//! moves only when the test moves it (then calls `poll_jobs`). Nothing sleeps.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::Patch;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops::Executor;
use omuzaki_ops_api::{
    CancelFlag, CancelReason, CostHint, ErrorCode, ErrorDetail, Event, FsMode, Grants, JobCtx, JobId, JobInfo,
    JobShape, JobState, Jobs, Lane, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, ProjectDir, Request, Runner, Scope,
    SessionId, Sessions, Undo, register_op,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Rig, agent, agent_request, drain, open_agents, path, person, request, rig};

/// What a test job's `work` does once it is running.
#[derive(Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
enum How {
    /// Returns at once with what the context offers.
    #[default]
    Echo,
    /// Signals it is running, waits for the gate and ignores cancellation.
    Block,
    /// Like `Block`, but polls `checkpoint` and stops with `Cancelled`.
    Coop,
    /// Runs until cancelled, then returns what it completed.
    Partial,
    /// Runs until cancelled, then fails with its own error.
    FailWhenCancelled,
    /// Runs until cancelled, then panics.
    PanicWhenCancelled,
    /// Runs until cancelled, signals again, waits for the gate, then fails with its own error.
    FailAfterTheGate,
    /// Spends `before`, signals, waits for the gate, then spends `after` (late, past any cancel).
    Spend { before: u64, after: u64 },
    /// Spends twice `each`, so the second report passes a cap of less than twice that.
    Overspend { each: u64 },
    /// Reports progress 100 times, signals, waits for the gate, reports once more.
    Progress,
    /// Fails with its own error.
    Fail,
    /// Panics.
    Panic,
    /// Returns the nice value its thread runs at (Linux; null elsewhere).
    Nice,
    /// Reports progress and money from inside `JobCtx::read`, where the document lock is held.
    ReadSignals,
    /// Opens `path` for reading through the job's file access and returns the path it was allowed.
    Open { path: String },
    /// Returns at once; its `finish` signals, then holds the document lock until the job is cancelled.
    SlowFinish,
    /// Reads the document from inside a read, reporting progress in the inner one (#1593).
    NestedRead,
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct TestJob {
    /// Names the gate and the document path the finish writes.
    key: String,
    #[serde(default)]
    how: How,
    #[serde(default)]
    usd: Option<f32>,
    #[serde(default)]
    seconds: Option<f32>,
    #[serde(default)]
    gpu: bool,
    /// Plans a patch, which a job must not.
    #[serde(default)]
    bad_plan: bool,
}

struct WorkGate {
    entered: Sender<()>,
    release: Mutex<Receiver<()>>,
}

static GATES: LazyLock<Mutex<HashMap<String, Arc<WorkGate>>>> = LazyLock::new(Mutex::default);
static FLAGS: LazyLock<Mutex<HashMap<String, CancelFlag>>> = LazyLock::new(Mutex::default);

/// The test's side of a gate: `entered` hears `work` start, `release` lets it go (dropping it does too).
struct Gate {
    entered: Receiver<()>,
    release: Sender<()>,
}

impl Gate {
    fn new(key: &str) -> Self {
        let (entered_tx, entered) = channel();
        let (release, release_rx) = channel();
        let gate = Arc::new(WorkGate { entered: entered_tx, release: Mutex::new(release_rx) });
        GATES.lock().unwrap().insert(key.to_owned(), gate);
        Self { entered, release }
    }

    fn wait_entered(&self) {
        self.entered.recv().unwrap();
    }
}

impl WorkGate {
    fn announce(&self) {
        self.entered.send(()).unwrap();
    }

    /// Waits for the test to open the gate; true if the gate is still open to wait on.
    fn wait(&self) {
        let _ = self.release.lock().unwrap().recv();
    }

    /// True once the test has released the gate or dropped it.
    fn released(&self) -> bool {
        !matches!(self.release.lock().unwrap().try_recv(), Err(TryRecvError::Empty))
    }
}

impl Op for TestJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.job",
            summary: "A job the tests drive.",
            kind: OpKind::Job,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let patch = if self.bad_plan { Patch::new().with(path(&self.key), None, Some(json!(1))) } else { Patch::new() };
        Ok(Plan {
            patch,
            output: Some(json!({"chose": self.key})),
            cost: Some(CostHint { seconds: self.seconds, gpu_mib: self.gpu.then_some(512), usd: self.usd }),
            effect: None,
        })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let gate = GATES.lock().unwrap().get(&self.key).cloned();
        match &self.how {
            How::Echo | How::SlowFinish => {
                FLAGS.lock().unwrap().insert(self.key.clone(), job.cancel_flag());
                Ok(json!({"output": job.plan_output(), "reserved": job.reserved_usd_micros()}))
            }
            How::Block => {
                let gate = gate.unwrap();
                gate.announce();
                gate.wait();
                Ok(json!("done"))
            }
            How::Coop => {
                let gate = gate.unwrap();
                gate.announce();
                while !gate.released() {
                    job.checkpoint("working", 0.5)?;
                    std::thread::yield_now();
                }
                Ok(json!("done"))
            }
            How::Partial => {
                gate.unwrap().announce();
                while !job.is_cancelled() {
                    std::thread::yield_now();
                }
                Ok(json!("partial"))
            }
            How::FailWhenCancelled | How::PanicWhenCancelled => {
                gate.unwrap().announce();
                while !job.is_cancelled() {
                    std::thread::yield_now();
                }
                assert!(matches!(self.how, How::FailWhenCancelled), "the job's work panicked after its cancel");
                Err(OpError::invalid("key", "boom after the cancel"))
            }
            How::FailAfterTheGate => {
                let gate = gate.unwrap();
                gate.announce();
                while !job.is_cancelled() {
                    std::thread::yield_now();
                }
                gate.announce();
                gate.wait();
                Err(OpError::invalid("key", "boom after the cancel"))
            }
            How::Spend { before, after } => {
                let gate = gate.unwrap();
                job.spend(*before)?;
                gate.announce();
                gate.wait();
                job.spend(*after)?;
                Ok(json!("done"))
            }
            How::Overspend { each } => {
                job.spend(*each)?;
                job.spend(*each)?;
                Ok(json!("done"))
            }
            How::Progress => {
                let gate = gate.unwrap();
                for n in 0..100_u8 {
                    job.progress("step", f32::from(n) / 100.0);
                }
                gate.announce();
                gate.wait();
                job.progress("last", 1.0);
                Ok(json!("done"))
            }
            How::Open { path } => Ok(json!(job.fs_access(path, FsMode::Read)?.to_string())),
            How::Fail => Err(OpError::invalid("key", "boom")),
            How::Panic => panic!("the job's work panicked"),
            How::Nice => Ok(json!(thread_nice())),
            How::NestedRead => {
                let (outer, inner) = job.read(|doc| {
                    let outer = doc.get(&path("seen")).cloned();
                    let inner = job.read(|again| {
                        job.progress("inner", 0.5);
                        again.get(&path("seen")).cloned()
                    });
                    (outer, inner)
                });
                Ok(json!({"same": outer == inner && outer.is_some()}))
            }
            How::ReadSignals => {
                let refused = job.read(|_| {
                    job.progress("reading", 0.25);
                    job.spend(1).is_err()
                });
                Ok(json!({"refused": refused}))
            }
        }
    }

    fn finish(&self, _ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        if matches!(self.how, How::SlowFinish) {
            let flag = FLAGS.lock().unwrap().get(&self.key).cloned().unwrap();
            GATES.lock().unwrap().get(&self.key).unwrap().announce();
            while !omuzaki_ops_api::Cancel::is_cancelled(&flag) {
                std::thread::yield_now();
            }
        }
        Ok(Plan::edit(Patch::new().with(path(&self.key), None, Some(result))))
    }
}
register_op!(TestJob);

/// A job that declares itself a driver.
#[derive(Serialize, Deserialize, JsonSchema)]
struct TestDriver {}

impl Op for TestDriver {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.driver_job",
            summary: "A driver job.",
            kind: OpKind::Job,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn job_shape() -> JobShape {
        JobShape::Driver
    }
}
register_op!(TestDriver);

/// The calling thread's nice value, from `/proc/thread-self/stat` (field 19).
#[cfg(target_os = "linux")]
fn thread_nice() -> Option<i64> {
    let stat = std::fs::read_to_string("/proc/thread-self/stat").ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(16)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
fn thread_nice() -> Option<i64> {
    None
}

/// The first `JobChanged` for `id` in an ended state, within `secs` of wall time.
fn ended_event(events: &Receiver<Event>, id: JobId, secs: u64) -> Option<JobInfo> {
    let until = Instant::now() + Duration::from_secs(secs);
    loop {
        let left = until.saturating_duration_since(Instant::now());
        match events.recv_timeout(left).ok()? {
            Event::JobChanged { job, .. }
                if job.id == id && !matches!(job.state, JobState::Queued | JobState::Running) =>
            {
                return Some(job);
            }
            _ => {}
        }
    }
}

fn job(key: &str, how: &Value) -> Request {
    request("test.job", json!({"key": key, "how": how}))
}

fn job_with(key: &str, how: &Value, extra: Value) -> Request {
    let mut args = json!({"key": key, "how": how});
    if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
        args.extend(extra.clone());
    }
    request("test.job", args)
}

/// A rig whose jobs share a budget of `usd_micros`.
fn budgeted(usd_micros: u64) -> Rig {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec =
        Executor::new(Box::new(MemDoc::from_value(json!({}))), clock.clone()).unwrap().with_job_budget(usd_micros);
    open_agents(&exec);
    Rig { exec, clock }
}

fn live(rig: &Rig, id: JobId) -> JobInfo {
    rig.exec.job(id, &Grants::person()).unwrap().unwrap()
}

fn code(error: &OpError) -> ErrorCode {
    error.code
}

/// Moves the fake clock and lets the manager see it.
fn advance(rig: &Rig, secs: f64) {
    rig.clock.advance(Seconds(secs));
    rig.exec.poll_jobs();
}

#[test]
fn a_running_job_does_not_block_another_actor_and_its_finish_is_one_undo_step() {
    let rig = rig(json!({"other": 0}));
    let gate = Gate::new("a1");
    let info = rig.exec.start(job("a1", &json!("block"))).unwrap();
    gate.wait_entered();
    assert_eq!(live(&rig, info.id).state, JobState::Running);

    let other = rig.bobs_agent();
    let began = Instant::now();
    rig.set_as(&other, "other", json!(1));
    assert!(began.elapsed() < Duration::from_millis(5), "an edit by another actor waited for the job");

    gate.release.send(()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { seq: Some(_), .. }), "{:?}", ended.state);
    assert_eq!(rig.at("a1"), Some(json!("done")));

    // One undo step, on the requesting actor's history: the other person's agent's edit is not touched.
    assert_eq!(
        rig.undo().unwrap().as_deref(),
        Some("A job the tests drive."),
        "a one-op step is labelled by its summary"
    );
    assert_eq!(rig.at("a1"), None);
    assert_eq!(rig.at("other"), Some(json!(1)));
    assert_eq!(rig.undo().unwrap(), None);
}

#[test]
fn run_waits_for_a_job_and_returns_its_outcome() {
    let rig = rig(json!({}));
    let outcome = rig.exec.run(job_with("r1", &json!("echo"), json!({"usd": 0.5}))).unwrap();
    assert_eq!(outcome.plan.patch.changes.len(), 1);
    assert!(outcome.applied.is_some());
    // `plan_output` and the reservation reach `work`.
    assert_eq!(rig.at("r1"), Some(json!({"output": {"chose": "r1"}, "reserved": 500_000})));
}

#[test]
fn a_job_plan_with_a_patch_is_internal_even_in_a_dry_run() {
    let rig = rig(json!({}));
    for mode in [Mode::Apply, Mode::DryRun] {
        let req = Request { mode, ..job_with("b1", &json!("echo"), json!({"bad_plan": true})) };
        assert_eq!(code(&rig.exec.run(req).unwrap_err()), ErrorCode::Internal);
    }
    assert_eq!(rig.exec.jobs(&Grants::person()).unwrap().len(), 0);
}

#[test]
fn a_plain_job_cannot_join_a_transaction() {
    let rig = rig(json!({}));
    let tx = rig.exec.begin("t", &person());
    let req = Request { tx: Some(tx), ..job("t1", &json!("echo")) };
    let error = rig.exec.run(req.clone()).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Invalid);
    assert_eq!(error.field.as_deref(), Some("tx"));
    assert_eq!(code(&rig.exec.start(req).unwrap_err()), ErrorCode::Invalid);
}

#[test]
fn a_dry_run_is_the_plan_alone_and_never_starts_work() {
    let rig = rig(json!({}));
    let gate = Gate::new("d1");
    let req = Request { mode: Mode::DryRun, ..job_with("d1", &json!("block"), json!({"usd": 0.5})) };
    let outcome = rig.exec.run(req.clone()).unwrap();
    assert_eq!(outcome.plan.cost.and_then(|c| c.usd), Some(0.5));
    assert!(outcome.applied.is_none());
    assert_eq!(rig.exec.jobs(&Grants::person()).unwrap().len(), 0);
    assert!(matches!(gate.entered.try_recv(), Err(TryRecvError::Empty)), "work ran");
    assert_eq!(code(&rig.exec.start(req).unwrap_err()), ErrorCode::Invalid, "Jobs::start needs apply mode");
}

#[test]
fn a_job_start_that_is_not_a_job_is_invalid() {
    let rig = rig(json!({"a": 1}));
    let error = rig.exec.start(request("test.set", json!({"path": "a", "value": 2}))).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Invalid);
}

#[test]
fn the_cpu_lane_holds_its_width_and_the_next_job_waits_for_a_slot() {
    let rig = rig(json!({}));
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let width = (cores / 2).clamp(1, 4);
    let gates: Vec<Gate> = (0..=width).map(|n| Gate::new(&format!("w{n}"))).collect();
    let ids: Vec<JobId> =
        (0..=width).map(|n| rig.exec.start(job(&format!("w{n}"), &json!("block"))).unwrap().id).collect();
    assert!(ids[..width].iter().all(|id| live(&rig, *id).state == JobState::Running));
    assert_eq!(live(&rig, ids[width]).state, JobState::Queued, "one past the lane width queues");
    assert_eq!(live(&rig, ids[0]).lane, Lane::Cpu);

    gates[0].release.send(()).unwrap();
    rig.exec.wait(ids[0], None).unwrap();
    assert_eq!(live(&rig, ids[width]).state, JobState::Running, "the freed slot starts the queued job");
    for gate in &gates {
        let _ = gate.release.send(());
    }
}

#[test]
fn the_257th_queued_job_is_limited() {
    let rig = rig(json!({}));
    let gate = Gate::new("q");
    let gpu = json!({"gpu": true});
    let first = rig.exec.start(job_with("q", &json!("block"), gpu.clone())).unwrap();
    gate.wait_entered();
    assert_eq!(first.lane, Lane::Gpu);
    for _ in 0..256 {
        assert_eq!(rig.exec.start(job_with("q", &json!("block"), gpu.clone())).unwrap().state, JobState::Queued);
    }
    let error = rig.exec.start(job_with("q", &json!("block"), gpu)).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Limited);
    // The full GPU queue does not hold up a job the idle CPU lane takes at once.
    let cpu = rig.exec.start(job("q-cpu", &json!("echo"))).unwrap();
    assert!(matches!(rig.exec.wait(cpu.id, None).unwrap().state, JobState::Done { .. }));
    drop(gate);
}

#[test]
fn a_job_past_its_time_cap_is_cancelled() {
    let rig = rig(json!({}));
    let gate = Gate::new("t1");
    // An estimate of 1 s gives the 60 s minimum.
    let info = rig.exec.start(job_with("t1", &json!("coop"), json!({"seconds": 1.0}))).unwrap();
    gate.wait_entered();
    advance(&rig, 59.0);
    assert_eq!(live(&rig, info.id).state, JobState::Running);
    advance(&rig, 2.0);
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Timeout });
    assert_eq!(rig.at("t1"), None);
}

#[test]
fn a_job_without_an_estimate_gets_thirty_minutes() {
    let rig = rig(json!({}));
    let gate = Gate::new("t2");
    let info = rig.exec.start(job("t2", &json!("coop"))).unwrap();
    gate.wait_entered();
    advance(&rig, 1_799.0);
    assert_eq!(live(&rig, info.id).state, JobState::Running);
    advance(&rig, 2.0);
    assert!(matches!(rig.exec.wait(info.id, None).unwrap().state, JobState::Cancelled { .. }));
}

#[test]
fn run_returns_the_cancelled_error_of_a_cancelled_job() {
    let rig = rig(json!({}));
    let gate = Gate::new("c1");
    let error = std::thread::scope(|scope| {
        let running = scope.spawn(|| rig.exec.run(job("c1", &json!("coop"))));
        gate.wait_entered();
        let id = rig.exec.jobs(&Grants::person()).unwrap()[0].id;
        let info = rig.exec.cancel(id, &person()).unwrap();
        assert!(matches!(info.state, JobState::Running), "cancel is cooperative: the job still runs");
        running.join().unwrap().unwrap_err()
    });
    assert_eq!(code(&error), ErrorCode::Cancelled);
    assert!(matches!(error.detail.as_deref(), Some(ErrorDetail::Cancelled { reason: CancelReason::Requested, .. })));
    let ended = &rig.exec.jobs(&Grants::person()).unwrap()[0];
    assert_eq!(ended.state, JobState::Cancelled { by: Some(person()), reason: CancelReason::Requested });
}

#[test]
fn a_cancelled_job_may_return_what_it_completed_and_its_finish_applies() {
    let rig = rig(json!({}));
    let gate = Gate::new("p1");
    let info = rig.exec.start(job("p1", &json!("partial"))).unwrap();
    gate.wait_entered();
    rig.exec.cancel(info.id, &person()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    assert_eq!(rig.at("p1"), Some(json!("partial")));
}

#[test]
fn only_the_jobs_actor_or_a_person_may_cancel_it() {
    let rig = rig(json!({}));
    let gate = Gate::new("o1");
    let info = rig
        .exec
        .start(Request { actor: agent(), ..agent_request("test.job", json!({"key": "o1", "how": "coop"})) })
        .unwrap();
    gate.wait_entered();
    let other = omuzaki_ops_api::Actor::Plugin { id: "x".into() };
    assert_eq!(code(&rig.exec.cancel(info.id, &other).unwrap_err()), ErrorCode::Denied);
    assert_eq!(code(&rig.exec.cancel(JobId(999), &person()).unwrap_err()), ErrorCode::NotFound);
    rig.exec.cancel(info.id, &agent()).unwrap();
    rig.exec.cancel(info.id, &agent()).unwrap();
    rig.exec.wait(info.id, None).unwrap();
}

#[test]
fn a_job_that_ignores_cancel_is_ended_after_ten_seconds_and_the_third_detached_thread_closes_the_lane() {
    let rig = rig(json!({}));
    let gpu = json!({"gpu": true});
    let gates: Vec<Gate> = (0..4).map(|n| Gate::new(&format!("x{n}"))).collect();
    for (n, gate) in gates.iter().enumerate().take(3) {
        let info = rig.exec.start(job_with(&format!("x{n}"), &json!("block"), gpu.clone())).unwrap();
        gate.wait_entered();
        rig.exec.cancel(info.id, &person()).unwrap();
        advance(&rig, 9.9);
        assert_eq!(live(&rig, info.id).state, JobState::Running, "inside the grace period");
        advance(&rig, 0.2);
        let ended = live(&rig, info.id);
        assert_eq!(ended.state, JobState::Cancelled { by: Some(person()), reason: CancelReason::Requested });
    }
    let refused = rig.exec.start(job_with("x3", &json!("block"), gpu.clone())).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Unavailable);

    gates[0].release.send(()).unwrap();
    rig.exec.wait_detached(2);
    let accepted = rig.exec.start(job_with("x3", &json!("block"), gpu)).unwrap();
    assert_eq!(accepted.state, JobState::Queued, "the third thread still holds the slot");
    assert_eq!(rig.at("x0"), None, "a detached thread's late result is discarded");
}

#[test]
fn spending_past_the_estimate_fails_and_cancels_the_job() {
    let rig = rig(json!({}));
    let error =
        rig.exec.run(job_with("s1", &json!({"overspend": {"each": 300_000}}), json!({"usd": 0.5}))).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Limited);
    let ended = &rig.exec.jobs(&Grants::person()).unwrap()[0];
    assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Budget });
    assert_eq!(ended.spent_usd_micros, 300_000);
    assert_eq!(rig.at("s1"), None);
}

#[test]
fn progress_events_are_coalesced_to_ten_a_second() {
    let rig = rig(json!({}));
    let events = rig.subscribe();
    let gate = Gate::new("g1");
    let info = rig.exec.start(job("g1", &json!("progress"))).unwrap();
    gate.wait_entered();
    advance(&rig, 0.1);
    gate.release.send(()).unwrap();
    rig.exec.wait(info.id, None).unwrap();
    let stages: Vec<String> = drain(&events)
        .into_iter()
        .filter_map(|e| match e {
            Event::JobProgress { stage, .. } => Some(stage),
            _ => None,
        })
        .collect();
    assert_eq!(stages, ["step", "last"], "100 reports in one instant make one event");
}

#[test]
fn job_changes_arrive_in_order_around_the_edit_they_apply() {
    let rig = rig(json!({}));
    let events = rig.subscribe();
    rig.exec.run(job("e1", &json!("echo"))).unwrap();
    let seen: Vec<String> = drain(&events)
        .into_iter()
        .map(|e| match e {
            Event::JobChanged { job, .. } => match job.state {
                JobState::Queued => "queued".to_owned(),
                JobState::Running => "running".to_owned(),
                JobState::Done { .. } => "done".to_owned(),
                other => format!("{other:?}"),
            },
            Event::OpApplied { .. } => "applied".to_owned(),
            Event::TxCommitted { .. } => "committed".to_owned(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(seen, ["queued", "running", "applied", "committed", "done"]);
}

#[test]
fn a_reservation_of_a_detached_thread_stays_held_and_its_late_spend_is_charged() {
    let rig = budgeted(1_000_000);
    let gate = Gate::new("m1");
    let paid = json!({"usd": 0.6});
    let info =
        rig.exec.start(job_with("m1", &json!({"spend": {"before": 100_000, "after": 400_000}}), paid.clone())).unwrap();
    gate.wait_entered();
    rig.exec.cancel(info.id, &person()).unwrap();
    advance(&rig, 10.0);
    assert!(matches!(live(&rig, info.id).state, JobState::Cancelled { .. }));

    // The job is over, the thread is not: its money is still counted, so this would fit only in the released money.
    let refused = rig.exec.start(job_with("m2", &json!("echo"), json!({"usd": 0.45}))).unwrap_err();
    assert_eq!(code(&refused), ErrorCode::Limited);

    gate.release.send(()).unwrap();
    rig.exec.wait_detached(0);
    // 100,000 + the late 400,000 are charged: 0.6 more does not fit, 0.5 does.
    assert_eq!(code(&rig.exec.start(job_with("m3", &json!("echo"), paid)).unwrap_err()), ErrorCode::Limited);
    rig.exec.run(job_with("m4", &json!("echo"), json!({"usd": 0.5}))).unwrap();
}

#[test]
fn a_reservation_still_held_at_shutdown_is_charged_in_full() {
    let rig = budgeted(10_000_000);
    let gate = Gate::new("h1");
    let info = rig
        .exec
        .start(job_with("h1", &json!({"spend": {"before": 100_000, "after": 0}}), json!({"usd": 0.5})))
        .unwrap();
    gate.wait_entered();
    assert_eq!(rig.exec.shutdown_jobs(), 500_000);
    assert_eq!(rig.exec.shutdown_jobs(), 500_000, "a second shutdown charges nothing more");
    assert_eq!(code(&rig.exec.start(job("h2", &json!("echo"))).unwrap_err()), ErrorCode::Unavailable);
    drop(gate);
    let _ = rig.exec.wait(info.id, None);
}

#[test]
fn the_reservation_is_released_on_every_exit_path() {
    let rig = budgeted(1_000_000);
    let half = json!({"usd": 0.5});
    // A finished job, a failed one and a queued one that is cancelled each leave the money free.
    rig.exec.run(job_with("z1", &json!("echo"), half.clone())).unwrap();
    assert_eq!(code(&rig.exec.run(job_with("z2", &json!("fail"), half.clone())).unwrap_err()), ErrorCode::Invalid);
    rig.exec.run(job_with("z3", &json!("echo"), json!({"usd": 1.0}))).unwrap();

    let gate = Gate::new("z4");
    let gpu_half = json!({"usd": 0.4, "gpu": true});
    let running = rig.exec.start(job_with("z4", &json!("block"), gpu_half.clone())).unwrap();
    gate.wait_entered();
    let queued = rig.exec.start(job_with("z5", &json!("echo"), gpu_half)).unwrap();
    assert_eq!(queued.state, JobState::Queued);
    rig.exec.cancel(queued.id, &person()).unwrap();
    assert!(matches!(live(&rig, queued.id).state, JobState::Cancelled { reason: CancelReason::Requested, .. }));
    // 0.4 is still held by the running job; the cancelled queued job gave back its 0.4.
    let rest = rig.exec.start(job_with("z6", &json!("echo"), json!({"usd": 0.6}))).unwrap();
    rig.exec.wait(rest.id, None).unwrap();
    gate.release.send(()).unwrap();
    rig.exec.wait(running.id, None).unwrap();
}

#[test]
fn a_panic_in_work_fails_the_job_and_charges_its_whole_reservation() {
    let rig = budgeted(10_000_000);
    let error = rig.exec.run(job_with("k1", &json!("panic"), json!({"usd": 0.5, "gpu": true}))).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Internal);
    assert!(matches!(rig.exec.jobs(&Grants::person()).unwrap()[0].state, JobState::Failed { .. }));
    // The lane is free again, and the unknown spend of the panicked call stays charged.
    rig.exec.run(job_with("k2", &json!("echo"), json!({"gpu": true}))).unwrap();
    assert_eq!(rig.exec.shutdown_jobs(), 500_000);
}

#[test]
fn the_context_shows_the_plan_and_a_cloned_cancel_flag_sees_a_later_cancel() {
    let rig = rig(json!({}));
    let info = rig.exec.start(job_with("f1", &json!("echo"), json!({"usd": 0.25}))).unwrap();
    rig.exec.wait(info.id, None).unwrap();
    assert_eq!(rig.at("f1"), Some(json!({"output": {"chose": "f1"}, "reserved": 250_000})));

    let flag = FLAGS.lock().unwrap().get("f1").cloned().unwrap();
    assert!(!omuzaki_ops_api::Cancel::is_cancelled(&flag));
    rig.exec.cancel(info.id, &person()).unwrap();
    assert!(omuzaki_ops_api::Cancel::is_cancelled(&flag), "work has returned, and the flag is still the job's");
}

#[test]
fn a_job_needs_read_to_be_listed_and_its_op_scope_to_start() {
    let rig = rig(json!({}));
    let none = Grants::of([]);
    assert_eq!(code(&rig.exec.jobs(&none).unwrap_err()), ErrorCode::Denied);
    assert_eq!(code(&rig.exec.job(JobId(1), &none).unwrap_err()), ErrorCode::Denied);
    let mut read_only = job("n1", &json!("echo"));
    read_only.grants = Grants::of([Scope::Read]);
    assert_eq!(code(&rig.exec.start(read_only).unwrap_err()), ErrorCode::Denied);
    assert_eq!(rig.exec.jobs(&Grants::person()).unwrap().len(), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn a_job_thread_runs_below_normal_priority() {
    let rig = rig(json!({}));
    rig.exec.run(job("n1", &json!("nice"))).unwrap();
    let here = thread_nice().unwrap();
    let worker = rig.at("n1").and_then(|nice| nice.as_i64()).unwrap();
    assert!(worker >= here.max(10), "the job ran at nice {worker}, the test at {here}");
}

#[test]
fn shutdown_still_ends_a_job_that_ignores_cancel_at_its_deadline() {
    let rig = rig(json!({}));
    let events = rig.exec.subscribe(&Grants::person()).unwrap();
    let gate = Gate::new("s1");
    let info = rig.exec.start(job("s1", &json!("block"))).unwrap();
    gate.wait_entered();
    rig.exec.shutdown_jobs();
    // Only the supervisor sees the clock move: the test does not call `poll_jobs`.
    rig.clock.advance(Seconds(10.1));
    let ended = ended_event(&events, info.id, 30).expect("the job was never ended after shutdown");
    assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Shutdown });
    assert_eq!(rig.exec.wait(info.id, None).unwrap().state, ended.state);
    drop(gate);
    rig.exec.wait_detached(0);
}

#[test]
fn run_gets_its_cancelled_outcome_however_many_queued_jobs_shutdown_ends() {
    let rig = rig(json!({}));
    let gpu = json!({"gpu": true});
    let gate = Gate::new("e");
    rig.exec.start(job_with("e", &json!("block"), gpu.clone())).unwrap();
    gate.wait_entered();
    let error = std::thread::scope(|scope| {
        let running = scope.spawn(|| rig.exec.run(job_with("e", &json!("block"), gpu.clone())));
        while rig.exec.jobs(&Grants::person()).unwrap().len() < 2 {
            std::thread::yield_now();
        }
        for _ in 0..150 {
            rig.exec.start(job_with("e", &json!("block"), gpu.clone())).unwrap();
        }
        // The waiting run's job ends first of 151 queued jobs, past the 100 the history keeps.
        rig.exec.shutdown_jobs();
        running.join().unwrap().unwrap_err()
    });
    assert_eq!(code(&error), ErrorCode::Cancelled, "{error}");
    assert!(matches!(error.detail.as_deref(), Some(ErrorDetail::Cancelled { reason: CancelReason::Shutdown, .. })));
    assert!(rig.exec.jobs(&Grants::person()).unwrap().len() <= 101, "the history is trimmed once it is read");
    drop(gate);
}

#[test]
fn a_timed_wait_runs_on_the_executor_clock() {
    let rig = rig(json!({}));
    let gate = Gate::new("tw");
    let info = rig.exec.start(job("tw", &json!("block"))).unwrap();
    gate.wait_entered();
    let (done, waited) = channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = done.send(rig.exec.wait(info.id, Some(Duration::from_millis(50))));
        });
        // Six times the timeout in wall time, while the fake clock stands still.
        assert!(waited.recv_timeout(Duration::from_millis(300)).is_err(), "wall time expired the wait");
        advance(&rig, 0.06);
        let woke = waited.recv_timeout(Duration::from_secs(30)).expect("moving the clock did not expire the wait");
        assert_eq!(woke.unwrap().state, JobState::Running);
    });
    gate.release.send(()).unwrap();
    rig.exec.wait(info.id, None).unwrap();
}

#[test]
fn signals_from_inside_a_document_read_do_not_deadlock() {
    let rig = rig(json!({}));
    let info = rig.exec.start(job("rs", &json!("read_signals"))).unwrap();
    let (done, ended) = channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = done.send(rig.exec.wait(info.id, None));
        });
        let ended = ended.recv_timeout(Duration::from_secs(30)).expect("the job deadlocked inside read").unwrap();
        assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    });
    assert_eq!(rig.at("rs"), Some(json!({"refused": true})));
    // The progress reported under the lock went out when the read ended; the money report was refused.
    let after = live(&rig, info.id);
    assert_eq!(after.stage.as_deref(), Some("reading"));
    assert_eq!(after.spent_usd_micros, 0);
}

#[test]
fn a_result_returned_after_shutdown_began_is_discarded() {
    let rig = rig(json!({}));
    let gate = Gate::new("sd");
    let info = rig.exec.start(job("sd", &json!("partial"))).unwrap();
    gate.wait_entered();
    rig.exec.shutdown_jobs();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Shutdown });
    assert_eq!(rig.at("sd"), None, "the document changed after shutdown began");
}

#[test]
fn shutdown_cancels_a_job_whose_work_then_fails_or_panics() {
    for (key, how) in [("sx-fail", "fail_when_cancelled"), ("sx-panic", "panic_when_cancelled")] {
        let rig = rig(json!({}));
        let gate = Gate::new(key);
        let error = std::thread::scope(|scope| {
            let running = scope.spawn(|| rig.exec.run(job(key, &json!(how))));
            gate.wait_entered();
            rig.exec.shutdown_jobs();
            running.join().unwrap().unwrap_err()
        });
        // Within the grace period `work` failed or panicked: shutdown still wins, and the run says so.
        assert_eq!(code(&error), ErrorCode::Cancelled, "{how}: {error:?}");
        assert!(
            matches!(error.detail.as_deref(), Some(ErrorDetail::Cancelled { reason: CancelReason::Shutdown, .. })),
            "{how}: {error:?}"
        );
        let ended = &rig.exec.jobs(&Grants::person()).unwrap()[0];
        assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Shutdown }, "{how}");
    }
}

#[test]
fn shutdown_overrides_an_earlier_cancel_reason() {
    for (key, earlier) in [("so-req", CancelReason::Requested), ("so-time", CancelReason::Timeout)] {
        let rig = rig(json!({}));
        let gate = Gate::new(key);
        let error = std::thread::scope(|scope| {
            let running =
                scope.spawn(|| rig.exec.run(job_with(key, &json!("fail_after_the_gate"), json!({"seconds": 1.0}))));
            gate.wait_entered();
            let id = rig.exec.jobs(&Grants::person()).unwrap()[0].id;
            if earlier == CancelReason::Requested {
                rig.exec.cancel(id, &person()).unwrap();
            } else {
                // An estimate of 1 s gives the 60 s cap.
                advance(&rig, 61.0);
            }
            // `work` saw the earlier cancel and waits; shutdown comes while it is still running.
            gate.wait_entered();
            rig.exec.shutdown_jobs();
            drop(gate);
            running.join().unwrap().unwrap_err()
        });
        assert_eq!(code(&error), ErrorCode::Cancelled, "{earlier:?}: {error:?}");
        assert!(
            matches!(error.detail.as_deref(), Some(ErrorDetail::Cancelled { reason: CancelReason::Shutdown, .. })),
            "{earlier:?}: {error:?}"
        );
        let ended = &rig.exec.jobs(&Grants::person()).unwrap()[0];
        assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Shutdown }, "{earlier:?}");
    }
}

#[test]
fn a_builder_called_after_shutdown_keeps_the_job_manager_closed() {
    let Rig { exec, clock: _clock } = rig(json!({}));
    exec.shutdown_jobs();
    let exec = exec.with_job_budget(1_000_000);
    let error = exec.start(job("sb", &json!("echo"))).unwrap_err();
    assert_eq!(code(&error), ErrorCode::Unavailable, "a builder reopened a shut-down manager: {error:?}");
}

#[test]
fn a_builder_called_after_a_job_started_keeps_the_job_manager() {
    let Rig { exec, clock: _clock } = rig(json!({}));
    let gate = Gate::new("rb");
    let info = exec.start(job("rb", &json!("coop"))).unwrap();
    gate.wait_entered();
    // A budget of one micro-dollar would refuse the job below: the call must change nothing.
    let exec = exec.with_job_budget(1);
    assert_eq!(exec.jobs(&Grants::person()).unwrap()[0].id, info.id, "the running job is no longer listed");
    let cheap = exec.start(job_with("rb2", &json!("echo"), json!({"usd": 0.5}))).unwrap();
    assert_eq!(cheap.id, JobId(info.id.0 + 1), "the job ids restarted");
    assert!(matches!(exec.wait(cheap.id, None).unwrap().state, JobState::Done { .. }));
    exec.cancel(info.id, &person()).unwrap();
    let ended = exec.wait(info.id, None).unwrap();
    assert_eq!(ended.state, JobState::Cancelled { by: Some(person()), reason: CancelReason::Requested });
}

#[test]
fn a_job_start_is_recorded_before_its_work_runs_and_a_refused_record_stops_it() {
    let seen: Arc<Mutex<Vec<(String, bool)>>> = Arc::default();
    let log = Arc::clone(&seen);
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec =
        Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_job_journal(move |_, args| {
            let key = args["key"].as_str().unwrap_or_default().to_owned();
            // `Echo` stores its cancel flag when `work` runs: it must not have yet.
            let ran = FLAGS.lock().unwrap().contains_key(&key);
            log.lock().unwrap().push((key.clone(), ran));
            if key == "refused" { Err(OpError::unavailable("the journal is full")) } else { Ok(()) }
        });
    FLAGS.lock().unwrap().remove("recorded");
    FLAGS.lock().unwrap().remove("refused");
    let ok = exec.start(job("recorded", &json!("echo"))).unwrap();
    assert!(matches!(exec.wait(ok.id, None).unwrap().state, JobState::Done { .. }));
    let no = exec.start(job("refused", &json!("echo"))).unwrap();
    let failed = exec.wait(no.id, None).unwrap();
    assert!(matches!(failed.state, JobState::Failed { .. }), "{:?}", failed.state);
    assert_eq!(*seen.lock().unwrap(), vec![("recorded".to_owned(), false), ("refused".to_owned(), false)]);
    assert!(FLAGS.lock().unwrap().contains_key("recorded"));
    assert!(!FLAGS.lock().unwrap().contains_key("refused"), "work ran although the start was refused");
}

#[test]
fn a_shutdown_during_a_slow_finish_leaves_the_document_unchanged() {
    let rig = rig(json!({}));
    let gate = Gate::new("sf");
    let info = rig.exec.start(job("sf", &json!("slow_finish"))).unwrap();
    // `finish` is running with the document lock held; it returns its plan only once shutdown has cancelled the job.
    gate.wait_entered();
    rig.exec.shutdown_jobs();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert_eq!(ended.state, JobState::Cancelled { by: None, reason: CancelReason::Shutdown });
    assert_eq!(rig.at("sf"), None, "the finish applied after shutdown began");
}

/// An executor over a GPU lane one wide whose start journal logs each key, refuses `refused` and panics on `panics`.
fn journalled(log: Arc<Mutex<Vec<String>>>) -> Executor {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_job_budget(1_000_000);
    open_agents(&exec);
    exec.with_job_journal(move |_, args| {
        let key = args["key"].as_str().unwrap_or_default().to_owned();
        assert_ne!(key, "panics", "the journal panicked");
        log.lock().unwrap().push(key);
        Ok(())
    })
}

#[test]
fn a_queued_start_is_recorded_before_it_is_acknowledged() {
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let exec = journalled(Arc::clone(&seen));
    let gate = Gate::new("jq1");
    let gpu = json!({"gpu": true});
    let first = exec.start(job_with("jq1", &json!("block"), gpu.clone())).unwrap();
    gate.wait_entered();
    let queued = exec.start(job_with("jq2", &json!("echo"), gpu)).unwrap();
    assert_eq!(queued.state, JobState::Queued);
    assert_eq!(*seen.lock().unwrap(), ["jq1", "jq2"], "the queued start was acknowledged before it was recorded");
    gate.release.send(()).unwrap();
    exec.wait(first.id, None).unwrap();
    assert!(matches!(exec.wait(queued.id, None).unwrap().state, JobState::Done { .. }));
}

#[test]
fn a_panicking_journal_fails_the_start_and_gives_back_its_place_and_money() {
    let exec = journalled(Arc::default());
    let whole = json!({"gpu": true, "usd": 1.0});
    let failed = exec.start(job_with("panics", &json!("echo"), whole.clone())).unwrap();
    assert!(matches!(failed.state, JobState::Failed { .. }), "{:?}", failed.state);
    // The whole budget and the one GPU slot are free again.
    let next = exec.start(job_with("jp-next", &json!("echo"), whole)).unwrap();
    assert_eq!(next.state, JobState::Running);
    assert!(matches!(exec.wait(next.id, None).unwrap().state, JobState::Done { .. }));
}

// ---- sessions and jobs ---------------------------------------------------------------------------------------------

fn agent_job(key: &str, how: &Value) -> Request {
    Request { actor: agent(), ..agent_request("test.job", json!({"key": key, "how": how})) }
}

#[test]
fn a_job_carries_its_session_and_principal() {
    let rig = rig(json!({}));
    let by_agent = rig.exec.start(agent_job("j1", &json!("echo"))).unwrap();
    assert_eq!((by_agent.session, by_agent.on_behalf_of), (Some(SessionId(1)), Some(person())));
    let by_person = rig.exec.start(job("j2", &json!("echo"))).unwrap();
    assert_eq!((by_person.session, by_person.on_behalf_of), (None, None));
    rig.exec.wait(by_agent.id, None).unwrap();
    rig.exec.wait(by_person.id, None).unwrap();
}

#[test]
fn a_finish_whose_session_is_paused_is_held_until_it_resumes() {
    let rig = rig(json!({}));
    let gate = Gate::new("p1");
    let events = rig.subscribe();
    let info = rig.exec.start(agent_job("p1", &json!("block"))).unwrap();
    gate.wait_entered();
    rig.exec.pause(SessionId(1), &person()).unwrap();
    gate.release.send(()).unwrap();

    let held = ended_event(&events, info.id, 30).unwrap();
    assert_eq!(held.state, JobState::Held, "a paused session changes nothing (design 4.3)");
    assert_eq!(rig.at("p1"), None);
    assert_eq!(rig.log().len(), 0);

    rig.exec.resume(SessionId(1), &person()).unwrap();
    let ended = rig.exec.wait(info.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    assert_eq!(rig.at("p1"), Some(json!("done")), "applied once the session resumed");
}

#[test]
fn a_queued_job_waits_while_its_session_is_paused() {
    let rig = rig(json!({}));
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let width = (cores / 2).clamp(1, 4);
    let gates: Vec<Gate> = (0..width).map(|n| Gate::new(&format!("q{n}"))).collect();
    for n in 0..width {
        rig.exec.start(job(&format!("q{n}"), &json!("block"))).unwrap();
    }
    for gate in &gates {
        gate.wait_entered();
    }
    let waiting = rig.exec.start(agent_job("qa", &json!("echo"))).unwrap();
    assert_eq!(live(&rig, waiting.id).state, JobState::Queued);

    rig.exec.pause(SessionId(1), &person()).unwrap();
    for gate in &gates {
        gate.release.send(()).unwrap();
    }
    let others: Vec<JobId> =
        rig.exec.jobs(&Grants::person()).unwrap().iter().map(|j| j.id).filter(|id| *id != waiting.id).collect();
    for id in others {
        rig.exec.wait(id, None).unwrap();
    }
    rig.exec.poll_jobs();
    assert_eq!(live(&rig, waiting.id).state, JobState::Queued, "a paused session's queued job does not start");
    assert_eq!(rig.at("qa"), None, "its work never ran");

    rig.exec.resume(SessionId(1), &person()).unwrap();
    rig.exec.poll_jobs();
    let ended = rig.exec.wait(waiting.id, None).unwrap();
    assert!(matches!(ended.state, JobState::Done { .. }), "it starts once resumed: {:?}", ended.state);
}

#[test]
fn a_queued_job_works_with_the_grants_its_session_holds_when_it_starts() {
    // A file root outside the project folder, which agents may always open.
    let (project, dir) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(root.join("a.txt"), "a").unwrap();
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock.clone())
        .unwrap()
        .with_project_dir(ProjectDir::open(project.path()).unwrap());
    open_agents(&exec);
    let rig = Rig { exec, clock };
    let cores = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
    let width = (cores / 2).clamp(1, 4);
    let gates: Vec<Gate> = (0..width).map(|n| Gate::new(&format!("g{n}"))).collect();
    for n in 0..width {
        rig.exec.start(job(&format!("g{n}"), &json!("block"))).unwrap();
    }
    for gate in &gates {
        gate.wait_entered();
    }
    let wide = Grants::agent_default().with_fs_root(root.to_str().unwrap());
    let open = json!({"open": {"path": root.join("a.txt").to_str().unwrap()}});
    let waiting = rig.exec.start(Request { grants: wide, ..agent_job("ga", &open) }).unwrap();
    assert_eq!(live(&rig, waiting.id).state, JobState::Queued);

    // The person narrows the session to the default agent ceiling (no file roots) while the job waits.
    rig.exec.set_grants(SessionId(1), Grants::agent_default(), &person()).unwrap();
    for gate in &gates {
        gate.release.send(()).unwrap();
    }
    let ended = rig.exec.wait(waiting.id, None).unwrap();
    assert!(
        matches!(&ended.state, JobState::Failed { error } if error.code == ErrorCode::Denied),
        "its work read a file the session no longer holds: {:?}",
        ended.state
    );
}

// ---- #1593, #1602, #1603 ----------------------------------------------------------------------------------------

#[test]
fn a_read_inside_a_read_is_served_from_the_held_document() {
    let rig = rig(json!({"seen": 7}));
    let events = rig.subscribe();
    let info = rig.exec.start(job("nr", &json!("nested_read"))).unwrap();
    let (done, ended) = channel();
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let _ = done.send(rig.exec.wait(info.id, None));
        });
        let ended = ended.recv_timeout(Duration::from_secs(30)).expect("a nested read hung").unwrap();
        assert!(matches!(ended.state, JobState::Done { .. }), "{:?}", ended.state);
    });
    assert_eq!(rig.at("nr"), Some(json!({"same": true})), "both closures saw the same document");
    let progress = drain(&events)
        .into_iter()
        .filter(|e| matches!(e, Event::JobProgress { job, stage, .. } if *job == info.id && stage == "inner"))
        .count();
    assert_eq!(progress, 1, "the inner read's progress went out once, after the outer read");
}

/// A rig whose job ends are recorded by a hook, as `(id, state)`.
fn end_recorded(ends: Arc<Mutex<Vec<(JobId, JobState)>>>) -> Rig {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock.clone()).unwrap().with_job_end_journal(
        move |info| {
            ends.lock().unwrap().push((info.id, info.state.clone()));
            Ok(())
        },
    );
    open_agents(&exec);
    Rig { exec, clock }
}

#[test]
fn every_end_is_journalled_once_with_the_state_the_snapshot_shows() {
    let ends = Arc::new(Mutex::new(Vec::new()));
    let rig = end_recorded(ends.clone());
    let done = rig.exec.start(job("e1", &json!("echo"))).unwrap();
    let failed = rig.exec.start(job("e2", &json!("fail"))).unwrap();
    for id in [done.id, failed.id] {
        rig.exec.wait(id, None).unwrap();
    }
    let gate = Gate::new("e3");
    let running = rig.exec.start(job("e3", &json!("coop"))).unwrap();
    gate.wait_entered();
    rig.exec.cancel(running.id, &person()).unwrap();
    rig.exec.wait(running.id, None).unwrap();
    let stuck = Gate::new("e4");
    let detached = rig.exec.start(job("e4", &json!("block"))).unwrap();
    stuck.wait_entered();
    rig.exec.cancel(detached.id, &person()).unwrap();
    advance(&rig, 11.0);
    let detached_end = rig.exec.wait(detached.id, None).unwrap();
    let snapshots: Vec<(JobId, JobState)> = [done.id, failed.id, running.id]
        .into_iter()
        .map(|id| (id, live(&rig, id).state))
        .chain([(detached.id, detached_end.state)])
        .collect();
    let recorded = ends.lock().unwrap().clone();
    for snapshot in &snapshots {
        assert_eq!(recorded.iter().filter(|r| r.0 == snapshot.0).count(), 1, "{snapshot:?} once");
        assert!(recorded.contains(snapshot), "{snapshot:?} in {recorded:?}");
    }
    drop(stuck);
    rig.exec.wait_detached(0);
}

#[test]
fn a_cancel_while_queued_and_shutdown_are_journalled_too() {
    let ends = Arc::new(Mutex::new(Vec::new()));
    let rig = end_recorded(ends.clone());
    let gpu = json!({"gpu": true});
    let gate = Gate::new("s1");
    // Cooperative, so shutdown ends it at its next checkpoint.
    let running = rig.exec.start(job_with("s1", &json!("coop"), gpu.clone())).unwrap();
    gate.wait_entered();
    let queued = rig.exec.start(job_with("s2", &json!("echo"), gpu.clone())).unwrap();
    let last = rig.exec.start(job_with("s3", &json!("echo"), gpu)).unwrap();
    rig.exec.cancel(queued.id, &person()).unwrap();
    rig.exec.shutdown_jobs();
    rig.exec.wait(running.id, None).unwrap();
    drop(gate);
    let recorded = ends.lock().unwrap().clone();
    let reason = |id| recorded.iter().find(|(r, _)| *r == id).map(|(_, state)| state.clone());
    assert!(matches!(reason(queued.id), Some(JobState::Cancelled { reason: CancelReason::Requested, .. })));
    for id in [running.id, last.id] {
        assert!(matches!(reason(id), Some(JobState::Cancelled { reason: CancelReason::Shutdown, .. })), "{recorded:?}");
    }
}

#[test]
fn a_finish_is_journalled_right_after_its_record_in_the_same_hold() {
    let sink = Arc::new(omuzaki_history_mock::RecordingSink::new());
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_journal(sink.clone());
    let info = exec.start(job("f1", &json!("echo"))).unwrap();
    let ended = exec.wait(info.id, None).unwrap();
    let JobState::Done { tx, seq: Some(seq) } = ended.state else { panic!("{:?}", ended.state) };
    let entries = sink.entries();
    let op = entries.iter().position(
        |e| matches!(e, omuzaki_history_api::HistoryEntry::Op { record, .. } if record.seq == seq && record.tx == tx),
    );
    let end = entries
        .iter()
        .position(|e| matches!(e, omuzaki_history_api::HistoryEntry::JobEnded { job, .. } if *job == info.id));
    let started = entries
        .iter()
        .position(|e| matches!(e, omuzaki_history_api::HistoryEntry::JobStarted { job, .. } if *job == info.id));
    assert!(started.is_some() && op.is_some() && end == op.map(|at| at + 1), "{entries:?}");
}

#[test]
fn a_panicking_end_journal_strands_nothing() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock)
        .unwrap()
        .with_job_end_journal(|_| panic!("the end journal broke"));
    for how in ["echo", "fail"] {
        let info = exec.start(job(how, &json!(how))).unwrap();
        let ended = exec.wait(info.id, None).unwrap();
        assert!(!matches!(ended.state, JobState::Queued | JobState::Running), "{:?}", ended.state);
    }
}

#[test]
fn job_ids_continue_after_the_journal_and_never_restart() {
    let clock = Arc::new(FakeClock::new(Seconds(0.0)));
    let exec = Executor::new(Box::new(MemDoc::from_value(json!({}))), clock).unwrap().with_last_job_id(41);
    let first = exec.start(job("i1", &json!("echo"))).unwrap();
    assert_eq!(first.id, JobId(42));
    let exec = exec.with_last_job_id(7);
    assert_eq!(exec.start(job("i2", &json!("echo"))).unwrap().id, JobId(43), "seeding after a job has no effect");
}

#[test]
fn the_listing_keeps_the_last_hundred_ended_jobs_while_waiters_pin_more() {
    let rig = rig(json!({}));
    let gpu = json!({"gpu": true});
    let gate = Gate::new("w");
    rig.exec.start(job_with("w", &json!("block"), gpu.clone())).unwrap();
    gate.wait_entered();
    let outcomes = std::thread::scope(|scope| {
        let callers: Vec<_> =
            (0..150).map(|_| scope.spawn(|| rig.exec.run(job_with("w", &json!("block"), gpu.clone())))).collect();
        while rig.exec.jobs(&Grants::person()).unwrap().iter().filter(|j| j.state == JobState::Queued).count() < 150 {
            std::thread::yield_now();
        }
        rig.exec.shutdown_jobs();
        let ended = rig
            .exec
            .jobs(&Grants::person())
            .unwrap()
            .iter()
            .filter(|j| !matches!(j.state, JobState::Queued | JobState::Running))
            .count();
        assert!(ended <= 100, "{ended} ended jobs listed");
        callers.into_iter().map(|c| c.join().unwrap()).collect::<Vec<_>>()
    });
    for outcome in outcomes {
        let error = outcome.unwrap_err();
        assert!(matches!(error.detail.as_deref(), Some(ErrorDetail::Cancelled { reason: CancelReason::Shutdown, .. })));
    }
    assert!(rig.exec.jobs(&Grants::person()).unwrap().len() <= 101, "trimmed back once the waiters have read");
    drop(gate);
}
