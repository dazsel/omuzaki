//! The `script.*` ops: check, list, save, delete and run, over the shared mock runner, and with the ops planned and
//! run by hand where a store or a job is involved.

#![allow(clippy::unwrap_used, clippy::expect_used)] // a test file: a failed unwrap is the failure

use std::sync::{Arc, Mutex};

use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Cancel as _;
use omuzaki_ops_api::{
    Actor, CancelFlag, CostHint, DynOp, ErrorCode, ErrorDetail, Grants, JobCalls, JobCtx, JobDoc, JobId, JobShape,
    JobSignal, Mode, Op, OpCtx, OpError, OpKind, OpMeta, Outcome, Plan, Request, Runner, Scope, Services,
    ServicesBuilder, Undo, find, register_op,
};
use omuzaki_ops_mock::MockRunner;
use omuzaki_script::{Limits, MemScriptStore, ScriptKind, ScriptStore, content_hash};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

// -- two fake ops for scripts to call ---------------------------------------------------------------------------------

/// Sets a document path (an edit that undoes with its patch).
#[derive(Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct FakeSet {
    path: DocPath,
    value: Value,
}

impl Op for FakeSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fake.set",
            summary: "Set a path.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let before = ctx.doc.get(&self.path).cloned();
        Ok(Plan::edit(Patch::new().with(self.path.clone(), before, Some(self.value.clone()))))
    }
}
register_op!(FakeSet);

/// Answers a fixed value.
#[derive(Serialize, Deserialize, JsonSchema)]
struct FakeRead {}

impl Op for FakeRead {
    fn meta() -> OpMeta {
        OpMeta { name: "fake.read", summary: "Read.", kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!(7)))
    }
}
register_op!(FakeRead);

// -- helpers ----------------------------------------------------------------------------------------------------------

fn alice() -> Actor {
    Actor::Person { name: "alice".into() }
}

fn agent() -> Actor {
    Actor::Agent { id: "claude-1".into(), model: None }
}

fn runner() -> MockRunner<MemDoc> {
    MockRunner::new(MemDoc::new())
}

fn request(op: &str, args: Value, actor: &Actor, grants: &Grants) -> Request {
    Request {
        op: op.into(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: grants.clone(),
        cause: None,
        tx: None,
        base: None,
    }
}

fn run_as_person(runner: &MockRunner<MemDoc>, op: &str, args: Value) -> Result<Outcome, OpError> {
    runner.run(request(op, args, &alice(), &Grants::person()))
}

fn doc_value(runner: &MockRunner<MemDoc>, path: &str) -> Option<Value> {
    runner.get(&DocPath::parse(path).unwrap(), &Grants::person()).unwrap()
}

fn decode(op: &str, args: Value) -> Box<dyn DynOp> {
    (find(op).expect("registered").decode)(args).unwrap_or_else(|e| panic!("{op}: {e}"))
}

fn services(store: &Arc<MemScriptStore>) -> Services {
    let store: Arc<dyn ScriptStore> = store.clone();
    ServicesBuilder::new().insert::<dyn ScriptStore>(store).build()
}

/// Plans `op` by hand over `doc`, as `actor`, with `services`.
fn plan_with(op: &dyn DynOp, doc: &MemDoc, services: &Services, dry_run: bool) -> Result<Plan, OpError> {
    let clock = FakeClock::new(Seconds(0.0));
    let actor = alice();
    op.plan(&OpCtx::new(doc, &actor, &clock).with_services(services).with_dry_run(dry_run))
}

// -- script.check -----------------------------------------------------------------------------------------------------

#[test]
fn check_is_read_only_and_returns_the_report() {
    let runner = runner();
    let source = "ops::fake::set(#{ path: \"a\", value: 1 });\nops::fake::nope(#{});";
    let outcome = run_as_person(&runner, "script.check", json!({ "source": source })).unwrap();
    let report = outcome.plan.output.unwrap();
    assert_eq!(report["ops"], json!([{ "name": "fake.set", "line": 1, "column": 12 }]));
    assert_eq!(report["unknown_ops"], json!([{ "name": "fake.nope", "line": 2, "column": 12 }]));
    assert_eq!(report["scopes"], json!(["edit:character"]));
    assert_eq!(runner.records(), vec![], "a check applies nothing");
    assert_eq!(outcome.plan.patch, Patch::new());
    let meta = (find("script.check").unwrap().meta)();
    assert_eq!((meta.kind, meta.scope, meta.undo), (OpKind::Query, Scope::Read, Undo::None));
}

#[test]
fn check_works_with_read_alone_and_never_runs_the_script() {
    let runner = runner();
    let read_only = Grants::of([Scope::Read]);
    let source = "ops::fake::set(#{ path: \"a\", value: 1 })";
    runner.run(request("script.check", json!({ "source": source }), &agent(), &read_only)).unwrap();
    assert_eq!(doc_value(&runner, "a"), None);
}

#[test]
fn check_reports_the_scripts_own_ops_as_refused_or_known() {
    let runner = runner();
    let source = "ops::script::run(#{ source: \"1\" }); ops::script::save(#{}); ops::script::list(#{});";
    let report = run_as_person(&runner, "script.check", json!({ "source": source })).unwrap().plan.output.unwrap();
    let refused: Vec<&str> = report["refused"].as_array().unwrap().iter().map(|r| r["op"].as_str().unwrap()).collect();
    assert_eq!(refused, ["script.run", "script.save"], "a nested run and an effectful save are refused");
}

// -- script.save, script.delete and script.list in the project ---------------------------------------------------------

#[test]
fn save_writes_the_project_node_and_list_shows_it() {
    let runner = runner();
    let saved = run_as_person(
        &runner,
        "script.save",
        json!({ "id": "hello", "name": "Hello", "source": "print(\"hi\");", "place": "project" }),
    )
    .unwrap();
    let hash = content_hash("print(\"hi\");");
    assert_eq!(saved.plan.output.unwrap()["hash"], json!(hash));
    let node = doc_value(&runner, "scripts/hello").expect("the node");
    assert_eq!(node["name"], "Hello");
    assert_eq!(node["kind"], "script");
    assert_eq!(node["source"], "print(\"hi\");");
    assert_eq!(node["hash"], json!(hash));
    assert_eq!(node["author"], json!(alice()), "the last author is kept");

    let listing = run_as_person(&runner, "script.list", json!({})).unwrap().plan.output.unwrap();
    assert_eq!(
        listing["scripts"],
        json!([{ "id": "hello", "name": "Hello", "kind": "script", "place": "project", "hash": hash, "author": alice() }])
    );
}

#[test]
fn saving_again_replaces_and_a_second_item_joins_the_first() {
    let runner = runner();
    let save = |id: &str, source: &str| {
        run_as_person(&runner, "script.save", json!({ "id": id, "name": id, "source": source, "place": "project" }))
            .unwrap();
    };
    save("one", "1");
    save("two", "2");
    save("one", "3");
    assert_eq!(doc_value(&runner, "scripts/one").unwrap()["source"], "3");
    assert_eq!(doc_value(&runner, "scripts/two").unwrap()["source"], "2");
    let listing = run_as_person(&runner, "script.list", json!({})).unwrap().plan.output.unwrap();
    let ids: Vec<&str> = listing["scripts"].as_array().unwrap().iter().map(|s| s["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["one", "two"]);
}

#[test]
fn a_macro_is_saved_as_steps_with_its_kind() {
    let runner = runner();
    let steps = json!([{ "op": "fake.set", "args": { "path": "a", "value": 1 } }]);
    run_as_person(&runner, "script.save", json!({ "id": "m", "name": "M", "steps": steps, "place": "project" }))
        .unwrap();
    let node = doc_value(&runner, "scripts/m").unwrap();
    assert_eq!((node["kind"].as_str(), &node["steps"]), (Some("macro"), &steps));
    assert!(node.get("source").is_none());
    let listing = run_as_person(&runner, "script.list", json!({})).unwrap().plan.output.unwrap();
    assert_eq!(listing["scripts"][0]["kind"], "macro");
}

#[test]
fn save_refuses_a_macro_step_a_script_could_not_call() {
    let runner = runner();
    for (op, needle) in [("script.run", "another script run"), ("script.save", "outside the document")] {
        let steps = json!([
            { "op": "fake.set", "args": { "path": "a", "value": 1 } },
            { "op": op, "args": { "id": "loop" } },
        ]);
        let args = json!({ "id": "loop", "name": "Loop", "steps": steps, "place": "project" });
        let error = run_as_person(&runner, "script.save", args).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("steps")), "{op}: {error}");
        assert!(error.message.contains("step 2") && error.message.contains(op), "{op}: {error}");
        assert!(error.message.contains(needle), "{op}: {error}");
        assert!(error.hint.is_some(), "{op}: {error}");
    }
    assert_eq!(runner.records(), vec![], "nothing was saved");
    assert_eq!(doc_value(&runner, "scripts/loop"), None);
}

#[test]
fn delete_removes_the_node_and_a_missing_one_is_not_found() {
    let runner = runner();
    run_as_person(&runner, "script.save", json!({ "id": "x", "name": "X", "source": "1", "place": "project" }))
        .unwrap();
    run_as_person(&runner, "script.delete", json!({ "id": "x", "place": "project" })).unwrap();
    assert_eq!(doc_value(&runner, "scripts/x"), None);
    let missing = run_as_person(&runner, "script.delete", json!({ "id": "x", "place": "project" })).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
    let nothing_at_all = MockRunner::new(MemDoc::new());
    let error = run_as_person(&nothing_at_all, "script.delete", json!({ "id": "x", "place": "project" })).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[test]
fn an_agent_without_the_script_scope_is_denied_naming_it() {
    let runner = runner();
    for (op, args) in [
        ("script.save", json!({ "id": "x", "name": "X", "source": "1", "place": "project" })),
        ("script.delete", json!({ "id": "x", "place": "project" })),
    ] {
        let error = runner.run(request(op, args, &agent(), &Grants::agent_default())).unwrap_err();
        assert_eq!(error.code, ErrorCode::Denied, "{op}");
        assert!(error.message.contains("`script`"), "{op}: {}", error.message);
    }
    assert_eq!(runner.records(), vec![], "nothing was planted");
    // With the scope granted for the session, the agent may.
    let granted = Grants::agent_default().with(Scope::Script);
    runner
        .run(request(
            "script.save",
            json!({ "id": "x", "name": "X", "source": "1", "place": "project" }),
            &agent(),
            &granted,
        ))
        .unwrap();
}

#[test]
fn listing_and_checking_need_only_read() {
    let runner = runner();
    let read = Grants::of([Scope::Read]);
    runner.run(request("script.list", json!({}), &agent(), &read)).unwrap();
}

#[test]
fn save_refuses_bad_input_naming_the_field() {
    let runner = runner();
    let cases = [
        (json!({ "id": "x", "name": "X", "place": "project" }), "source"),
        (json!({ "id": "x", "name": "X", "source": "1", "steps": [], "place": "project" }), "source"),
        (json!({ "id": "x", "name": "  ", "source": "1", "place": "project" }), "name"),
        (json!({ "id": "x", "name": "X", "source": "x".repeat(64 * 1024 + 1), "place": "project" }), "source"),
        (json!({ "id": "x", "name": "X", "steps": [{ "op": "a.b", "args": [] }], "place": "project" }), "steps"),
        (json!({ "id": "x", "name": "X", "steps": [{ "op": "", "args": {} }], "place": "project" }), "steps"),
        (json!({ "id": "Not A Slug", "name": "X", "source": "1", "place": "project" }), "id"),
    ];
    for (args, field) in cases {
        let error = run_as_person(&runner, "script.save", args.clone()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{args}");
        assert_eq!(error.field.as_deref(), Some(field), "{args}: {error}");
    }
    assert_eq!(runner.records(), vec![]);
}

#[test]
fn list_reports_a_damaged_scripts_node_instead_of_an_empty_list() {
    let runner = MockRunner::new(MemDoc::from_value(json!({ "scripts": "oops" })));
    let error = run_as_person(&runner, "script.list", json!({})).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("scripts")), "{error}");
    let empty = MockRunner::new(MemDoc::new());
    let listing = run_as_person(&empty, "script.list", json!({})).unwrap().plan.output.unwrap();
    assert_eq!(listing, json!({ "scripts": [] }), "a missing node is just an empty list");
}

#[test]
fn a_damaged_scripts_node_is_reported_not_overwritten() {
    let runner = MockRunner::new(MemDoc::from_value(json!({ "scripts": "oops" })));
    let error =
        run_as_person(&runner, "script.save", json!({ "id": "x", "name": "X", "source": "1", "place": "project" }))
            .unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid);
    assert_eq!(doc_value(&runner, "scripts"), Some(json!("oops")));
}

#[test]
fn list_marks_a_node_it_cannot_read() {
    let runner = MockRunner::new(MemDoc::from_value(json!({ "scripts": { "bad": { "name": "Bad" } } })));
    let listing = run_as_person(&runner, "script.list", json!({})).unwrap().plan.output.unwrap();
    assert_eq!(listing["scripts"][0]["id"], "bad");
    assert!(listing["scripts"][0]["problem"].as_str().unwrap().contains("kind"));
}

// -- My scripts -------------------------------------------------------------------------------------------------------

#[test]
fn mine_without_a_store_is_unavailable() {
    let runner = runner();
    for (op, args) in [
        ("script.save", json!({ "id": "x", "name": "X", "source": "1", "place": "mine" })),
        ("script.delete", json!({ "id": "x", "place": "mine" })),
    ] {
        let error = run_as_person(&runner, op, args).unwrap_err();
        assert_eq!(error.code, ErrorCode::Unavailable, "{op}");
    }
    // A listing has nothing to show from My scripts, which is not an error.
    run_as_person(&runner, "script.list", json!({})).unwrap();
}

#[test]
fn save_to_mine_writes_the_store_changes_no_node_and_carries_an_effect() {
    let store = Arc::new(MemScriptStore::new());
    let doc = MemDoc::new();
    let op = decode("script.save", json!({ "id": "mine1", "name": "Mine", "source": "print(1);", "place": "mine" }));

    let dry = plan_with(op.as_ref(), &doc, &services(&store), true).unwrap();
    assert!(store.list().unwrap().is_empty(), "a dry run writes nothing");
    assert!(dry.effect.is_some(), "but it shows the effect");

    let plan = plan_with(op.as_ref(), &doc, &services(&store), false).unwrap();
    assert_eq!(plan.patch, Patch::new(), "My scripts is not the document");
    let stored = store.get("mine1").unwrap().expect("saved");
    assert_eq!((stored.kind, stored.body.as_str(), stored.name.as_str()), (ScriptKind::Script, "print(1);", "Mine"));
    assert_eq!(stored.hash, content_hash("print(1);"));
    let effect = plan.effect.expect("an effect");
    assert_eq!(
        (effect.undo.op.as_str(), &effect.undo.args),
        ("script.delete", &json!({ "id": "mine1", "place": "mine" }))
    );
    assert_eq!(effect.redo.op, "script.save");
    assert_eq!(effect.redo.args["source"], "print(1);");
}

#[test]
fn saving_over_an_item_in_mine_undoes_to_the_earlier_one() {
    let store = Arc::new(MemScriptStore::new());
    let doc = MemDoc::new();
    let first = decode("script.save", json!({ "id": "s", "name": "S", "source": "old", "place": "mine" }));
    plan_with(first.as_ref(), &doc, &services(&store), false).unwrap();
    let second = decode("script.save", json!({ "id": "s", "name": "S2", "source": "new", "place": "mine" }));
    let plan = plan_with(second.as_ref(), &doc, &services(&store), false).unwrap();
    let undo = plan.effect.unwrap().undo;
    assert_eq!(undo.op, "script.save");
    assert_eq!((undo.args["source"].as_str(), undo.args["name"].as_str()), (Some("old"), Some("S")));
}

#[test]
fn delete_from_mine_removes_the_entry_and_its_undo_saves_it_back() {
    let store = Arc::new(MemScriptStore::new());
    let doc = MemDoc::new();
    let steps = json!([{ "op": "fake.set", "args": { "path": "a", "value": 1 } }]);
    let save = decode("script.save", json!({ "id": "mac", "name": "Mac", "steps": steps, "place": "mine" }));
    plan_with(save.as_ref(), &doc, &services(&store), false).unwrap();
    assert_eq!(store.get("mac").unwrap().unwrap().kind, ScriptKind::Macro);

    let delete = decode("script.delete", json!({ "id": "mac", "place": "mine" }));
    let plan = plan_with(delete.as_ref(), &doc, &services(&store), false).unwrap();
    assert!(store.get("mac").unwrap().is_none());
    let effect = plan.effect.unwrap();
    assert_eq!(effect.undo.op, "script.save");
    assert_eq!(effect.undo.args["steps"], steps, "the macro comes back as steps");
    assert_eq!(effect.redo.op, "script.delete");

    let again = plan_with(delete.as_ref(), &doc, &services(&store), false).unwrap_err();
    assert_eq!(again.code, ErrorCode::NotFound);
}

#[test]
fn list_shows_both_places_with_a_store() {
    let store = Arc::new(MemScriptStore::new());
    let doc = MemDoc::from_value(json!({ "scripts": { "pj": { "name": "P", "kind": "script", "source": "1" } } }));
    let save = decode("script.save", json!({ "id": "mn", "name": "M", "source": "2", "place": "mine" }));
    plan_with(save.as_ref(), &doc, &services(&store), false).unwrap();
    let list = decode("script.list", json!({}));
    let listing = plan_with(list.as_ref(), &doc, &services(&store), false).unwrap().output.unwrap();
    let rows: Vec<(&str, &str)> = listing["scripts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["place"].as_str().unwrap(), s["id"].as_str().unwrap()))
        .collect();
    assert_eq!(rows, [("project", "pj"), ("mine", "mn")]);
    assert_eq!(listing["scripts"][1]["hash"], json!(content_hash("2")));
}

// -- script.run: planning ---------------------------------------------------------------------------------------------

#[test]
fn run_is_a_driver_job_behind_the_script_scope() {
    let registration = find("script.run").expect("registered");
    let meta = (registration.meta)();
    assert_eq!((meta.kind, meta.scope), (OpKind::Job, Scope::Script));
    let op = decode("script.run", json!({ "source": "1" }));
    assert_eq!(op.job_shape(), JobShape::Driver);
}

fn plan_run(args: Value, doc: &MemDoc) -> Result<Plan, OpError> {
    let store = Arc::new(MemScriptStore::new());
    plan_with(decode("script.run", args).as_ref(), doc, &services(&store), false)
}

#[test]
fn run_plans_an_empty_patch_and_the_seconds_it_was_given() {
    let plan = plan_run(json!({ "source": "1 + 1" }), &MemDoc::new()).unwrap();
    assert_eq!(plan.patch, Patch::new(), "the run's edits are its inner calls");
    assert_eq!(plan.cost, Some(CostHint { seconds: Some(120.0), gpu_mib: None, usd: None }));
    let plan = plan_run(json!({ "source": "1", "seconds": 600 }), &MemDoc::new()).unwrap();
    assert_eq!(plan.cost.unwrap().seconds, Some(600.0));
    for seconds in [0, 601] {
        let error = plan_run(json!({ "source": "1", "seconds": seconds }), &MemDoc::new()).unwrap_err();
        assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("seconds")), "{seconds}");
    }
}

#[test]
fn run_refuses_an_oversized_source() {
    let error = plan_run(json!({ "source": " ".repeat(64 * 1024 + 1) }), &MemDoc::new()).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("source")));
}

#[test]
fn run_refuses_a_source_the_check_rejects_at_the_first_errors_position() {
    let source = "let a = 1;\nops::fake::set(#{ path: \"a\", valu: 1 });\nops::fake::nope(#{});";
    let error = plan_run(json!({ "source": source }), &MemDoc::new()).unwrap_err();
    assert_eq!((error.code, error.field.as_deref()), (ErrorCode::Invalid, Some("source")));
    let first =
        omuzaki_script::registered_host(omuzaki_script::Limits::console()).check(source).first_problem().unwrap();
    assert_eq!(first.line, 2);
    assert_eq!(error.detail.as_deref(), Some(&ErrorDetail::Position { line: first.line, column: first.column }));
    assert!(
        error.message.ends_with(&format!("(line {}, column {})", first.line, first.column)),
        "text-only clients get the position too: {}",
        error.message
    );
    let syntax = plan_run(json!({ "source": "let = ;" }), &MemDoc::new()).unwrap_err();
    assert!(matches!(syntax.detail.as_deref(), Some(ErrorDetail::Position { line: 1, .. })), "{syntax}");
}

#[test]
fn run_needs_exactly_one_of_source_and_id() {
    for args in [json!({}), json!({ "source": "1", "id": "x" })] {
        let error = plan_run(args.clone(), &MemDoc::new()).unwrap_err();
        assert_eq!(error.code, ErrorCode::Invalid, "{args}");
    }
    let missing = plan_run(json!({ "id": "nothing" }), &MemDoc::new()).unwrap_err();
    assert_eq!(missing.code, ErrorCode::NotFound);
}

#[test]
fn run_by_id_finds_a_project_script_and_a_my_scripts_macro() {
    let doc = MemDoc::from_value(json!({ "scripts": { "pj": { "name": "P", "kind": "script", "source": "1 + 1" } } }));
    let planned = plan_run(json!({ "id": "pj" }), &doc).unwrap().output.unwrap();
    assert_eq!(planned, json!({ "kind": "script", "name": "P", "source": "1 + 1" }));

    let store = Arc::new(MemScriptStore::new());
    let empty = MemDoc::new();
    let steps = json!([{ "op": "fake.set", "args": { "path": "a", "value": 1 } }]);
    let save = decode("script.save", json!({ "id": "mac", "name": "Mac", "steps": steps, "place": "mine" }));
    plan_with(save.as_ref(), &empty, &services(&store), false).unwrap();
    let run = decode("script.run", json!({ "id": "mac" }));
    let planned = plan_with(run.as_ref(), &empty, &services(&store), false).unwrap().output.unwrap();
    assert_eq!(planned, json!({ "kind": "macro", "name": "Mac", "steps": steps }));
}

// -- script.run: work, over a hand-built JobCtx ---------------------------------------------------------------------

#[derive(Default)]
struct Signal {
    cancelled: CancelFlag,
}

impl JobSignal for Signal {
    fn progress(&self, _stage: &str, _fraction: f32) {}

    fn is_cancelled(&self) -> bool {
        self.cancelled.is_cancelled()
    }

    fn cancel_flag(&self) -> CancelFlag {
        self.cancelled.clone()
    }

    fn spend(&self, _usd_micros: u64) -> Result<(), OpError> {
        Ok(())
    }
}

struct Doc(MemDoc);

impl JobDoc for Doc {
    fn read(&self, f: &mut dyn FnMut(&dyn DocRead)) {
        f(&self.0);
    }
}

/// Answers every inner request and remembers it; fails the ones for `fake.fail`.
#[derive(Default)]
struct Calls {
    sent: Mutex<Vec<(String, Value, Mode)>>,
    cancel_after_first: Option<Arc<Signal>>,
    /// How long each request takes.
    delay: std::time::Duration,
}

impl JobCalls for Calls {
    fn call(&self, op: &str, args: Value, mode: Mode) -> Result<Outcome, OpError> {
        self.sent.lock().unwrap().push((op.to_owned(), args, mode));
        std::thread::sleep(self.delay);
        if let Some(signal) = &self.cancel_after_first {
            signal.cancelled.cancel();
        }
        match op {
            "fake.read" => Ok(Outcome::new(Plan::answer(json!(7)))),
            "fake.set" => Ok(Outcome::new(Plan::edit(Patch::new()))),
            other => Err(OpError::not_found(format!("no op {other}")).hint("the fake knows two ops")),
        }
    }
}

fn work(args: Value, doc: Value, calls: &Calls, signal: &Signal) -> Result<Value, OpError> {
    let op = decode("script.run", args);
    let store = Arc::new(MemScriptStore::new());
    let mem = MemDoc::from_value(doc);
    let planned = plan_with(op.as_ref(), &mem, &services(&store), false)?.output.unwrap();
    let actor = alice();
    let document = Doc(mem);
    let ctx = JobCtx::new(JobId(1), &actor, &document, signal).with_calls(calls).with_plan_output(&planned);
    op.work(&ctx)
}

#[test]
fn work_sends_every_inner_request_through_the_job_with_apply_and_returns_the_outcome() {
    let calls = Calls::default();
    let source = "let a = ops::fake::read(#{}); ops::fake::set(#{ path: \"a\", value: a }); print(`got ${a}`); a * 2";
    let result = work(json!({ "source": source }), json!({}), &calls, &Signal::default()).unwrap();
    assert_eq!(result["value"], 14);
    assert_eq!(result["calls"], 2);
    assert_eq!(result["output"][0]["text"], "got 7");
    assert_eq!(result["tx"], Value::Null, "the executor owns a driver job's transaction");
    let sent = calls.sent.lock().unwrap();
    assert_eq!(
        *sent,
        [
            ("fake.read".to_owned(), json!({}), Mode::Apply),
            ("fake.set".to_owned(), json!({ "path": "a", "value": 7 }), Mode::Apply),
        ]
    );
}

#[test]
fn work_with_dry_run_sends_dry_runs_and_lists_the_plans() {
    let calls = Calls::default();
    let source = "ops::fake::set(#{ path: \"a\", value: 1 })";
    let result = work(json!({ "source": source, "dry_run": true }), json!({}), &calls, &Signal::default()).unwrap();
    assert_eq!(calls.sent.lock().unwrap()[0].2, Mode::DryRun);
    assert_eq!(result["plans"][0]["op"], "fake.set");
}

#[test]
fn work_reads_params_and_the_document() {
    let calls = Calls::default();
    let source = "get(\"face/blend\") + params.extra";
    let result = work(
        json!({ "source": source, "params": { "extra": 1 } }),
        json!({ "face": { "blend": 4 } }),
        &calls,
        &Signal::default(),
    )
    .unwrap();
    assert_eq!(result["value"], 5);
    assert!(calls.sent.lock().unwrap().is_empty(), "get reads the document, it does not send a request");
}

#[test]
fn work_ends_cancelled_when_the_token_is_set() {
    let signal = Arc::new(Signal::default());
    let calls = Calls { cancel_after_first: Some(signal.clone()), ..Calls::default() };
    let source = "ops::fake::set(#{ path: \"a\", value: 1 }); ops::fake::set(#{ path: \"b\", value: 2 })";
    let error = work(json!({ "source": source }), json!({}), &calls, &signal).unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert_eq!(calls.sent.lock().unwrap().len(), 1, "the second call was not sent");
}

#[test]
fn work_maps_an_inner_error_to_the_runs_error() {
    let calls = Calls::default();
    // `fake.set` is registered but the fake job knows no `fake.fail`; use an op the job calls reject by name.
    let source = "ops::fake::set(#{ path: \"a\", value: 1 }); ops::script::list(#{})";
    let error = work(json!({ "source": source }), json!({}), &calls, &Signal::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound, "the job's own answer reaches the caller: {error}");
    assert!(error.message.contains("script.list"));
    assert_eq!(error.hint.as_deref(), Some("the fake knows two ops"));
}

#[test]
fn work_runs_a_saved_macro_step_by_step() {
    let calls = Calls::default();
    let doc = json!({ "scripts": { "m": { "name": "M", "kind": "macro", "steps": [
        { "op": "fake.set", "args": { "path": "a", "value": 1 } },
        { "op": "fake.nope", "args": {} },
        { "op": "fake.set", "args": { "path": "b", "value": 2 } }
    ] } } });
    let error = work(json!({ "id": "m" }), doc, &calls, &Signal::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    assert!(error.message.starts_with("step 2 (`fake.nope`) failed"), "{}", error.message);
    assert_eq!(calls.sent.lock().unwrap().len(), 2, "the third step never ran");
}

#[test]
fn replay_refuses_a_step_a_script_could_not_call_before_sending_anything() {
    // A macro saved by an older build or edited by hand can hold what `script.save` now refuses.
    for (op, needle) in [("script.run", "another script run"), ("script.save", "outside the document")] {
        let calls = Calls::default();
        let doc = json!({ "scripts": { "loop": { "name": "Loop", "kind": "macro", "steps": [
            { "op": "fake.set", "args": { "path": "a", "value": 1 } },
            { "op": op, "args": { "id": "loop" } }
        ] } } });
        for dry_run in [false, true] {
            let error =
                work(json!({ "id": "loop", "dry_run": dry_run }), doc.clone(), &calls, &Signal::default()).unwrap_err();
            assert_eq!(error.code, ErrorCode::Unsupported, "{op}: {error}");
            assert!(error.message.starts_with(&format!("step 2 (`{op}`) failed")), "{op}: {}", error.message);
            assert!(error.message.contains(needle), "{op}: {}", error.message);
            assert!(error.hint.is_some(), "{op}: {error}");
        }
        assert!(calls.sent.lock().unwrap().is_empty(), "{op}: not even the first step was sent");
    }
}

#[test]
fn replay_holds_a_macro_to_the_call_limit() {
    let calls = Calls::default();
    let limit = Limits::job(None).calls as usize;
    let steps: Vec<Value> =
        (0..=limit).map(|_| json!({ "op": "fake.set", "args": { "path": "a", "value": 1 } })).collect();
    let doc = json!({ "scripts": { "long": { "name": "Long", "kind": "macro", "steps": steps } } });
    let error = work(json!({ "id": "long" }), doc, &calls, &Signal::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "{error}");
    assert!(error.message.contains(&format!("{} steps, over the {limit}-call limit", limit + 1)), "{error}");
    assert!(calls.sent.lock().unwrap().is_empty(), "the macro was refused before it started");
}

#[test]
fn replay_stops_at_the_seconds_it_was_given() {
    let calls = Calls { delay: std::time::Duration::from_millis(600), ..Calls::default() };
    let steps: Vec<Value> = (0..5).map(|_| json!({ "op": "fake.set", "args": { "path": "a", "value": 1 } })).collect();
    let doc = json!({ "scripts": { "slow": { "name": "Slow", "kind": "macro", "steps": steps } } });
    let error = work(json!({ "id": "slow", "seconds": 1 }), doc, &calls, &Signal::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Invalid, "{error}");
    assert!(error.message.contains("1-second limit"), "{error}");
    let sent = calls.sent.lock().unwrap().len();
    assert!((2..5).contains(&sent), "it stopped partway, after {sent} steps");
}

#[test]
fn the_ceiling_is_enforced_for_a_job_by_the_adapter() {
    let calls = Calls::default();
    let source = "// omuzaki: grants = [\"render\"]\nops::fake::set(#{ path: \"a\", value: 1 })";
    let error = work(json!({ "source": source }), json!({}), &calls, &Signal::default()).unwrap_err();
    assert_eq!(error.code, ErrorCode::Denied, "{error}");
    assert!(error.message.contains("`edit:character`"));
    assert!(calls.sent.lock().unwrap().is_empty(), "the denied call was not sent");
}

#[test]
fn work_without_a_plan_is_an_internal_error() {
    let op = decode("script.run", json!({ "source": "1" }));
    let actor = alice();
    let document = Doc(MemDoc::new());
    let signal = Signal::default();
    let ctx = JobCtx::new(JobId(1), &actor, &document, &signal);
    assert_eq!(op.work(&ctx).unwrap_err().code, ErrorCode::Internal);
}
