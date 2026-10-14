//! The commands against a real project folder and the real executor, in one process, so `undo` and `redo` have a
//! history to work on. `e2e.rs` runs the same flow through the built binaries.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::io;
use std::path::Path;
use std::sync::Arc;

use common::{new_project, project_toml};
use omuzaki_cli::cli::try_parse_from;
use omuzaki_cli::commands::execute;
use omuzaki_cli::session::{Session, open_runner};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_ops_api::{ErrorCode, Grants, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Runner, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Set the project's note.
#[derive(Serialize, Deserialize, JsonSchema)]
struct SetNote {
    /// The note's text.
    text: String,
}

impl Op for SetNote {
    fn meta() -> OpMeta {
        OpMeta {
            name: "note.set",
            summary: "Set the project's note.",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let at = DocPath::parse("project/note").unwrap();
        Ok(Plan::edit(Patch::new().with(at.clone(), ctx.doc.get(&at).cloned(), Some(json!(self.text)))))
    }
}

omuzaki_ops_api::register_op!(SetNote);

/// One `omuzaki` process's worth of state: a runner over a project that the commands share, as `omuzaki mcp` shares it.
struct Process<'a> {
    project: &'a Path,
    runner: Arc<dyn Runner>,
}

impl<'a> Process<'a> {
    fn start(project: &'a Path) -> Self {
        let cli = parse(Some(project), &["ops"]);
        Self { project, runner: open_runner(&cli).expect("the project opens") }
    }

    fn omuzaki(&self, args: &[&str]) -> Result<String, OpError> {
        let cli = parse(Some(self.project), args);
        execute(&cli, Arc::clone(&self.runner), &Session::person("tester"), &mut io::empty())
    }
}

fn parse(project: Option<&Path>, args: &[&str]) -> omuzaki_cli::cli::Cli {
    let project: Vec<&str> = project.map(|p| vec!["--project", p.to_str().unwrap()]).unwrap_or_default();
    try_parse_from(["omuzaki"].into_iter().chain(project).chain(args.iter().copied())).expect("valid command line")
}

const SET_HELLO: [&str; 4] = ["run", "note.set", "--args", r#"{"text": "hello"}"#];

#[test]
fn a_dry_run_changes_nothing_and_an_applied_run_is_saved_before_it_returns() {
    let project = new_project();
    let original = project_toml(project.path());
    let process = Process::start(project.path());

    let preview = process.omuzaki(&[&SET_HELLO[..], &["--dry-run"]].concat()).unwrap();
    assert!(preview.starts_with("dry run of note.set: nothing was changed"), "{preview}");
    assert_eq!(project_toml(project.path()), original);

    let applied = process.omuzaki(&SET_HELLO).unwrap();
    assert!(applied.starts_with("applied note.set (step 1, document version 1)"), "{applied}");
    assert!(project_toml(project.path()).contains("note = \"hello\""), "{}", project_toml(project.path()));
}

#[test]
fn undo_and_redo_move_the_files_back_and_forth_and_name_the_step() {
    let project = new_project();
    let original = project_toml(project.path());
    let process = Process::start(project.path());
    process.omuzaki(&SET_HELLO).unwrap();
    let applied = project_toml(project.path());

    assert_eq!(process.omuzaki(&["undo"]).unwrap(), "undid \"note.set\"\n");
    assert_eq!(project_toml(project.path()), original);

    assert_eq!(process.omuzaki(&["redo", "--json"]).unwrap(), "{\n  \"redone\": \"note.set\"\n}\n");
    assert_eq!(project_toml(project.path()), applied);

    assert_eq!(process.omuzaki(&["redo"]).unwrap(), "nothing to redo\n");
}

#[test]
fn a_new_process_sees_the_saved_change_but_starts_with_no_history() {
    let project = new_project();
    Process::start(project.path()).omuzaki(&SET_HELLO).unwrap();
    let applied = project_toml(project.path());

    let later = Process::start(project.path());
    assert_eq!(
        later.runner.get(&DocPath::parse("project/note").unwrap(), &Grants::person()).unwrap(),
        Some(json!("hello"))
    );
    assert_eq!(later.omuzaki(&["undo"]).unwrap(), "nothing to undo\n");
    assert_eq!(project_toml(project.path()), applied);
}

#[test]
fn arguments_the_op_cannot_decode_are_refused_and_the_files_stay_as_they_were() {
    let project = new_project();
    let original = project_toml(project.path());
    let process = Process::start(project.path());
    let e = process.omuzaki(&["run", "note.set", "--args", r#"{"text": 5}"#]).unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid, "{e:?}");
    assert_eq!(project_toml(project.path()), original);
}

#[test]
fn only_ops_and_help_work_without_a_project() {
    for args in [&["ops"][..], &["help"], &["help", "note.set"]] {
        let cli = parse(None, args);
        let runner = open_runner(&cli).unwrap_or_else(|e| panic!("{args:?}: {e:?}"));
        assert!(execute(&cli, runner, &Session::person("tester"), &mut io::empty()).is_ok(), "{args:?}");
    }
    for args in [&["run", "note.set"][..], &["undo"], &["redo"], &["mcp"]] {
        let Err(e) = open_runner(&parse(None, args)) else { panic!("{args:?} should need a project") };
        assert_eq!((e.code, e.field.as_deref()), (ErrorCode::Invalid, Some("project")), "{args:?}");
        assert!(e.hint.is_some_and(|h| h.contains("--project")));
    }
}

#[test]
fn a_bad_project_is_refused_even_for_ops_and_help() {
    let dir = tempfile::tempdir().unwrap();
    let Err(e) = open_runner(&parse(Some(&dir.path().join("nope")), &["ops"])) else { panic!("no such folder") };
    assert_eq!(e.code, ErrorCode::NotFound, "{e:?}");
}
