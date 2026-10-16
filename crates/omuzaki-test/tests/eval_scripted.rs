//! The scripted driver on demo ops over the doc mock: a script with a dry run, an apply, an undo and asserts passes; a
//! wrong assert fails with its line; twenty runs of one script agree byte for byte; the caps stop an attempt.
#![allow(clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;
use std::sync::Arc;

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use omuzaki_ops_mock::MockRunner;
use omuzaki_test::eval::driver::scripted::Scripted;
use omuzaki_test::eval::driver::{Attempt, Driver, Stop};
use omuzaki_test::eval::host::Host;
use omuzaki_test::eval::script::Script;
use omuzaki_test::eval::task::{Caps, Task};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetCounter {
    value: i64,
}
impl Op for SetCounter {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.set_counter",
            summary: "Test op.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let path = DocPath::parse("counter").unwrap();
        Ok(Plan::edit(Patch::new().with(path.clone(), ctx.doc.get(&path).cloned(), Some(json!(self.value)))))
    }
}
omuzaki_ops_api::register_op!(SetCounter);

/// A query standing in for a render: its answer is a function of the document, as a content-addressed picture is.
#[derive(Serialize, Deserialize, JsonSchema)]
struct Shot {}
impl Op for Shot {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.shot",
            summary: "Test op.",
            kind: OpKind::Query,
            scope: Scope::Render,
            undo: Undo::None,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let counter = ctx.doc.get(&DocPath::parse("counter").unwrap()).cloned();
        Ok(Plan::answer(json!({ "image": format!("omuzaki://render/{}.png", counter.unwrap_or(Value::Null)) })))
    }
}
omuzaki_ops_api::register_op!(Shot);

#[derive(Serialize, Deserialize, JsonSchema)]
struct Character {
    id: String,
}
impl Op for Character {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.character",
            summary: "Test op.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }
    fn plan(&self, _: &OpCtx<'_>) -> Result<Plan, OpError> {
        Ok(Plan::answer(json!({ "id": self.id })))
    }
}
omuzaki_ops_api::register_op!(Character);

fn runner() -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(json!({"counter": 1}))))
}

fn host(grants: Grants) -> Host {
    Host::with_runner(runner(), grants).unwrap()
}

fn task() -> Task {
    let head = "id = 'demo.counter'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    toml::from_str(&format!("{head}[[check]]\nid='c'\nkind='doc.exists'\npath='counter'\n")).unwrap()
}

fn caps() -> Caps {
    Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 }
}

fn play(script: &str, caps: &Caps, exports: &[(&str, &str)]) -> (Attempt, Value) {
    let exports: BTreeMap<String, String> = exports.iter().map(|(k, v)| ((*k).to_owned(), (*v).to_owned())).collect();
    let host = host(Grants::agent_default());
    let attempt = Scripted::new(Script::parse(script).unwrap(), exports).run(&task(), &host, caps).unwrap();
    let document = host.document().unwrap();
    host.finish().unwrap();
    (attempt, document)
}

const PASSING: &str = r#"
{"call": "evaltest_set_counter", "args": {"value": 9, "dry_run": true}, "expect": {"ok": true, "mode": "dry_run", "applied": false}, "save": "preview"}
{"assert": {"doc_same_as": "start"}}
{"call": "evaltest_set_counter", "args": {"value": 9}, "expect": {"ok": true, "mode": "apply", "applied": true}}
{"call": "history_undo", "args": {}}
{"assert": {"doc_same_as": "start"}}
{"call": "evaltest_set_counter", "args": {"value": 9}}
"#;

#[test]
fn a_script_with_a_dry_run_an_apply_an_undo_and_asserts_passes() {
    let (attempt, document) = play(PASSING, &caps(), &[]);
    assert_eq!(attempt.stop, Stop::Finished);
    assert_eq!(attempt.tool_calls, 4);
    assert_eq!(document["counter"], json!(9));
    let calls = attempt.log.transcript().calls;
    let modes: Vec<(String, bool, bool)> = calls.iter().map(|c| (c.op.clone(), c.ok, c.applied())).collect();
    assert_eq!(
        modes,
        [
            ("evaltest_set_counter".to_owned(), true, false),
            ("evaltest_set_counter".to_owned(), true, true),
            ("history_undo".to_owned(), true, false),
            ("evaltest_set_counter".to_owned(), true, true),
        ]
    );
}

#[test]
fn a_wrong_assert_fails_with_its_line_and_the_path_that_differs() {
    // After the apply the document no longer equals `start`: the assert on line 3 is wrong.
    let script = "{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": 9}}\n\n{\"assert\": {\"doc_same_as\": \"start\"}}\n{\"call\": \"history_undo\"}\n";
    let (attempt, _) = play(script, &caps(), &[]);
    let Stop::ScriptFailed { line, reason } = attempt.stop else { panic!("{:?}", attempt.stop) };
    assert_eq!(line, 3);
    assert!(reason.contains("`counter`"), "{reason}");
    assert_eq!(attempt.tool_calls, 1, "the script stops at the first failure");
}

#[test]
fn an_expect_that_does_not_hold_fails_with_its_line() {
    let script =
        "{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": 9}, \"expect\": {\"mode\": \"dry_run\"}}\n";
    let (attempt, _) = play(script, &caps(), &[]);
    let Stop::ScriptFailed { line, reason } = attempt.stop else { panic!("{:?}", attempt.stop) };
    assert_eq!(line, 1);
    assert!(reason.contains("expected"), "{reason}");
}

#[test]
fn a_tool_that_answers_with_an_error_is_an_answer_the_script_can_expect() {
    let script = "{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": \"nine\"}, \"expect\": {\"ok\": false}}\n";
    let (attempt, document) = play(script, &caps(), &[]);
    assert_eq!(attempt.stop, Stop::Finished);
    assert_eq!(document["counter"], json!(1));
    assert!(!attempt.log.transcript().calls[0].ok);
}

#[test]
fn render_same_as_reruns_the_call_and_compares_its_answer() {
    let same = "{\"call\": \"evaltest_shot\", \"args\": {}, \"save\": \"before\"}\n{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": 9}}\n{\"call\": \"history_undo\"}\n{\"assert\": {\"render_same_as\": \"before\"}}\n";
    let (attempt, _) = play(same, &caps(), &[]);
    assert_eq!(attempt.stop, Stop::Finished);
    assert_eq!(attempt.tool_calls, 4, "the re-run is a call of the transcript too");
    let different = "{\"call\": \"evaltest_shot\", \"args\": {}, \"save\": \"before\"}\n{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": 9}}\n{\"assert\": {\"render_same_as\": \"before\"}}\n";
    let (attempt, _) = play(different, &caps(), &[]);
    assert!(matches!(attempt.stop, Stop::ScriptFailed { line: 3, .. }), "{:?}", attempt.stop);
}

#[test]
fn render_same_as_needs_a_picture_both_times_so_two_identical_failures_do_not_pass() {
    // An answer without an image, and a call that is an error: both answer the same twice, and neither is a picture.
    let no_image = "{\"call\": \"evaltest_character\", \"args\": {\"id\": \"x\"}, \"save\": \"before\"}\n{\"assert\": {\"render_same_as\": \"before\"}}\n";
    let (attempt, _) = play(no_image, &caps(), &[]);
    let Stop::ScriptFailed { reason, .. } = attempt.stop else { panic!("{:?}", attempt.stop) };
    assert!(reason.contains("image"), "{reason}");
    let failing = "{\"call\": \"evaltest_shot\", \"args\": {\"nope\": 1}, \"save\": \"before\"}\n{\"assert\": {\"render_same_as\": \"before\"}}\n";
    let (attempt, _) = play(failing, &caps(), &[]);
    assert!(matches!(attempt.stop, Stop::ScriptFailed { line: 2, .. }), "{:?}", attempt.stop);
}

#[test]
fn names_the_fixture_exports_are_filled_in_and_a_misspelt_one_stops_the_script() {
    let script = "{\"call\": \"evaltest_character\", \"args\": {\"id\": \"{main}\"}, \"expect\": {\"output\": {\"id\": \"c-1\"}}}\n";
    let (attempt, _) = play(script, &caps(), &[("main", "c-1")]);
    assert_eq!(attempt.stop, Stop::Finished);
    let misspelt = script.replace("{main}", "{mian}");
    let (attempt, _) = play(&misspelt, &caps(), &[("main", "c-1")]);
    let Stop::ScriptFailed { line, reason } = attempt.stop else { panic!("{:?}", attempt.stop) };
    assert_eq!(line, 1);
    assert!(reason.contains("{mian}"), "{reason}");
    assert_eq!(attempt.tool_calls, 0, "nothing was sent with a name unfilled");
}

#[test]
fn the_caps_stop_an_attempt_and_say_which() {
    let calls = "{\"call\": \"evaltest_shot\"}\n{\"call\": \"evaltest_shot\"}\n{\"call\": \"evaltest_shot\"}\n";
    let (attempt, _) = play(calls, &Caps { tool_calls: 2, ..caps() }, &[]);
    assert_eq!((attempt.stop, attempt.tool_calls), (Stop::ToolCallCap, 2));
    let (attempt, _) = play(calls, &Caps { wall_s: 0, ..caps() }, &[]);
    assert_eq!((attempt.stop, attempt.tool_calls), (Stop::WallClock, 0));
}

#[test]
fn the_same_script_twenty_times_gives_byte_identical_transcripts_and_documents() {
    let (first, first_doc) = play(PASSING, &caps(), &[("main", "c-1")]);
    let first_log = first.log.to_jsonl(false).unwrap();
    assert!(first_log.contains("\"kind\":\"tool_result\""));
    for _ in 0..19 {
        let (attempt, document) = play(PASSING, &caps(), &[("main", "c-1")]);
        assert_eq!(attempt.log.to_jsonl(false).unwrap(), first_log);
        assert_eq!(document, first_doc);
    }
}

#[test]
fn a_transcript_promotes_to_a_script_that_replays() {
    let (attempt, _) = play(PASSING, &caps(), &[("main", "c-1")]);
    let promoted = Script::from_log(&attempt.log).to_jsonl().unwrap();
    assert_eq!(promoted.lines().count(), 4, "the calls only: asserts and expectations are not recorded");
    let (again, document) = play(&promoted, &caps(), &[("main", "c-1")]);
    assert_eq!(again.stop, Stop::Finished);
    assert_eq!(document["counter"], json!(9));
}

#[test]
fn the_server_serves_only_the_grants_of_the_task() {
    let host = host(Grants::of([Scope::Read]));
    let script =
        Script::parse("{\"call\": \"evaltest_set_counter\", \"args\": {\"value\": 9}, \"expect\": {\"ok\": false}}\n")
            .unwrap();
    let attempt = Scripted::new(script, BTreeMap::new()).run(&task(), &host, &caps()).unwrap();
    assert_eq!(attempt.stop, Stop::Finished);
    assert_eq!(host.document().unwrap()["counter"], json!(1));
}

/// A script against a real project: create a character as an agent would, with a preview first.
const REAL: &str = r#"
{"call": "character_create", "args": {"id": "aki", "name": "Aki", "template": "mini", "dry_run": true}, "expect": {"ok": true, "mode": "dry_run", "applied": false}}
{"assert": {"doc_same_as": "start"}}
{"call": "character_create", "args": {"id": "aki", "name": "Aki", "template": "mini"}, "expect": {"ok": true, "applied": true}}
"#;

fn real_project() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    omuzaki_doc::Project::create(dir.path(), "eval").unwrap();
    dir
}

fn play_real(host: &Host) -> Attempt {
    Scripted::new(Script::parse(REAL).unwrap(), BTreeMap::new()).run(&task(), host, &caps()).unwrap()
}

#[test]
fn the_in_process_host_serves_a_real_project_through_the_cli_constructor() {
    let dir = real_project();
    let host = Host::in_process(dir.path(), Grants::agent_default()).unwrap();
    let attempt = play_real(&host);
    assert_eq!(attempt.stop, Stop::Finished);
    assert!(host.document().unwrap()["characters"]["aki"].is_object());
    assert!(host.tool_names().op_of("character_create").is_some_and(|op| op == "character.create"));
    host.finish().unwrap();
    // Saved as you go: a fresh runner on the folder sees the character.
    let reopened = Host::in_process(dir.path(), Grants::agent_default()).unwrap();
    assert!(reopened.start_document()["characters"]["aki"].is_object());
}

/// `--binary`: the same script through `omuzaki mcp` as a child process. Skipped, with a message, unless `OMUZAKI_BIN`
/// names a built `omuzaki`.
#[test]
fn the_binary_host_passes_the_same_script() {
    let Some(bin) = std::env::var_os("OMUZAKI_BIN") else {
        eprintln!("skipped: OMUZAKI_BIN does not name a built `omuzaki`");
        return;
    };
    let dir = real_project();
    let host = Host::binary(std::path::Path::new(&bin), dir.path(), &Grants::agent_default()).unwrap();
    let pid = host.pid().unwrap();
    let attempt = play_real(&host);
    assert_eq!(attempt.stop, Stop::Finished);
    assert!(host.document().unwrap()["characters"]["aki"].is_object());
    host.finish().unwrap();
    // The child is gone once the host has finished: signal 0 asks only whether the process exists.
    let alive = std::process::Command::new("kill").args(["-0", &pid.to_string()]).output().unwrap().status.success();
    assert!(!alive, "the child {pid} outlived the attempt");
}

#[test]
fn the_binary_host_refuses_a_task_whose_grants_are_not_the_agent_default() {
    let error = Host::binary(
        std::path::Path::new("/nonexistent/omuzaki"),
        std::path::Path::new("."),
        &Grants::of([Scope::Read]),
    )
    .err()
    .unwrap();
    assert!(error.to_string().contains("agent default"), "{error}");
}

/// `eval promote` writes the recorded calls beside the task, once: it never replaces an existing script.
#[test]
fn promote_writes_a_script_beside_its_task_and_never_replaces_one() {
    let (attempt, _) = play(PASSING, &caps(), &[("main", "c-1")]);
    let root = tempfile::tempdir().unwrap();
    let evals = root.path().join("crates/c/evals");
    std::fs::create_dir_all(&evals).unwrap();
    let head = "id = 'demo.counter'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    std::fs::write(evals.join("demo.toml"), format!("{head}[[check]]\nid='c'\nkind='doc.exists'\npath='counter'\n"))
        .unwrap();
    let transcript = root.path().join("transcript.jsonl");
    std::fs::write(&transcript, attempt.log.to_jsonl(true).unwrap()).unwrap();
    let promote = |name: &str, task: &str| {
        common::run(
            &[
                "eval",
                "promote",
                common::arg(&transcript),
                "--task",
                task,
                "--as",
                name,
                "--expect",
                "pass",
                "--root",
                common::arg(root.path()),
            ],
            &[],
        )
    };
    let done = promote("run-1", "demo.counter");
    assert_eq!(common::code(&done), 0, "{}", common::text(&done.stderr));
    let script = std::fs::read_to_string(evals.join("demo.run-1.pass.jsonl")).unwrap();
    assert_eq!(script.lines().count(), 4);
    assert!(Script::parse(&script).is_ok());
    assert_eq!(common::code(&promote("run-1", "demo.counter")), 2, "an existing script is not replaced");
    assert_eq!(common::code(&promote("run-2", "no.such.task")), 2);
    assert_eq!(common::code(&promote("Bad Name", "demo.counter")), 2);
}

/// A child that starts and never says a word: the host gives up, says why, and the child does not outlive it.
#[cfg(unix)]
#[test]
fn a_binary_that_never_answers_ends_the_host_with_an_error_and_not_a_hang() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let silent = dir.path().join("silent");
    std::fs::write(&silent, "#!/bin/sh\nexec sleep 60\n").unwrap();
    std::fs::set_permissions(&silent, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let result =
        Host::binary_within(&silent, dir.path(), &Grants::agent_default(), std::time::Duration::from_millis(300));
    let Err(error) = result else { panic!("a silent child must not connect") };
    assert!(error.to_string().contains("did not complete within"), "{error}");
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
}
