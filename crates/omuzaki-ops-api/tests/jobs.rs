//! The jobs contract (ADR 0036, design `ops-api.md` §8): every new type survives a serde round trip, old JSON still
//! decodes and re-encodes byte for byte, the job hooks of `Op` have the documented defaults, and `JobCtx` forwards to
//! its collaborators.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fmt::Debug;
use std::sync::Mutex;

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, Cancel, CancelFlag, CancelReason, Cause, CauseKind, CostHint, Effect, Ended, ErrorCode, ErrorDetail, Event,
    Grants, JobCalls, JobCtx, JobDoc, JobId, JobInfo, JobShape, JobSignal, JobState, Lane, Mode, NeverCancel, Op,
    OpCall, OpCtx, OpError, OpInfo, OpKind, OpMeta, OpRecord, OpRegistration, Outcome, Plan, Request, RevertConflict,
    Scope, TxId, Undo,
};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

fn person() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude-code#1".into(), model: None }
}

/// `value` encodes to JSON that decodes to an equal value, and its type has a JSON Schema.
fn round_trips<T: Serialize + DeserializeOwned + JsonSchema + PartialEq + Debug>(value: &T) {
    let json = serde_json::to_value(value).unwrap();
    let back: T = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(&back, value, "{json}");
    let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap();
    assert!(schema.as_object().is_some_and(|s| !s.is_empty()), "{}", std::any::type_name::<T>());
}

fn call(op: &str) -> OpCall {
    OpCall { op: op.into(), args: json!({"path": "out.png"}) }
}

fn effect() -> Effect {
    Effect { summary: "wrote out.png".into(), undo: call("file.delete"), redo: call("file.restore") }
}

fn job_info(state: JobState) -> JobInfo {
    JobInfo {
        id: JobId(4),
        op: "render.still".into(),
        label: "Render still".into(),
        actor: agent(),
        on_behalf_of: Some(person()),
        session: Some(omuzaki_ops_api::SessionId(2)),
        cause: Some(Cause { kind: CauseKind::Prompt, text: Some("render it".into()) }),
        lane: Lane::Gpu,
        state,
        stage: Some("sampling".into()),
        fraction: 0.5,
        estimate: Some(CostHint { seconds: Some(12.0), gpu_mib: Some(2048), usd: None }),
        spent_usd_micros: 250,
        queued_at: Seconds(1.0),
        started_at: Some(Seconds(2.0)),
        ended_at: None,
    }
}

fn states() -> Vec<JobState> {
    vec![
        JobState::Queued,
        JobState::Running,
        JobState::Held,
        JobState::Done { tx: TxId(3), seq: Some(9) },
        JobState::Done { tx: TxId(3), seq: None },
        JobState::Failed { error: OpError::internal("boom") },
        JobState::Cancelled { by: Some(person()), reason: CancelReason::Requested },
        JobState::Cancelled { by: None, reason: CancelReason::Timeout },
        JobState::Interrupted,
    ]
}

#[test]
fn job_types_round_trip_and_have_schemas() {
    round_trips(&call("file.delete"));
    round_trips(&effect());
    round_trips(&JobId(7));
    for lane in [Lane::Cpu, Lane::Gpu, Lane::Net] {
        round_trips(&lane);
    }
    for reason in [
        CancelReason::Requested,
        CancelReason::Stop,
        CancelReason::SessionEnded,
        CancelReason::Timeout,
        CancelReason::Budget,
        CancelReason::Shutdown,
    ] {
        round_trips(&reason);
    }
    for shape in [JobShape::Plain, JobShape::Driver] {
        round_trips(&shape);
    }
    for state in states() {
        round_trips(&state);
        round_trips(&job_info(state));
    }
    round_trips(&RevertConflict {
        path: DocPath::parse("a/b").unwrap(),
        job_value: json!(1),
        current: Some(json!(2)),
        actor: person(),
    });
    let conflict =
        || RevertConflict { path: DocPath::parse("a/b").unwrap(), job_value: json!(1), current: None, actor: person() };
    round_trips(
        &OpError::cancelled(JobId(4)).with_detail(ErrorDetail::Cancelled { job: JobId(4), reason: CancelReason::Stop }),
    );
    round_trips(&OpError::conflict("revert conflicted").with_detail(ErrorDetail::Unreverted {
        conflicts: vec![conflict()],
        retained: vec![DocPath::parse("a/b").unwrap()],
    }));
    round_trips(&Event::JobChanged { job: job_info(JobState::Running) });
    round_trips(&Event::JobProgress { job: JobId(4), stage: "sampling".into(), fraction: 0.25 });
    round_trips(&Ended {
        abandoned: vec![TxId(5)],
        kept: vec![],
        withdrawn: vec![],
        cancelled: vec![JobId(1), JobId(2)],
    });
}

#[test]
fn job_state_json_is_tagged_by_state() {
    assert_eq!(serde_json::to_value(JobState::Queued).unwrap(), json!({"state": "queued"}));
    assert_eq!(
        serde_json::to_value(JobState::Done { tx: TxId(3), seq: None }).unwrap(),
        json!({"state": "done", "tx": 3, "seq": null})
    );
    assert_eq!(serde_json::to_value(JobId(7)).unwrap(), json!(7));
    assert_eq!(serde_json::to_value(CancelReason::SessionEnded).unwrap(), json!("session_ended"));
}

#[test]
fn old_json_without_the_new_fields_still_parses() {
    let plan: Plan = serde_json::from_value(json!({"patch": {"changes": []}, "output": null, "cost": null})).unwrap();
    assert_eq!(plan.effect, None);
    let with_effect = Plan { effect: Some(effect()), ..Plan::default() };
    assert!(serde_json::to_value(Plan::default()).unwrap().get("effect").is_none(), "None stays off the wire");
    round_trips(&with_effect);

    let record: OpRecord = serde_json::from_value(json!({
        "seq": 1, "op": "x.y", "args": {}, "actor": {"kind": "person", "name": "alice"}, "cause": null, "tx": 1,
        "at": 0.5, "patch": {"changes": []}, "version": 3
    }))
    .unwrap();
    assert_eq!(record.effect, None);

    let ended: Ended = serde_json::from_value(json!({"abandoned": [], "kept": [], "withdrawn": []})).unwrap();
    assert_eq!(ended.cancelled, Vec::<JobId>::new());
}

fn golden(name: &str) -> String {
    std::fs::read_to_string(format!("{}/tests/golden/pre_jobs/{name}.json", env!("CARGO_MANIFEST_DIR"))).unwrap()
}

/// Reads the file written from main before the jobs change, decodes it as `T`, and encodes it back byte for byte.
fn golden_round_trip<T: Serialize + DeserializeOwned + Debug>(name: &str) -> T {
    let text = golden(name);
    let value: T = serde_json::from_str(&text).unwrap();
    let mut back = serde_json::to_string_pretty(&value).unwrap();
    back.push('\n');
    assert_eq!(back, text, "{name}.json no longer serialises byte for byte");
    value
}

#[test]
fn json_written_before_the_jobs_change_is_unchanged() {
    let doc: Value = golden_round_trip("document");
    assert_eq!(doc["character"]["face"]["blend"], json!(0.25));

    let request: Request = golden_round_trip("request");
    assert_eq!((request.op.as_str(), request.mode, request.tx), ("face.set_blend", Mode::Apply, Some(TxId(2))));

    let record: OpRecord = golden_round_trip("op_record");
    assert_eq!((record.seq, record.effect), (1, None));

    let plan: Plan = golden_round_trip("plan");
    assert_eq!(plan.effect, None);
    assert_eq!(plan.cost.and_then(|c| c.seconds), Some(1.5));

    let ended: Ended = golden_round_trip("ended");
    assert_eq!((ended.abandoned, ended.cancelled), (vec![TxId(5)], vec![]));
}

/// A plain op: no job hooks overridden.
#[derive(Serialize, Deserialize, JsonSchema)]
struct NotAJob {}

impl Op for NotAJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.not_a_job",
            summary: "A plain edit.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }
}

omuzaki_ops_api::register_op!(NotAJob);

/// A driver job that overrides all three hooks.
#[derive(Serialize, Deserialize, JsonSchema)]
struct DriverJob {}

impl Op for DriverJob {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.driver_job",
            summary: "A driver job.",
            kind: OpKind::Job,
            scope: Scope::Script,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        job.checkpoint("start", 0.0)?;
        Ok(json!({"job": job.id().0}))
    }

    fn finish(&self, _ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        Ok(Plan { effect: Some(effect()), ..Plan::answer(result) })
    }

    fn job_shape() -> JobShape {
        JobShape::Driver
    }
}

omuzaki_ops_api::register_op!(DriverJob);

#[derive(Default)]
struct Signal {
    cancelled: CancelFlag,
    progress: Mutex<Vec<(String, f32)>>,
    spent: Mutex<u64>,
}

impl JobSignal for Signal {
    fn progress(&self, stage: &str, fraction: f32) {
        self.progress.lock().unwrap().push((stage.to_owned(), fraction));
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.cancelled.clone()
    }

    fn spend(&self, usd_micros: u64) -> Result<(), OpError> {
        let mut spent = self.spent.lock().unwrap();
        if *spent + usd_micros > 100 {
            return Err(OpError {
                code: ErrorCode::Limited,
                message: "over the cap".into(),
                field: None,
                hint: None,
                detail: None,
            });
        }
        *spent += usd_micros;
        Ok(())
    }
}

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

#[derive(Default)]
struct Calls(Mutex<Vec<(String, Value, Mode)>>);

impl JobCalls for Calls {
    fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError> {
        self.0.lock().unwrap().push((op.to_owned(), args, mode));
        Ok(Outcome { plan: Plan::default(), applied: None })
    }
}

fn registered(name: &str) -> &'static OpRegistration {
    omuzaki_ops_api::find(name).expect("registered")
}

#[test]
fn an_op_without_work_is_not_a_job() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(1), &actor, &doc, &signal);
    let op = (registered("test.not_a_job").decode)(json!({})).unwrap();
    let err = op.work(&ctx).unwrap_err();
    assert_eq!(err.code, ErrorCode::Unsupported);
    assert!(err.message.contains("test.not_a_job"), "{}", err.message);
    assert_eq!(op.job_shape(), JobShape::Plain);
    let clock = FakeClock::new(Seconds(0.0));
    let plan = op.finish(&OpCtx::new(&doc.0, &actor, &clock), json!({"answer": 1})).unwrap();
    assert_eq!(plan, Plan::answer(json!({"answer": 1})));
}

#[test]
fn overridden_job_hooks_are_seen_through_the_registration() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(5), &actor, &doc, &signal);
    let op = (registered("test.driver_job").decode)(json!({})).unwrap();
    assert_eq!(op.job_shape(), JobShape::Driver);
    assert_eq!(op.work(&ctx).unwrap(), json!({"job": 5}));
    let clock = FakeClock::new(Seconds(0.0));
    let plan = op.finish(&OpCtx::new(&doc.0, &actor, &clock), json!(1)).unwrap();
    assert_eq!(plan.effect, Some(effect()));
    assert_eq!(plan.output, Some(json!(1)));
}

#[test]
fn checkpoint_reports_progress_then_fails_once_cancelled() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(9), &actor, &doc, &signal);
    ctx.checkpoint("load", 0.1).unwrap();
    signal.cancelled.cancel();
    assert!(ctx.is_cancelled());
    let err = ctx.checkpoint("sample", 0.5).unwrap_err();
    assert_eq!(err.code, ErrorCode::Cancelled);
    assert_eq!(*signal.progress.lock().unwrap(), vec![("load".to_owned(), 0.1), ("sample".to_owned(), 0.5)]);
}

#[test]
fn read_calls_through_to_the_job_doc_and_spend_to_the_signal() {
    let actor = person();
    let doc = Doc(MemDoc::from_value(json!({"a": {"b": 3}})));
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(1), &actor, &doc, &signal);
    let b = ctx.read(|d| d.get(&DocPath::parse("a/b").unwrap()).cloned());
    assert_eq!(b, Some(json!(3)));
    ctx.spend(60).unwrap();
    assert_eq!(ctx.spend(60).unwrap_err().code, ErrorCode::Limited);
    assert_eq!(ctx.actor(), &actor);
}

#[test]
fn call_is_unsupported_in_a_plain_job_and_forwards_in_a_driver_job() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let calls = Calls::default();
    let plain = JobCtx::new(JobId(1), &actor, &doc, &signal);
    assert_eq!(plain.call("x.y", json!({}), Mode::Apply).unwrap_err().code, ErrorCode::Unsupported);
    let driver = JobCtx::new(JobId(1), &actor, &doc, &signal).with_calls(&calls);
    driver.call("x.y", json!({"k": 1}), Mode::DryRun).unwrap();
    assert_eq!(*calls.0.lock().unwrap(), vec![("x.y".to_owned(), json!({"k": 1}), Mode::DryRun)]);
}

#[test]
fn scratch_needs_a_project_folder() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let grants = Grants::person();
    let ctx = JobCtx::new(JobId(3), &actor, &doc, &signal).with_grants(&grants);
    assert_eq!(ctx.scratch().unwrap_err().code, ErrorCode::Invalid, "relative path, no project folder");
    assert_eq!(ctx.fs_access("/nowhere/x", omuzaki_ops_api::FsMode::Read).unwrap_err().code, ErrorCode::Denied);
}

#[test]
fn error_code_cancelled_displays_as_cancelled() {
    assert_eq!(ErrorCode::Cancelled.to_string(), "cancelled");
    assert_eq!(serde_json::to_value(ErrorCode::Cancelled).unwrap(), json!("cancelled"));
}

#[test]
fn script_scope_is_person_only_by_default() {
    assert_eq!("script".parse::<Scope>().unwrap(), Scope::Script);
    assert_eq!(serde_json::to_value(Scope::Script).unwrap(), json!("script"));
    assert!(Scope::ALL.contains(&Scope::Script));
    assert!(Grants::person().allows(Scope::Script));
    assert!(!Grants::agent_default().allows(Scope::Script));
}

#[test]
fn plan_output_and_reserved_money_are_unset_until_the_executor_sets_them() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(1), &actor, &doc, &signal);
    assert_eq!((ctx.plan_output(), ctx.reserved_usd_micros()), (None, 0));
    let chosen = json!({"model": "small"});
    let ctx = ctx.with_plan_output(&chosen).with_reserved_usd_micros(750);
    assert_eq!((ctx.plan_output(), ctx.reserved_usd_micros()), (Some(&chosen), 750));
}

#[test]
fn a_cancel_flag_outlives_the_ctx_and_clones_share_one_flag() {
    let actor = person();
    let doc = Doc(MemDoc::new());
    let signal = Signal::default();
    // The ctx lives only inside this block; the token must outlive it.
    let token = JobCtx::new(JobId(1), &actor, &doc, &signal).cancel_flag();
    let other = token.clone();
    assert!(!token.is_cancelled());
    signal.cancelled.cancel();
    assert!(token.is_cancelled() && other.is_cancelled());
    let ctx = JobCtx::new(JobId(1), &actor, &doc, &signal);
    assert_eq!(ctx.is_cancelled(), ctx.cancel_flag().is_cancelled(), "the ctx and its flag agree");
    assert!(!CancelFlag::new().is_cancelled());
}

#[test]
fn the_flag_and_never_cancel_work_as_dyn_cancel() {
    let flag = CancelFlag::new();
    let handles: [&dyn Cancel; 2] = [&flag, &NeverCancel];
    assert!(handles.iter().all(|h| !h.is_cancelled()));
    flag.cancel();
    assert_eq!(handles.map(Cancel::is_cancelled), [true, false]);
}

/// An absolute setter that opts in to coalescing.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Setter {}

impl Op for Setter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "test.setter",
            summary: "An absolute setter.",
            kind: OpKind::Edit,
            scope: Scope::Read,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::default())
    }

    fn coalesces() -> bool {
        true
    }
}

omuzaki_ops_api::register_op!(Setter);

#[test]
fn coalesces_defaults_to_false_and_an_override_reaches_the_registration() {
    assert!(!(registered("test.not_a_job").coalesces)());
    assert!((registered("test.setter").coalesces)());
}

#[test]
fn op_info_reports_coalesces_and_omits_it_when_false() {
    let reg = registered("test.setter");
    let info = OpInfo::new((reg.meta)(), (reg.schema)());
    assert!(!info.coalesces);
    assert!(serde_json::to_value(&info).unwrap().get("coalesces").is_none(), "false stays off the wire");
    let info = info.with_coalesces((reg.coalesces)());
    assert_eq!(serde_json::to_value(&info).unwrap()["coalesces"], json!(true));
}

#[test]
fn position_detail_round_trips_and_old_errors_still_parse() {
    let error =
        OpError::invalid("source", "unexpected token").with_detail(ErrorDetail::Position { line: 3, column: 14 });
    round_trips(&error);
    let json = serde_json::to_value(&error).unwrap();
    assert_eq!(json["detail"], json!({"kind": "position", "line": 3, "column": 14}));
    let old: OpError = serde_json::from_value(json!({
        "code": "invalid", "message": "m", "field": null, "hint": null
    }))
    .unwrap();
    assert_eq!(old.detail, None);
}
