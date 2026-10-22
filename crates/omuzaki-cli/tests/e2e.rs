//! End to end through the built binaries, against a temporary project folder.
//!
//! `omuzaki-fixture` is `omuzaki` with two demo ops linked in (the real binary has the product's ops, not these), so these tests run a real
//! op the way a person or an agent does: list the ops, dry-run, apply, and read the files. The history commands run in
//! a fresh process here, so the undo and redo of a session are tested over `omuzaki mcp`, the one command whose process
//! lives long enough to have a history. Every child runs under a timeout and is killed if it overstays, so a hung
//! binary fails the test instead of the build.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests write and read their fixtures on disk; the fence is for ops")]

mod common;

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use common::mcp::{Mcp, read_to_string_on_thread, wait_within};
use common::{new_project, project_toml};
use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_omuzaki-fixture");
const OMUZAKI: &str = env!("CARGO_BIN_EXE_omuzaki");

/// Longest any child process may take. Generous: a loaded CI machine is slow, a hang is infinite.
const LIMIT: Duration = Duration::from_secs(60);

/// The tools every MCP session has, whatever ops are linked.
const BUILTIN_TOOLS: [&str; 5] = ["history_redo", "history_undo", "tx_begin", "tx_commit", "tx_rollback"];

/// A command that ran to the end.
struct Done {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `bin [--project dir] args` to completion with nothing on stdin.
fn run(bin: &str, project: Option<&Path>, args: &[&str]) -> Done {
    run_in(bin, project, args, &[])
}

/// [`run`] with `env` set for the child.
fn run_in(bin: &str, project: Option<&Path>, args: &[&str], env: &[(&str, &Path)]) -> Done {
    let mut command = Command::new(bin);
    command.envs(env.iter().copied());
    if let Some(dir) = project {
        command.arg("--project").arg(dir);
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the binary");
    // Read both streams on their own threads: a full pipe must not stall the child while we wait for it to exit.
    let readers =
        [child.stdout.take().map(read_to_string_on_thread), child.stderr.take().map(read_to_string_on_thread)];
    let code = wait_within(&mut child, LIMIT, &format!("{bin} {args:?}")).code();
    let [stdout, stderr] = readers.map(|r| r.map(|handle| handle.join().expect("reader thread")).unwrap_or_default());
    Done { code, stdout, stderr }
}

fn fixture(project: &Path, args: &[&str]) -> Done {
    run(FIXTURE, Some(project), args)
}

#[test]
fn ops_lists_the_linked_ops_and_help_documents_their_arguments() {
    let table = run(FIXTURE, None, &["ops"]);
    assert_eq!(table.code, Some(0), "{}", table.stderr);
    for line in ["demo.get_note", "demo.set_note"] {
        assert!(table.stdout.contains(line), "{line:?} missing from:\n{}", table.stdout);
    }

    let json = run(FIXTURE, None, &["ops", "--json"]);
    let rows: Value = serde_json::from_str(&json.stdout).expect("ops --json is JSON");
    let names: Vec<&str> = rows.as_array().unwrap().iter().map(|r| r["name"].as_str().unwrap()).collect();
    let demo: Vec<&str> = names.iter().copied().filter(|name| name.starts_with("demo.")).collect();
    assert_eq!(demo, ["demo.get_note", "demo.get_project", "demo.set_field", "demo.set_note"]);

    let help = run(FIXTURE, None, &["help", "demo.set_note"]);
    assert_eq!(help.code, Some(0), "{}", help.stderr);
    assert!(help.stdout.contains("text") && help.stdout.contains("The note's text."), "{}", help.stdout);
}

#[test]
fn a_dry_run_shows_the_change_and_writes_nothing() {
    let project = new_project();
    let before = project_toml(project.path());
    let out = fixture(project.path(), &["run", "demo.set_note", "--args", r#"{"text": "hello"}"#, "--dry-run"]);
    assert_eq!(out.code, Some(0), "{}", out.stderr);
    for line in ["dry run of demo.set_note: nothing was changed", "+ project/note", "+ \"hello\""] {
        assert!(out.stdout.contains(line), "{line:?} missing from:\n{}", out.stdout);
    }
    assert_eq!(project_toml(project.path()), before);
}

#[test]
fn applying_saves_the_project_and_the_next_process_reads_it_back() {
    let project = new_project();
    let applied = fixture(project.path(), &["run", "demo.set_note", "--args", r#"{"text": "hello"}"#]);
    assert_eq!(applied.code, Some(0), "{}", applied.stderr);
    assert!(applied.stdout.starts_with("applied demo.set_note"), "{}", applied.stdout);
    assert!(project_toml(project.path()).contains("note = \"hello\""), "{}", project_toml(project.path()));

    let read = fixture(project.path(), &["--json", "run", "demo.get_note"]);
    assert_eq!(read.code, Some(0), "{}", read.stderr);
    let outcome: Value = serde_json::from_str(&read.stdout).expect("--json output");
    assert_eq!(outcome["plan"]["output"]["note"], "hello");
    assert!(outcome["applied"].is_null(), "a query logs nothing");
}

#[test]
fn undo_and_redo_in_a_separate_process_reach_the_last_ones_history() {
    let project = new_project();
    let original = project_toml(project.path());
    fixture(project.path(), &["run", "demo.set_note", "--args", r#"{"text": "hello"}"#]);
    let saved = project_toml(project.path());
    assert_ne!(saved, original);

    // The history is journalled and restored on open, so a new process undoes what the last one did.
    let undone = fixture(project.path(), &["undo"]);
    assert_eq!(undone.code, Some(0), "{}", undone.stderr);
    assert!(undone.stdout.starts_with("undid "), "{}", undone.stdout);
    assert_eq!(project_toml(project.path()), original, "the files follow the undo before the process exits");
    let redone = fixture(project.path(), &["redo"]);
    assert!(redone.stdout.starts_with("redid "), "{}", redone.stdout);
    assert_eq!(project_toml(project.path()), saved);
    let nothing = fixture(project.path(), &["redo"]);
    assert_eq!((nothing.code, nothing.stdout.as_str()), (Some(0), "nothing to redo\n"), "{}", nothing.stderr);
}

#[test]
fn history_lists_restores_and_names_across_processes() {
    let project = new_project();
    let set =
        |text: &str| fixture(project.path(), &["run", "demo.set_note", "--args", &json!({"text": text}).to_string()]);
    set("first");
    set("second");

    let listed = fixture(project.path(), &["--json", "history", "list"]);
    assert_eq!(listed.code, Some(0), "{}", listed.stderr);
    let page: Value = serde_json::from_str(&listed.stdout).expect("--json output");
    let txs = page["plan"]["output"]["transactions"].as_array().expect("transactions").clone();
    assert_eq!(txs.len(), 2, "{page}");
    let first = txs[1]["tx"].as_u64().unwrap().to_string();

    let diff = fixture(project.path(), &["--json", "history", "diff", &first]);
    let diff: Value = serde_json::from_str(&diff.stdout).expect("--json output");
    assert_eq!(diff["plan"]["output"]["changes"], 1, "{diff}");

    let restored = fixture(project.path(), &["history", "restore", &first]);
    assert_eq!(restored.code, Some(0), "{}", restored.stderr);
    assert!(project_toml(project.path()).contains("note = \"first\""), "{}", project_toml(project.path()));
    // One undoable step, from another process.
    fixture(project.path(), &["undo"]);
    assert!(project_toml(project.path()).contains("note = \"second\""), "{}", project_toml(project.path()));

    let named = fixture(project.path(), &["history", "name", "first look", "--at", &first]);
    assert_eq!(named.code, Some(0), "{}", named.stderr);
    let again = fixture(project.path(), &["history", "name", "first look"]);
    assert_ne!(again.code, Some(0), "a name is unique: {}", again.stdout);
    let page: Value = serde_json::from_str(&fixture(project.path(), &["--json", "history", "list"]).stdout).unwrap();
    let versions: Vec<&Value> = page["plan"]["output"]["transactions"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["names"].as_array().unwrap())
        .collect();
    assert_eq!(versions, [&json!("first look")], "{page}");
}

/// `io.import` in the product: the command line gives ops the person's asset store (the studio's), so the model's
/// bytes land there and the project records the asset by hash.
#[test]
fn io_import_stores_the_model_in_the_person_s_asset_store() {
    let project = new_project();
    let data = tempfile::tempdir().expect("temp dir");
    std::fs::write(project.path().join("model.glb"), omuzaki_io_mock::fixtures::tiny_glb()).expect("write the model");
    let env = [("OMUZAKI_DATA_DIR", data.path()), ("OMUZAKI_CONFIG_DIR", data.path())];
    let args = json!({"path": "model.glb"}).to_string();
    let done = run_in(OMUZAKI, Some(project.path()), &["--json", "run", "io.import", "--args", &args], &env);
    assert_eq!(done.code, Some(0), "{}\n{}", done.stdout, done.stderr);
    let reply: Value = serde_json::from_str(&done.stdout).expect("--json output");
    let hash =
        reply["plan"]["output"]["asset"]["original"].as_str().or_else(|| reply["output"]["asset"]["original"].as_str());
    let hash = hash.unwrap_or_else(|| panic!("the asset's hash: {reply}"));
    let objects = data.path().join("store").join("objects");
    assert!(objects.join(&hash[..2]).is_dir(), "the original is in the person's store: {hash}");
    assert!(walk_text(project.path()).contains(hash), "the project records the asset by its hash");
}

/// The command line works in the person's settings: a value set in one process is there in the next, in the file the
/// studio reads (`machine.toml` for a machine key).
#[test]
fn a_setting_changed_from_the_command_line_is_the_persons_in_the_next_process() {
    let project = new_project();
    let folders = tempfile::tempdir().expect("temp dir");
    let env = [("OMUZAKI_DATA_DIR", folders.path()), ("OMUZAKI_CONFIG_DIR", folders.path())];
    let args = json!({"key": "obs.port", "value": 4460}).to_string();
    let set = run_in(OMUZAKI, Some(project.path()), &["run", "settings.set", "--args", &args], &env);
    assert_eq!(set.code, Some(0), "{}\n{}", set.stdout, set.stderr);
    let read = json!({"key": "obs.port"}).to_string();
    let got = run_in(OMUZAKI, Some(project.path()), &["--json", "run", "settings.get", "--args", &read], &env);
    let reply: Value = serde_json::from_str(&got.stdout).expect("--json output");
    assert_eq!(reply["plan"]["output"]["value"], 4460, "{reply}");
    let machine = std::fs::read_to_string(folders.path().join("machine.toml")).expect("machine.toml");
    assert!(machine.contains("4460"), "{machine}");
}

/// Every TOML file's text under `dir`, the app's own folder left out.
fn walk_text(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).expect("read a folder").flatten() {
        let path = entry.path();
        if path.file_name().is_some_and(|name| name == ".omuzaki") {
            continue;
        }
        if path.is_dir() {
            text.push_str(&walk_text(&path));
        } else if path.extension().is_some_and(|ext| ext == "toml") {
            text.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
        }
    }
    text
}

#[test]
fn a_bad_call_exits_with_the_documented_code_and_leaves_the_project_alone() {
    let project = new_project();
    let before = project_toml(project.path());
    let unknown_op = fixture(project.path(), &["run", "demo.nope"]);
    assert_eq!(unknown_op.code, Some(3), "{}", unknown_op.stderr);
    assert!(unknown_op.stderr.contains("field: op"), "{}", unknown_op.stderr);

    let wrong_type = fixture(project.path(), &["run", "demo.set_note", "--args", r#"{"text": 5}"#]);
    assert_eq!(wrong_type.code, Some(3), "{}", wrong_type.stderr);
    assert_eq!(project_toml(project.path()), before);
}

// ---- omuzaki mcp ----------------------------------------------------------------------------------------------------

#[test]
fn the_real_binary_answers_initialize_and_tools_list_over_stdio() {
    let project = new_project();
    let mut mcp = Mcp::start(OMUZAKI, project.path());
    let info = mcp.handshake();
    assert_eq!(info["serverInfo"]["name"], "omuzaki");
    assert!(info["capabilities"]["tools"].is_object(), "{info}");
    // The product's ops come with the built-in tools; no demo op is in the real binary.
    let tools = mcp.tool_names();
    for tool in BUILTIN_TOOLS.into_iter().chain(["character_create", "face_set_style", "io_inspect"]) {
        assert!(tools.iter().any(|name| name == tool), "the real binary lacks tool {tool}: {tools:?}");
    }
    assert!(!tools.iter().any(|name| name.starts_with("demo_")), "{tools:?}");
    assert_eq!(mcp.close(), (Some(0), String::new()), "a client that hangs up cleanly ends the server cleanly");
}

#[test]
fn by_default_the_list_is_a_small_core_that_reaches_every_op_and_grant_adds_scopes() {
    let project = new_project();
    let mut mcp = Mcp::start_with(OMUZAKI, project.path(), &[], &["--grant", "perform,broadcast"]);
    let info = mcp.handshake();
    assert_eq!(info["capabilities"]["tools"]["listChanged"], true, "{info}");
    let tools = mcp.tool_names();
    for tool in BUILTIN_TOOLS.into_iter().chain(["ops_find", "ops_describe", "ops_run", "tools_enable", "doc_get"]) {
        assert!(tools.iter().any(|name| name == tool), "the core list lacks {tool}: {tools:?}");
    }
    assert!(tools.len() < 25 && !tools.iter().any(|t| t == "character_create"), "{tools:?}");
    let found = mcp.call("ops_find", &json!({"query": "create character"}));
    assert!(found["ops"].as_array().unwrap().iter().any(|op| op["op"] == "character.create"), "{found}");
    let preview = mcp.call("ops_run", &json!({"op": "character.create", "args": {"id": "kit", "name": "Kit", "template": "standard", "seed": 7}, "dry_run": true}));
    assert_eq!(preview["mode"], "dry_run", "{preview}");
    let me = mcp.call("session_info", &json!({}));
    let scopes = me.to_string();
    assert!(scopes.contains("\"perform\"") && scopes.contains("\"broadcast\""), "--grant reaches the session: {me}");
    assert_eq!(mcp.close().0, Some(0));
}

#[test]
fn an_mcp_session_dry_runs_applies_undoes_and_redoes_and_the_files_follow() {
    let project = new_project();
    let original = project_toml(project.path());
    let noted = |text: &str| format!("note = \"{text}\"");
    let mut mcp = Mcp::start(FIXTURE, project.path());
    mcp.handshake();

    // The fixture links the same product crates as `omuzaki` (they hang off the library), plus the demo pair.
    let tools = mcp.tool_names();
    for tool in BUILTIN_TOOLS.into_iter().chain(["demo_get_note", "demo_set_note"]) {
        assert!(tools.iter().any(|name| name == tool), "the fixture lacks tool {tool}: {tools:?}");
    }

    let preview = mcp.call("demo_set_note", &json!({"text": "hello", "dry_run": true}));
    assert_eq!(preview["mode"], "dry_run", "{preview}");
    assert_eq!(project_toml(project.path()), original, "a dry run writes nothing");

    // A change is acknowledged once the journal holds it; the files follow within the keeper's two seconds.
    let applied = mcp.call("demo_set_note", &json!({"text": "hello"}));
    assert_eq!(applied["mode"], "apply", "{applied}");
    files_come_to(project.path(), |toml| toml.contains(&noted("hello")));
    assert_eq!(mcp.call("demo_get_note", &json!({}))["output"]["note"], "hello");

    mcp.call("history_undo", &json!({}));
    files_come_to(project.path(), |toml| toml == original);
    assert!(mcp.call("demo_get_note", &json!({}))["output"]["note"].is_null());

    mcp.call("history_redo", &json!({}));
    assert_eq!(mcp.close(), (Some(0), String::new()), "a session logs nothing to stderr either");
    assert!(project_toml(project.path()).contains(&noted("hello")), "closing saves: {}", project_toml(project.path()));
}

#[test]
fn logging_goes_to_stderr_and_never_into_the_protocol_on_stdout() {
    let project = new_project();
    let mut mcp = Mcp::start_with_env(FIXTURE, project.path(), &[("RUST_LOG", "debug")]);
    mcp.handshake();
    mcp.call("demo_set_note", &json!({"text": "hello"}));
    // `close` fails the test if any stdout line isn't a JSON-RPC message.
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0));
    assert!(!stderr.trim().is_empty(), "RUST_LOG=debug should log something to stderr");
}

#[test]
fn a_client_that_leaves_before_the_handshake_ends_the_server_with_an_internal_error() {
    let project = new_project();
    // Nothing is sent: standard input closes straight away, and nothing may be printed on stdout.
    let (code, stderr) = Mcp::start(FIXTURE, project.path()).close();
    assert_eq!(code, Some(1));
    assert!(stderr.starts_with("error: internal: MCP handshake failed"), "{stderr}");
}

/// Waits until `project.toml` satisfies `done`, at most ten seconds: the keeper saves behind the journal, so the file
/// trails an acknowledged change by up to its save interval.
fn files_come_to(dir: &std::path::Path, done: impl Fn(&str) -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !done(&project_toml(dir)) {
        assert!(std::time::Instant::now() < deadline, "the files never followed: {}", project_toml(dir));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn omuzaki_mcp_bridges_into_a_studio_that_holds_the_project_after_the_person_pairs_it() {
    omuzaki_cli::link();
    let (project, data) = (new_project(), tempfile::tempdir().unwrap());
    // This test plays the studio: it holds the project and takes agents on its port.
    let person = omuzaki_ops_api::Actor::Person { name: "pat".into() };
    let studio = omuzaki_app::AppCore::open(project.path(), omuzaki_app::AppOptions::studio(person.clone())).unwrap();
    studio.serve_http(omuzaki_mcp::HttpOptions::default().with_bind("127.0.0.1:0".parse().unwrap())).unwrap();
    let events = studio.subscribe();

    let data_dir = data.path().to_str().unwrap();
    let env = [("OMUZAKI_DATA_DIR", data_dir), ("OMUZAKI_CONFIG_DIR", data_dir)];
    let mut mcp = Mcp::start_with_env(OMUZAKI, project.path(), &env);
    mcp.handshake();
    let sessions = studio.sessions();
    let deadline = Instant::now() + LIMIT;
    while sessions.approvals().is_empty() {
        assert!(Instant::now() < deadline, "no pairing card");
        thread::yield_now();
    }
    let card = sessions.approvals().remove(0);
    assert_eq!(card.need, omuzaki_ops_api::Need::Pairing { client: "e2e".into() });
    sessions.resolve(card.id, omuzaki_ops_api::Decision::Approve, &person).unwrap();

    // The agent's op runs in the studio's executor, in the agent's name.
    mcp.call("character_create", &json!({"id": "mia", "name": "Mia", "template": "soft-sd"}));
    let applied = loop {
        let event = serde_json::to_value(events.recv_timeout(LIMIT).expect("an event")).unwrap();
        if event["event"] == "op_applied" {
            break event;
        }
    };
    assert_eq!(applied["record"]["op"], "character.create", "{applied}");
    let actor: omuzaki_ops_api::Actor = serde_json::from_value(applied["record"]["actor"].clone()).unwrap();
    assert!(matches!(&actor, omuzaki_ops_api::Actor::Agent { id, .. } if id.starts_with("e2e#")), "{actor:?}");
    let (code, stderr) = mcp.close();
    assert_eq!(code, Some(0), "{stderr}");
    assert!(stderr.contains("Pair \"e2e\""), "the person is told to pair the client: {stderr}");
    studio.close().unwrap();
    // The test reads the project's files as a person would, not as an op.
    #[allow(clippy::disallowed_methods)]
    let saved = std::fs::read_to_string(project.path().join("characters").join("mia.toml"));
    assert!(saved.is_ok_and(|text| text.contains("Mia")), "the studio saved the agent's character");
}
