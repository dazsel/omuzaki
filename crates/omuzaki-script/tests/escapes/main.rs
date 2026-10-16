//! The sandbox-escape corpus: every script in this folder tries to get past the sandbox and must fail with a refusal,
//! never succeed (`docs/design/scripting.md` section 9). `TABLE` names the refusal each one must end in; the test also
//! lists the folder, so a script without a row, or a row without a script, fails.
//!
//! The scripts that depend on how the executor answers (a missing scope, a person-only op, an approval need) run over
//! `ExecutorLike`, a fake that answers as the executor does; the real-executor versions belong to the approval and
//! script-ops changes.

// A test file: a failed unwrap is the failure, and reading the corpus folder is what the test is for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods, clippy::unnecessary_wraps)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

use omuzaki_doc_api::{DocPath, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{
    Actor, Cause, CauseKind, ErrorCode, ErrorDetail, Event, FsMode, Grants, Op, OpCtx, OpError, OpInfo, OpKind, OpMeta,
    OpRecord, Outcome, Plan, Request, Runner, Scope, TxId, Undo, register_op,
};
use omuzaki_ops_mock::MockRunner;
use omuzaki_script::{Limits, RunRequest, RunnerCalls, ScriptErrorKind, ScriptHost};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

// -- the ops the corpus reaches for -------------------------------------------------------------------------------------

macro_rules! fake_op {
    ($name:ident, $op:literal, $kind:expr, $scope:expr, $undo:expr, $plan:expr) => {
        #[derive(Serialize, Deserialize, JsonSchema)]
        struct $name {}

        impl Op for $name {
            fn meta() -> OpMeta {
                OpMeta { name: $op, summary: "A corpus op.", kind: $kind, scope: $scope, undo: $undo }
            }

            fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
                let plan: fn(&OpCtx<'_>) -> Result<Plan, OpError> = $plan;
                plan(ctx)
            }
        }
        register_op!($name);
    };
}

fn nothing(_: &OpCtx<'_>) -> Result<Plan, OpError> {
    Ok(Plan::default())
}

fn read_a_file(ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
    ctx.fs_access("/etc/passwd", FsMode::Read)?;
    Ok(Plan::answer(Value::Null))
}

fn needs_a_person(ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
    ctx.require_person("only a person may do this")?;
    Ok(Plan::default())
}

fn asks_first(_: &OpCtx<'_>) -> Result<Plan, OpError> {
    Err(OpError::needs_person("this publishes to the audience"))
}

fake_op!(Set, "fake.set", OpKind::Edit, Scope::EditCharacter, Undo::Auto, nothing);
fake_op!(Publish, "fake.publish", OpKind::Edit, Scope::Publish, Undo::Auto, nothing);
fake_op!(Render, "fake.render", OpKind::Job, Scope::Render, Undo::Auto, nothing);
fake_op!(WriteFile, "fake.write_file", OpKind::Edit, Scope::Fs, Undo::Explicit, nothing);
fake_op!(ReadFile, "fake.read_file", OpKind::Query, Scope::Fs, Undo::None, read_a_file);
fake_op!(PersonTask, "fake.person_task", OpKind::Edit, Scope::EditCharacter, Undo::Auto, needs_a_person);
fake_op!(Gate, "fake.gate", OpKind::Edit, Scope::EditCharacter, Undo::Auto, asks_first);

// -- a runner that answers like the executor -----------------------------------------------------------------------------

/// The shared mock runner, with the one executor behaviour it leaves out: a request whose cause is `Script` is refused
/// where the executor would queue it for approval, with the need as the detail (`docs/design/scripting.md` section 3).
struct ExecutorLike {
    inner: MockRunner<MemDoc>,
    /// Every request that reached the runner.
    seen: Mutex<Vec<Request>>,
}

impl ExecutorLike {
    fn new() -> Self {
        Self { inner: MockRunner::new(MemDoc::new()), seen: Mutex::new(Vec::new()) }
    }
}

impl Runner for ExecutorLike {
    fn ops(&self) -> Vec<OpInfo> {
        self.inner.ops()
    }

    fn run(&self, req: Request) -> Result<Outcome, OpError> {
        self.seen.lock().unwrap().push(req.clone());
        let scripted = req.cause.as_ref().is_some_and(|cause| cause.kind == CauseKind::Script);
        self.inner.run(req).map_err(|error| match error.detail.as_deref() {
            Some(detail @ ErrorDetail::NeedsPerson { .. }) if scripted => OpError::denied(error.message.clone())
                .hint("grant the scope to this session before running the script; a script is never queued")
                .with_detail(detail.clone()),
            _ => error,
        })
    }

    fn begin(&self, label: &str, actor: &Actor) -> TxId {
        self.inner.begin(label, actor)
    }

    fn commit(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.commit(tx, actor)
    }

    fn rollback(&self, tx: TxId, actor: &Actor) -> Result<(), OpError> {
        self.inner.rollback(tx, actor)
    }

    fn undo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.undo(actor, grants)
    }

    fn redo(&self, actor: &Actor, grants: &Grants) -> Result<Option<String>, OpError> {
        self.inner.redo(actor, grants)
    }

    fn subscribe(&self, grants: &Grants) -> Result<Receiver<Event>, OpError> {
        self.inner.subscribe(grants)
    }

    fn get(&self, path: &DocPath, grants: &Grants) -> Result<Option<Value>, OpError> {
        self.inner.get(path, grants)
    }

    fn log(&self, after_seq: u64, limit: usize, grants: &Grants) -> Result<Vec<OpRecord>, OpError> {
        self.inner.log(after_seq, limit, grants)
    }
}

// -- the table -----------------------------------------------------------------------------------------------------------

/// Who runs the script.
#[derive(Copy, Clone)]
enum Caller {
    Person,
    Agent,
}

/// One corpus script and the refusal it must end in: the script's error kind and, for a refusal that comes from an op
/// answer, the `OpError` code.
struct Row {
    script: &'static str,
    caller: Caller,
    kind: ScriptErrorKind,
    code: Option<ErrorCode>,
}

const fn row(script: &'static str, caller: Caller, kind: ScriptErrorKind, code: Option<ErrorCode>) -> Row {
    Row { script, caller, kind, code }
}

const PARSE: ScriptErrorKind = ScriptErrorKind::Parse;
const RUNTIME: ScriptErrorKind = ScriptErrorKind::Runtime;
const REFUSED: ScriptErrorKind = ScriptErrorKind::Refused;
const OP: ScriptErrorKind = ScriptErrorKind::Op;

const TABLE: [Row; 19] = [
    row("eval.rhai", Caller::Person, PARSE, None),
    row("fn_pointer_by_name.rhai", Caller::Person, PARSE, None),
    row("call_function.rhai", Caller::Person, PARSE, None),
    row("curry_pointer.rhai", Caller::Person, PARSE, None),
    row("import_module.rhai", Caller::Person, PARSE, None),
    row("closure_kept_in_array.rhai", Caller::Person, PARSE, None),
    row("closure_kept_in_map.rhai", Caller::Person, PARSE, None),
    row("closure_returned.rhai", Caller::Person, PARSE, None),
    row("closure_curried_capture.rhai", Caller::Person, PARSE, None),
    row("computed_name_variable.rhai", Caller::Person, RUNTIME, None),
    row("computed_index.rhai", Caller::Person, PARSE, None),
    row("op_inside_fn.rhai", Caller::Person, REFUSED, Some(ErrorCode::Unsupported)),
    row("nested_script_run.rhai", Caller::Person, REFUSED, Some(ErrorCode::Unsupported)),
    row("job_op.rhai", Caller::Person, REFUSED, Some(ErrorCode::Unsupported)),
    row("explicit_undo_op.rhai", Caller::Person, REFUSED, Some(ErrorCode::Unsupported)),
    row("path_outside_every_root.rhai", Caller::Person, OP, Some(ErrorCode::Denied)),
    row("outside_the_ceiling.rhai", Caller::Person, OP, Some(ErrorCode::Denied)),
    row("require_person_from_an_agent.rhai", Caller::Agent, OP, Some(ErrorCode::Denied)),
    row("approval_need.rhai", Caller::Person, OP, Some(ErrorCode::Denied)),
];

// -- the tests -----------------------------------------------------------------------------------------------------------

fn corpus() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests").join("escapes")
}

fn source(script: &str) -> String {
    std::fs::read_to_string(corpus().join(script)).unwrap_or_else(|e| panic!("{script}: {e}"))
}

fn actor(caller: Caller) -> Actor {
    match caller {
        Caller::Person => Actor::Person { name: "alice".into() },
        Caller::Agent => Actor::Agent { id: "claude-1".into(), model: None },
    }
}

fn grants(caller: Caller) -> Grants {
    match caller {
        Caller::Person => Grants::person(),
        // What an agent holds with the `script` scope granted for the session.
        Caller::Agent => Grants::agent_default().with(Scope::Script).with(Scope::Publish),
    }
}

#[test]
fn every_script_in_the_corpus_has_a_row_and_every_row_a_script() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(corpus())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_ascii_lowercase().ends_with(".rhai"))
        .collect();
    let in_table: BTreeSet<String> = TABLE.iter().map(|row| row.script.to_owned()).collect();
    assert_eq!(on_disk, in_table, "the corpus folder and the table disagree");
    assert_eq!(in_table.len(), TABLE.len(), "a script is listed twice");
    assert!(TABLE.len() >= 14, "the design lists fourteen kinds of escape");
}

#[test]
fn every_script_is_refused_with_the_expected_kind_and_code() {
    for row in &TABLE {
        let runner = Arc::new(ExecutorLike::new());
        let host = ScriptHost::new(&runner.ops(), Limits::console());
        let who = actor(row.caller);
        let calls = RunnerCalls::new(runner.clone(), row.script, who.clone(), grants(row.caller));
        let request = RunRequest {
            name: row.script.to_owned(),
            source: source(row.script),
            actor: who,
            grants: grants(row.caller),
            tx: None,
            params: serde_json::Map::new(),
            dry_run: false,
        };
        let result = host.run(request, &calls, &Never);
        let error = match result {
            Ok(outcome) => panic!("{} succeeded: {outcome:?}", row.script),
            Err(error) => error,
        };
        assert_eq!(error.kind, row.kind, "{}: {error}", row.script);
        assert_eq!(error.op_error.as_ref().map(|e| e.code), row.code, "{}: {error}", row.script);
        assert_eq!(runner.inner.records(), vec![], "{}: the refused script changed nothing", row.script);
        assert!(!runner.inner.has_open_tx(), "{}: nothing is left open", row.script);
    }
}

struct Never;

impl omuzaki_script::Cancel for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn an_approval_need_is_refused_with_the_need_as_the_detail_and_never_queued() {
    let row = TABLE.iter().find(|row| row.script == "approval_need.rhai").unwrap();
    let runner = Arc::new(ExecutorLike::new());
    let host = ScriptHost::new(&runner.ops(), Limits::console());
    let who = actor(row.caller);
    let calls = RunnerCalls::new(runner.clone(), "gate", who.clone(), grants(row.caller));
    let request = RunRequest {
        name: "gate".into(),
        source: source(row.script),
        actor: who,
        grants: grants(row.caller),
        tx: None,
        params: serde_json::Map::new(),
        dry_run: false,
    };
    let error = host.run(request, &calls, &Never).unwrap_err();
    let op_error = error.op_error.expect("the op's answer");
    assert!(matches!(op_error.detail.as_deref(), Some(ErrorDetail::NeedsPerson { .. })), "{op_error}");
    assert!(op_error.hint.as_deref().unwrap().contains("never queued"));
    let sent = runner.inner.records();
    assert_eq!(sent, vec![], "nothing was applied");
}

#[test]
fn check_lists_the_op_inside_the_fn_and_closure_even_though_the_run_refuses_it() {
    let runner = ExecutorLike::new();
    let host = ScriptHost::new(&runner.ops(), Limits::console());
    let report = host.check(&source("op_inside_fn.rhai"));
    assert_eq!(report.ops.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["script.run"]);
    assert_eq!(report.ops[0].line, 3, "it is found where it is written, inside the function");
    assert_eq!(report.refused.len(), 1);
}

#[test]
fn the_parse_rows_are_refused_by_the_check_too() {
    let runner = ExecutorLike::new();
    let host = ScriptHost::new(&runner.ops(), Limits::console());
    for row in TABLE.iter().filter(|row| row.kind == PARSE) {
        let report = host.check(&source(row.script));
        assert!(!report.parse_errors.is_empty(), "{}: {report:?}", row.script);
    }
}

#[test]
fn the_cause_of_every_inner_request_is_the_script() {
    // The executor's never-queue rule keys on this cause, so a script that reached an op without it would be queued.
    let runner = Arc::new(ExecutorLike::new());
    let calls = RunnerCalls::new(runner.clone(), "probe", actor(Caller::Person), Grants::person());
    let host = ScriptHost::new(&runner.ops(), Limits::console());
    let request = RunRequest {
        name: "probe".into(),
        source: "ops::fake::set(#{}); ops::fake::set(#{});".into(),
        actor: actor(Caller::Person),
        grants: Grants::person(),
        tx: None,
        params: serde_json::Map::new(),
        dry_run: false,
    };
    host.run(request, &calls, &Never).unwrap();
    let seen = runner.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    for request in seen.iter() {
        assert_eq!(request.cause, Some(Cause { kind: CauseKind::Script, text: Some("probe".into()) }));
        assert!(request.tx.is_some(), "an edit joins the run's transaction");
    }
}
