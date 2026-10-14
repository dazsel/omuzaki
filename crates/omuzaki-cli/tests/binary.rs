//! The real `omuzaki` binary: argument parsing, streams and exit codes as a script or agent sees them.
//!
//! This binary ships the product's ops (`tests/registrations.rs` is the link gate); these tests pin that, the usage errors, how a missing
//! or broken project is reported, and the documented exit codes. Running ops end to end, with demo ops linked in, is
//! `e2e.rs`. Output formats are covered by `surface.rs` and `run.rs`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use common::{new_project, project_toml};

fn omuzaki(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omuzaki")).args(args).output().expect("run omuzaki")
}

/// `omuzaki --project <dir> <args>`.
fn omuzaki_in(dir: &Path, args: &[&str]) -> Output {
    let dir = dir.to_str().expect("utf-8 path");
    omuzaki(&[&["--project", dir][..], args].concat())
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8(bytes.to_vec()).expect("utf-8 output")
}

#[test]
fn help_documents_the_exit_codes() {
    let out = omuzaki(&["--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = text(&out.stdout);
    for line in ["Exit codes:", "  0  ok", "  2  usage", "  3  invalid", "  7  unsupported", "  8  unavailable"] {
        assert!(help.contains(line), "missing {line:?} in:\n{help}");
    }
    for command in ["ops", "help", "run", "undo", "redo", "mcp"] {
        assert!(help.contains(&format!("\n  {command}")), "missing command {command} in:\n{help}");
    }
}

#[test]
fn ops_lists_the_products_ops() {
    let out = omuzaki(&["ops"]);
    assert_eq!(out.status.code(), Some(0));
    let listing = text(&out.stdout);
    for op in ["character.create", "face.set_style", "io.inspect"] {
        assert!(listing.contains(op), "`omuzaki ops` lacks {op}:\n{listing}");
    }

    let json: serde_json::Value = serde_json::from_slice(&omuzaki(&["ops", "--json"]).stdout).expect("json");
    assert!(json.as_array().is_some_and(|ops| ops.len() > 30), "{json}");
}

#[test]
fn usage_errors_exit_2() {
    for args in [&[][..], &["bogus"], &["run"], &["run", "x", "--nope"]] {
        let out = omuzaki(args);
        assert_eq!(out.status.code(), Some(2), "{args:?}");
        assert_eq!(text(&out.stdout), "", "{args:?} printed to stdout");
    }
}

#[test]
fn unknown_op_help_exits_4_with_text_or_json_on_stderr() {
    let out = omuzaki(&["help", "no.such_op"]);
    assert_eq!(out.status.code(), Some(4));
    assert_eq!(text(&out.stdout), "");
    assert!(text(&out.stderr).contains("error: not_found: no op called no.such_op"));

    let json = omuzaki(&["help", "no.such_op", "--json"]);
    assert_eq!(json.status.code(), Some(4));
    let body: serde_json::Value = serde_json::from_str(&text(&json.stderr)).expect("stderr is JSON");
    assert_eq!(body["error"]["code"], "not_found");
}

#[test]
fn commands_that_touch_the_project_need_one_and_exit_3_naming_the_flag() {
    for args in [&["run", "x.y"][..], &["run", "x.y", "--dry-run"], &["undo"], &["redo"], &["mcp"]] {
        let out = omuzaki(args);
        assert_eq!(out.status.code(), Some(3), "{args:?}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), "", "{args:?}");
        let err = text(&out.stderr);
        assert!(err.contains("field: project") && err.contains("--project <DIR>"), "{args:?}: {err}");
    }
}

#[test]
fn a_project_folder_that_does_not_exist_or_is_not_a_project_exits_4() {
    let dir = tempfile::tempdir().expect("temp dir");
    let missing = dir.path().join("nope");
    for project in [dir.path(), missing.as_path()] {
        let out = omuzaki_in(project, &["ops"]);
        assert_eq!(out.status.code(), Some(4), "{}: {}", project.display(), text(&out.stderr));
        assert!(text(&out.stderr).contains("error: not_found:"), "{}", text(&out.stderr));
    }
}

#[test]
fn a_broken_project_exits_3_and_a_newer_one_exits_7() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("project.toml"), "name = \n").expect("write");
    let broken = omuzaki_in(dir.path(), &["run", "x.y"]);
    assert_eq!(broken.status.code(), Some(3), "{}", text(&broken.stderr));
    assert!(text(&broken.stderr).contains("field: project"));

    std::fs::write(dir.path().join("project.toml"), "name = \"x\"\nschema = 99999\n").expect("write");
    let newer = omuzaki_in(dir.path(), &["run", "x.y"]);
    assert_eq!(newer.status.code(), Some(7), "{}", text(&newer.stderr));
    assert!(text(&newer.stderr).contains("99999"));
}

#[test]
fn bad_args_exit_3_naming_the_args_field() {
    let project = new_project();
    let out = omuzaki_in(project.path(), &["run", "x.y", "--args", "{nope"]);
    assert_eq!(out.status.code(), Some(3));
    assert!(text(&out.stderr).contains("field: args"));
}

#[test]
fn a_missing_args_file_exits_4_as_documented() {
    let project = new_project();
    let out = omuzaki_in(project.path(), &["run", "x.y", "--args", "@/definitely/not/here.json"]);
    assert_eq!(out.status.code(), Some(4), "{}", text(&out.stderr));
    let err = text(&out.stderr);
    assert!(err.contains("error: not_found: no arguments file") && err.contains("hint: check the path"), "{err}");
}

#[test]
fn args_are_read_from_a_pipe_with_at_dash() {
    let project = new_project();
    let run = |stdin: &str| {
        let mut child = Command::new(env!("CARGO_BIN_EXE_omuzaki"))
            .arg("--project")
            .arg(project.path())
            .args(["run", "x.y", "--args", "@-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn omuzaki");
        child.stdin.take().expect("stdin").write_all(stdin.as_bytes()).expect("write stdin");
        child.wait_with_output().expect("wait for omuzaki")
    };
    // Valid JSON gets past argument parsing and reaches the executor, which has no op `x.y` (invalid, field `op`).
    let reached = run(r#"{"blend": 0.5}"#);
    assert_eq!(reached.status.code(), Some(3));
    assert!(text(&reached.stderr).contains("field: op"), "{}", text(&reached.stderr));
    // Bad JSON stops at argument parsing.
    let stopped = run("{nope");
    assert_eq!(stopped.status.code(), Some(3));
    assert!(text(&stopped.stderr).contains("field: args"), "{}", text(&stopped.stderr));
}

#[test]
fn history_commands_start_empty_in_every_process_and_leave_the_project_alone() {
    // The op log and the undo stacks live in the executor's memory (see the crate docs), so a fresh `omuzaki undo` has
    // nothing to undo. When they persist, this test is the one to change.
    let project = new_project();
    let before = project_toml(project.path());
    for (command, nothing) in [("undo", "nothing to undo\n"), ("redo", "nothing to redo\n")] {
        let out = omuzaki_in(project.path(), &[command]);
        assert_eq!(out.status.code(), Some(0), "{command}: {}", text(&out.stderr));
        assert_eq!(text(&out.stdout), nothing);
    }
    assert_eq!(project_toml(project.path()), before);
}
