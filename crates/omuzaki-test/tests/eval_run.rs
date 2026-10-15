//! The runner end to end on demo ops linked into this test binary: tasks play on real project folders opened by
//! `omuzaki-cli`'s constructor, are graded on separate runners, and leave a history, a run record and evidence folders.
#![allow(clippy::unwrap_used)]

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use omuzaki_test::eval::history::{self, AttemptOutcome, AttemptRecord, HISTORY_FILE, RUNS_FILE};
use omuzaki_test::eval::provenance::PrivateList;
use omuzaki_test::eval::run::{Options, RunReport, Selection, run_scripted};
use omuzaki_test::eval::task::Caps;
use omuzaki_test::eval::{self};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Where the demo ops keep their note: a key in `project.toml`.
fn note_path() -> DocPath {
    DocPath::root().join("project").join("note")
}

#[derive(Serialize, Deserialize, JsonSchema)]
struct SetNote {
    text: String,
}
impl Op for SetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "evaltest.set_note",
            summary: "Set the project's note.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }
    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = note_path();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.text)))))
    }
}
omuzaki_ops_api::register_op!(SetNote);

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

const FIXTURE: &str = "id = 'fx'\nsummary = 's'\nsteps = [{ op = 'evaltest.set_note', args = { text = 'start' } }]\n";

/// A task that sets the note to `want`, with a pass script that does and a fail script that sets `wrong`.
fn task(ws: &Path, name: &str, want: &str, wrong: &str) {
    let dir = ws.join("crates/c/evals");
    write(
        &dir.join(format!("{name}.toml")),
        &format!(
            "id = 'demo.{name}'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'Make the note say {want}.'\nfixture = 'fx'\n\
             scopes = ['read', 'edit:scene']\n\
             [[check]]\nid = 'note'\nkind = 'doc.equals'\npath = 'project/note'\nvalue = '{want}'\n\
             [[check]]\nid = 'only-the-note'\nkind = 'doc.changed_only'\npaths = ['project/note']\n"
        ),
    );
    let call = |text: &str| format!("{{\"call\": \"evaltest_set_note\", \"args\": {{\"text\": \"{text}\"}}}}\n");
    write(&dir.join(format!("{name}.good.pass.jsonl")), &call(want));
    write(&dir.join(format!("{name}.nope.fail.jsonl")), &call(wrong));
}

fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    write(&ws.path().join("crates/c/evals/fixtures/fx.toml"), FIXTURE);
    task(ws.path(), "up", "up", "nope");
    task(ws.path(), "down", "down", "nope");
    ws
}

fn caps() -> Caps {
    Caps { turns: 10, tool_calls: 20, wall_s: 60, tokens: 1000 }
}

struct Folders {
    out: PathBuf,
    status: PathBuf,
    scratch: PathBuf,
}

impl Folders {
    fn new(root: &Path) -> Self {
        Self { out: root.join("out"), status: root.join("status"), scratch: root.join("scratch") }
    }
}

fn play(ws: &Path, folders: &Folders, run_id: &str, attempts: u32, jobs: usize, record: bool) -> RunReport {
    play_with(ws, folders, run_id, Selection::default(), attempts, jobs, record)
}

fn play_with(
    ws: &Path,
    folders: &Folders,
    run_id: &str,
    select: Selection,
    attempts: u32,
    jobs: usize,
    record: bool,
) -> RunReport {
    let discovery = eval::discover(ws).unwrap();
    let private = PrivateList { prefixes: vec!["example-clip/".to_owned()], ..PrivateList::default() };
    let no_items = |item: &eval::fixture::SetItemRef| Err(format!("no store for {}", item.set));
    let options = Options {
        select,
        attempts,
        jobs,
        binary: None,
        kind: "manual".to_owned(),
        run_id: run_id.to_owned(),
        commit: "abc1234".to_owned(),
        out: folders.out.clone(),
        status: record.then(|| folders.status.clone()),
        scratch: folders.scratch.clone(),
        item_path: &no_items,
        store: None,
    };
    run_scripted(&discovery, caps(), &private, &options).unwrap()
}

#[test]
fn two_tasks_three_attempts_recorded_give_six_history_lines_and_a_run_record() {
    let ws = workspace();
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 3, 4, true);
    assert!(report.ok(), "{:?}", report.deviations);

    let history = history::read_attempts(&folders.status.join(HISTORY_FILE)).unwrap();
    assert_eq!(history.len(), 6);
    assert!(history.iter().all(|r| r.outcome == AttemptOutcome::Pass && r.family == "task" && r.v == 1));
    let keys: Vec<(&str, u32)> = history.iter().map(|r| (r.task.as_str(), r.attempt)).collect();
    assert_eq!(
        keys,
        [("demo.down", 1), ("demo.down", 2), ("demo.down", 3), ("demo.up", 1), ("demo.up", 2), ("demo.up", 3)]
    );
    let first = &history[0];
    assert_eq!(first.checks.len(), 2);
    assert_eq!(first.checks[0].id, "note");
    assert_eq!((first.checks[0].passed, first.checks[0].after.clone()), (Some(true), Some(json!("down"))));
    assert_eq!((first.tool_calls, first.tool_errors, first.turns, first.usd_micros), (1, 0, 0, 0));
    assert_eq!(first.commit, "abc1234");
    assert_eq!(first.transcript, "evals/runs/2026-10-13T1000Z-manual/demo.down/1/");
    assert!(
        first.tools_hash.starts_with("b3:")
            && first.prompt_hash.starts_with("b3:")
            && first.task_hash.starts_with("b3:")
    );

    let runs = history::read_runs(&folders.status.join(RUNS_FILE)).unwrap();
    assert_eq!(runs.len(), 1);
    let run = &runs[0];
    assert_eq!((run.attempts.pass, run.attempts.total(), run.valid), (6, 6, true));
    assert_eq!(run.tasks_run, ["demo.down", "demo.up"]);
    // Per task: the fail script and the empty script, each of which must fail.
    assert_eq!((run.oracles.played, run.oracles.deviations), (4, 0));

    // Evidence for every attempt and oracle; nothing left in the scratch.
    let task_dir = folders.out.join("runs/2026-10-13T1000Z-manual/demo.up");
    for folder in ["1", "2", "3", "oracle-nope-fail", "oracle-null"] {
        assert!(task_dir.join(folder).join("transcript.jsonl").is_file(), "{folder}");
        assert!(task_dir.join(folder).join("checks.json").is_file(), "{folder}");
    }
    assert!(!folders.scratch.join("2026-10-13T1000Z-manual").exists(), "the scratch is gone");
}

#[test]
fn nothing_is_recorded_without_record() {
    let ws = workspace();
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 1, 2, false);
    assert!(report.ok());
    assert!(!folders.status.exists());
    assert_eq!(report.attempts.len(), 2);
}

#[test]
fn the_order_of_the_history_does_not_depend_on_how_many_jobs_run_at_once() {
    let ws = workspace();
    let lines = |jobs: usize| -> Vec<AttemptRecord> {
        let tmp = tempfile::tempdir().unwrap();
        let folders = Folders::new(tmp.path());
        play(ws.path(), &folders, "2026-10-13T1000Z-manual", 2, jobs, true);
        let mut records = history::read_attempts(&folders.status.join(HISTORY_FILE)).unwrap();
        for record in &mut records {
            record.wall_ms = 0;
        }
        records
    };
    assert_eq!(lines(1), lines(4));
}

#[test]
fn an_oracle_that_does_not_end_as_it_must_fails_the_run() {
    let ws = workspace();
    // The "fail" script now does the right thing: the task cannot tell it from the pass script.
    write(
        &ws.path().join("crates/c/evals/up.nope.fail.jsonl"),
        "{\"call\": \"evaltest_set_note\", \"args\": {\"text\": \"up\"}}\n",
    );
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play_with(
        ws.path(),
        &folders,
        "2026-10-13T1000Z-manual",
        Selection { tasks: vec!["demo.up".to_owned()], ..Selection::default() },
        1,
        1,
        true,
    );
    assert!(!report.ok());
    let [deviation] = report.deviations.as_slice() else { panic!("{:?}", report.deviations) };
    assert_eq!(
        (deviation.task.as_str(), deviation.expected, deviation.got),
        ("demo.up", AttemptOutcome::Fail, AttemptOutcome::Pass)
    );
    assert_eq!(report.run.oracles.deviations, 1);
    // The oracle is never in the history: a script that must fail would lower the pass rate.
    assert_eq!(history::read_attempts(&folders.status.join(HISTORY_FILE)).unwrap().len(), 1);
}

#[test]
fn a_script_that_cannot_run_is_an_error_attempt_and_too_many_make_the_run_invalid() {
    let ws = workspace();
    write(
        &ws.path().join("crates/c/evals/up.good.pass.jsonl"),
        // The script expects a preview and gets an apply: the script is wrong, which is the harness's trouble.
        "{\"call\": \"evaltest_set_note\", \"args\": {\"text\": \"up\"}, \"expect\": {\"mode\": \"dry_run\"}}\n",
    );
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 1, 2, true);
    assert!(!report.ok());
    let up = report.attempts.iter().find(|a| a.task == "demo.up").unwrap();
    assert_eq!(up.outcome, AttemptOutcome::Error);
    assert!(up.reason.as_deref().unwrap().contains("line 1"), "{:?}", up.reason);
    // One of two recorded attempts errored: more than 10%.
    assert!(!report.run.valid);
    let runs = history::read_runs(&folders.status.join(RUNS_FILE)).unwrap();
    assert!(!runs[0].valid);
}

#[test]
fn a_caps_override_reaches_the_attempt() {
    let ws = workspace();
    // Two calls where one is allowed: the cap stops the attempt, which is a fail.
    write(
        &ws.path().join("crates/c/evals/up.good.pass.jsonl"),
        "{\"call\": \"evaltest_set_note\", \"args\": {\"text\": \"up\"}}\n{\"call\": \"evaltest_set_note\", \"args\": {\"text\": \"up\"}}\n",
    );
    let toml = fs::read_to_string(ws.path().join("crates/c/evals/up.toml")).unwrap();
    write(
        &ws.path().join("crates/c/evals/up.toml"),
        &toml.replacen("[[check]]", "[caps]\ntool_calls = 1\n[[check]]", 1),
    );
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play_with(
        ws.path(),
        &folders,
        "2026-10-13T1000Z-manual",
        Selection { tasks: vec!["demo.up".to_owned()], ..Selection::default() },
        1,
        1,
        false,
    );
    let up = &report.attempts[0];
    assert_eq!(up.outcome, AttemptOutcome::Fail);
    assert!(up.reason.as_deref().unwrap().contains("tool-call cap"), "{:?}", up.reason);
}

#[test]
fn a_task_whose_fixture_is_on_the_private_list_is_refused_before_any_attempt() {
    let ws = workspace();
    write(
        &ws.path().join("crates/c/evalsets/clips.toml"),
        "id = 'clips'\nkind = 'synthetic'\nsplit = 'dev'\nsummary = 's'\n[[item]]\nhash = 'b3:aa'\npath = 'example-clip/a.png'\n\
         [item.provenance]\nsource = 'synthetic'\ncommercial = 'yes'\ngenerator = 'abc1234'\n",
    );
    write(
        &ws.path().join("crates/c/evals/fixtures/private.toml"),
        "id = 'private'\nsummary = 's'\nsteps = [{ op = 'evaltest.set_note', args = { text = { set = 'clips', item = 'b3:aa' } } }]\n",
    );
    task(ws.path(), "clip", "up", "nope");
    let clip = fs::read_to_string(ws.path().join("crates/c/evals/clip.toml")).unwrap();
    write(&ws.path().join("crates/c/evals/clip.toml"), &clip.replace("fixture = 'fx'", "fixture = 'private'"));
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 1, 2, true);
    assert!(!report.ok());
    let [refused] = report.refused.as_slice() else { panic!("{:?}", report.refused) };
    assert_eq!(refused.task, "demo.clip");
    assert!(refused.why.contains("private list"), "{}", refused.why);
    assert!(report.attempts.iter().all(|a| a.task != "demo.clip"), "no attempt of a refused task");
    assert!(!folders.out.join("runs/2026-10-13T1000Z-manual/demo.clip").exists());
    // The other two tasks still ran, and the refused task is in the run record with why.
    assert_eq!(report.run.tasks_run, ["demo.down", "demo.up"]);
    assert_eq!(report.run.tasks_skipped.len(), 1);
}

#[test]
fn a_fixture_that_does_not_build_refuses_its_tasks() {
    let ws = workspace();
    write(
        &ws.path().join("crates/c/evals/fixtures/fx.toml"),
        "id = 'fx'\nsummary = 's'\nsteps = [{ op = 'evaltest.no_such_op' }]\n",
    );
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 1, 1, false);
    assert!(!report.ok());
    assert_eq!(report.refused.len(), 2);
    assert!(report.refused[0].why.contains("did not build"), "{}", report.refused[0].why);
    assert_eq!(report.attempts.len(), 0);
}

#[test]
fn selectors_add_up() {
    let ws = workspace();
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let one = Selection { tasks: vec!["demo.d*".to_owned()], ..Selection::default() };
    let report = play_with(ws.path(), &folders, "2026-10-13T1000Z-manual", one, 1, 1, false);
    assert_eq!(report.run.tasks_run, ["demo.down"]);
    let both = Selection { tasks: vec!["demo.d*".to_owned()], crates: vec!["c".to_owned()], ..Selection::default() };
    let report = play_with(ws.path(), &folders, "2026-10-13T1000Z-second", both, 1, 1, false);
    assert_eq!(report.run.tasks_run, ["demo.down", "demo.up"]);
}

/// `eval run` and `eval gc` through the binary, on the sample task that ships with the harness and its real ops.
#[test]
fn the_command_line_plays_the_sample_task_and_gc_keeps_what_an_issue_names() {
    let ws = tempfile::tempdir().unwrap();
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("evals");
    let target = ws.path().join("crates/omuzaki-test/evals");
    omuzaki_test::eval::build::copy_dir(&source, &target).unwrap();
    // The one private-material list, as the loader finds it.
    let list = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/licensing/private-material.toml");
    write(&ws.path().join("docs/licensing/private-material.toml"), &fs::read_to_string(list).unwrap());
    let tmp = tempfile::tempdir().unwrap();
    let (out, status) = (tmp.path().join("out"), tmp.path().join("status"));
    let envs = [("OMUZAKI_EVAL_OUT", common::arg(&out)), ("OMUZAKI_EVAL_STATUS", common::arg(&status))];
    let root = common::arg(ws.path());
    let old = "2026-01-01T0000Z-manual";
    let run = |args: &[&str]| common::run(&[&["eval"], args, &["--root", root]].concat(), &envs);

    let done = run(&["run", "--attempts", "2", "--record", "--run-id", old]);
    assert_eq!(common::code(&done), 0, "{}{}", common::text(&done.stdout), common::text(&done.stderr));
    assert!(common::text(&done.stdout).contains("2 attempts (2 pass"), "{}", common::text(&done.stdout));
    assert_eq!(history::read_attempts(&status.join(HISTORY_FILE)).unwrap().len(), 2);

    // An old run folder: kept while an issue body names it, gone once nothing does.
    let issues = tmp.path().join("issues.txt");
    fs::write(&issues, format!("Eval regression: transcripts in {old}/sample.rename-character/1")).unwrap();
    let kept = run(&["gc", "--apply", "--issues", common::arg(&issues)]);
    assert_eq!(common::code(&kept), 0, "{}", common::text(&kept.stderr));
    assert!(common::text(&kept.stdout).contains("keep   2026-01-01T0000Z-manual: named in an open issue"));
    assert!(out.join("runs").join(old).is_dir());
    fs::write(&issues, "nothing here").unwrap();
    let dry = run(&["gc", "--issues", common::arg(&issues)]);
    assert!(common::text(&dry.stdout).contains("remove 2026-01-01T0000Z-manual"), "{}", common::text(&dry.stdout));
    assert!(out.join("runs").join(old).is_dir(), "a dry run removes nothing");
    let applied = run(&["gc", "--apply", "--issues", common::arg(&issues)]);
    assert_eq!(common::code(&applied), 0);
    assert!(!out.join("runs").join(old).exists());

    // The reference driver is a later task, and `gh` failing keeps everything (no issues file, no usable `gh`).
    let reference = run(&["run", "--driver", "reference"]);
    assert_eq!(common::code(&reference), 2);
    assert!(common::text(&reference.stderr).contains("reference"), "{}", common::text(&reference.stderr));
}

#[test]
fn a_run_id_that_is_not_a_plain_folder_name_or_is_taken_is_refused_before_anything_is_written() {
    let ws = workspace();
    let tmp = tempfile::tempdir().unwrap();
    let victim = tmp.path().join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("keep.txt"), "x").unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let attempt = |id: &str, scratch: &Path| {
        let options = Options {
            select: Selection::default(),
            attempts: 1,
            jobs: 1,
            binary: None,
            kind: "manual".to_owned(),
            run_id: id.to_owned(),
            commit: "c".to_owned(),
            out: tmp.path().join("out"),
            status: None,
            scratch: scratch.to_owned(),
            item_path: &no_items,
            store: None,
        };
        run_scripted(&discovery, caps(), &PrivateList::default(), &options)
    };
    let scratch = tmp.path().join("scratch");
    // An absolute path would replace the scratch folder in a join, and `..` would climb out of it: both would end with a
    // `remove_dir_all` of somebody else's folder.
    for bad in [victim.to_str().unwrap(), "..", ".", "", ".hidden", "a/b", "a b"] {
        let error = attempt(bad, &scratch).unwrap_err().to_string();
        assert!(error.contains("not a folder name"), "{bad:?}: {error}");
    }
    assert_eq!(fs::read_to_string(victim.join("keep.txt")).unwrap(), "x");
    // A run id that already has a folder is refused: it would share scratch and evidence with the first.
    fs::create_dir_all(tmp.path().join("out/runs/taken")).unwrap();
    assert!(attempt("taken", &scratch).unwrap_err().to_string().contains("already has a folder"));
    assert!(attempt("fresh", &scratch).is_ok());
}

#[test]
fn a_task_whose_id_cannot_be_a_folder_name_is_refused() {
    let ws = workspace();
    let toml = fs::read_to_string(ws.path().join("crates/c/evals/up.toml")).unwrap();
    write(&ws.path().join("crates/c/evals/up.toml"), &toml.replace("id = 'demo.up'", "id = '..'"));
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let report = play(ws.path(), &folders, "2026-10-13T1000Z-manual", 1, 1, false);
    assert!(
        report.refused.iter().any(|r| r.task == ".." && r.why.contains("cannot name a folder")),
        "{:?}",
        report.refused
    );
    assert!(report.attempts.iter().all(|a| a.task != ".."), "nothing of it ran");
}

#[test]
fn scripts_with_dotted_names_get_their_own_oracle_folders() {
    let ws = workspace();
    let dir = ws.path().join("crates/c/evals");
    let call = |text: &str| format!("{{\"call\": \"evaltest_set_note\", \"args\": {{\"text\": \"{text}\"}}}}\n");
    write(&dir.join("up.a.b.fail.jsonl"), &call("x"));
    write(&dir.join("up.c.b.fail.jsonl"), &call("y"));
    write(&dir.join("up.null.fail.jsonl"), &call("z"));
    let tmp = tempfile::tempdir().unwrap();
    let folders = Folders::new(tmp.path());
    let select = Selection { tasks: vec!["demo.up".to_owned()], ..Selection::default() };
    let report = play_with(ws.path(), &folders, "2026-10-13T1000Z-manual", select, 1, 4, false);
    assert!(report.ok(), "{:?}", report.deviations);
    let task_dir = folders.out.join("runs/2026-10-13T1000Z-manual/demo.up");
    for name in ["oracle-a.b-fail", "oracle-c.b-fail", "oracle-null-fail", "oracle-nope-fail", "oracle-null"] {
        assert!(task_dir.join(name).join("checks.json").is_file(), "{name}");
    }
    assert_eq!(report.run.oracles.played, 5);
}

#[test]
fn options_that_make_no_sense_are_refused_before_anything_runs() {
    let ws = workspace();
    let tmp = tempfile::tempdir().unwrap();
    let discovery = eval::discover(ws.path()).unwrap();
    let no_items = |_: &eval::fixture::SetItemRef| Err(String::new());
    let options = |attempts: u32, kind: &str| Options {
        select: Selection::default(),
        attempts,
        jobs: 1,
        binary: None,
        kind: kind.to_owned(),
        run_id: "r".to_owned(),
        commit: "c".to_owned(),
        out: tmp.path().join("out"),
        status: None,
        scratch: tmp.path().join("scratch"),
        item_path: &no_items,
        store: None,
    };
    for (attempts, kind, word) in [(0, "manual", "--attempts"), (1, "sometimes", "run kind")] {
        let error = run_scripted(&discovery, caps(), &PrivateList::default(), &options(attempts, kind)).unwrap_err();
        assert!(error.to_string().contains(word), "{error}");
    }
    assert!(!tmp.path().join("out").exists() && !tmp.path().join("scratch").exists());
}

/// One reference-driver attempt line of the history, as the schema of design 7.1 spells it.
fn history_line(run: &str, outcome: &str) -> String {
    json!({
        "v": 1, "run": run, "commit": "abc1234", "family": "task", "driver": "reference", "model": "m", "effort": null,
        "task": "t", "task_version": 1, "task_hash": "b3:x", "feature": "QG-03", "attempt": 1, "outcome": outcome,
        "reason": null, "checks": [], "turns": 3, "tool_calls": 5, "tool_errors": 0, "retries": 0,
        "usage": {"input": 1, "output": 1, "cache_read": 1, "cache_write": 0}, "usd_micros": 10, "wall_ms": 1,
        "tools_hash": "b3:t", "prompt_hash": "b3:p", "harness": "0", "transcript": format!("evals/runs/{run}/t/1/")
    })
    .to_string()
}

/// One run line of the history: a finished run, `valid` or not.
fn run_line(run: &str, valid: bool) -> String {
    json!({
        "v": 1, "run": run, "kind": "nightly", "driver": "reference", "model": "m", "commit": "abc1234",
        "tasks_run": ["t"], "tasks_skipped": [],
        "attempts": {"pass": 0, "fail": 0, "refusal": 0, "error": 0, "skipped": 0},
        "oracles": {"played": 0, "deviations": 0}, "spend": {"usd_micros": 0}, "wall_ms": 1, "valid": valid
    })
    .to_string()
}

/// `eval gate` exits 1 on a regression and 0 otherwise, and `--json` carries what a report needs.
#[test]
fn the_gate_command_reads_the_history_and_exits_by_what_it_finds() {
    let dir = tempfile::tempdir().unwrap();
    let status = dir.path().join("status");
    fs::create_dir_all(&status).unwrap();
    // Thirty baseline attempts at 90% on days before the window, then ten attempts with `window_passes` passes.
    let write_history = |window_passes: usize| {
        let mut lines = Vec::new();
        for day in 1..=10 {
            for i in 0..3 {
                let outcome = if day <= 3 && i == 0 { "fail" } else { "pass" };
                lines.push(history_line(&format!("2026-09-{day:02}T0000Z-nightly"), outcome));
            }
        }
        for i in 0..10 {
            lines.push(history_line("2026-09-12T0000Z-nightly", if i < window_passes { "pass" } else { "fail" }));
        }
        fs::write(status.join(HISTORY_FILE), lines.join("\n") + "\n").unwrap();
        // Every night's run is finished and valid; a run that is not never reaches the gate.
        let mut runs: Vec<String> = (1..=10).map(|d| run_line(&format!("2026-09-{d:02}T0000Z-nightly"), true)).collect();
        runs.push(run_line("2026-09-12T0000Z-nightly", true));
        fs::write(status.join(RUNS_FILE), runs.join("\n") + "\n").unwrap();
    };
    let gate =
        |extra: &[&str]| common::run(&[&["eval", "gate", "--status", common::arg(&status)], extra].concat(), &[]);

    write_history(5);
    let bad = gate(&[]);
    assert_eq!(common::code(&bad), 1, "{}", common::text(&bad.stdout));
    assert!(common::text(&bad.stdout).contains("regression"), "{}", common::text(&bad.stdout));
    let json: serde_json::Value = serde_json::from_str(&common::text(&gate(&["--json"]).stdout)).unwrap();
    assert_eq!(json["tasks"][0]["status"], "regression");
    assert_eq!(json["tasks"][0]["baseline"]["n"], 30);
    assert_eq!(json["tasks"][0]["failing"].as_array().unwrap().len(), 5);

    write_history(6);
    let fine = gate(&[]);
    assert_eq!(common::code(&fine), 0, "{}", common::text(&fine.stdout));
    assert!(common::text(&fine.stdout).starts_with("ok"), "{}", common::text(&fine.stdout));

    // A history line the harness cannot read is a fault of the harness's, not a quiet night.
    fs::write(status.join(HISTORY_FILE), "{\"v\": 2}\n").unwrap();
    assert_eq!(common::code(&gate(&[])), 2);
    // No history at all gates nothing.
    fs::remove_file(status.join(HISTORY_FILE)).unwrap();
    assert_eq!(common::code(&gate(&[])), 0);
}
