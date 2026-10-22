//! `omuzaki branch` end to end through the built binary (`docs/design/branches.md` 5 and 7): create a branch, edit it
//! with `omuzaki run`, switch the folder between branches (each command a new process), merge with a conflict
//! resolved by flag, compare, and the exit codes scripts branch on.

// `CARGO_BIN_EXE_omuzaki-fixture` exists only when the fixture is built (`test-fixtures` feature, see Cargo.toml).
#![cfg(feature = "test-fixtures")]
// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests write and read their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const FIXTURE: &str = env!("CARGO_BIN_EXE_omuzaki-fixture");

/// Longest any child process may take. Generous: a loaded CI machine is slow, a hang is infinite.
const LIMIT: Duration = Duration::from_secs(60);

struct Done {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

fn omuzaki(dir: &Path, args: &[&str]) -> Done {
    let mut child = Command::new(FIXTURE)
        .arg("--project")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the binary");
    let deadline = Instant::now() + LIMIT;
    while child.try_wait().expect("wait").is_none() {
        if Instant::now() > deadline {
            child.kill().ok();
            panic!("omuzaki {args:?} took longer than {LIMIT:?}");
        }
        std::thread::park_timeout(Duration::from_millis(20));
    }
    let output = child.wait_with_output().expect("the output");
    Done {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// Runs `args` and expects exit code `code`.
fn expect(dir: &Path, args: &[&str], code: i32) -> String {
    let done = omuzaki(dir, args);
    assert_eq!(done.code, Some(code), "omuzaki {args:?}\nstdout: {}\nstderr: {}", done.stdout, done.stderr);
    done.stdout
}

fn set_note(dir: &Path, text: &str) {
    expect(dir, &["run", "demo.set_note", "--args", &json!({"text": text}).to_string()], 0);
}

/// The note as the project's files hold it.
fn note_on_disk(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("project.toml")).expect("project.toml");
    let table: toml::Table = text.parse().expect("project.toml parses");
    table.get("project").and_then(|p| p.get("note")).and_then(|n| n.as_str()).map(str::to_owned)
}

#[test]
fn create_edit_switch_merge_and_diff_from_the_shell() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("demo");
    let made = Command::new(FIXTURE).arg("new").arg(&dir).output().expect("omuzaki new");
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    set_note(&dir, "base");

    // A branch, edited after switching to it: the folder holds the branch, and the next process opens on it.
    let created = expect(&dir, &["branch", "create", "aria/oval"], 0);
    assert!(created.starts_with("created aria/oval"), "{created}");
    expect(&dir, &["branch", "switch", "aria/oval"], 0);
    assert_eq!(note_on_disk(&dir).as_deref(), Some("base"));
    set_note(&dir, "oval");
    assert_eq!(note_on_disk(&dir).as_deref(), Some("oval"));
    let listed: Value = serde_json::from_str(&expect(&dir, &["--json", "branch", "list"], 0)).unwrap();
    assert_eq!(listed["folder"], "aria/oval", "{listed:#}");

    // Back on main, which never saw the branch's edit; then main changes the same note.
    expect(&dir, &["branch", "switch", "main"], 0);
    assert_eq!(note_on_disk(&dir).as_deref(), Some("base"));
    set_note(&dir, "main");
    let diff = expect(&dir, &["branch", "diff", "main", "aria/oval"], 0);
    assert!(diff.contains("project settings"), "{diff}");
    let status = expect(&dir, &["branch", "status", "aria/oval"], 0);
    assert_eq!(status.trim(), "1 changed here, 1 on main");

    // The conflict stops the merge with its own code and lists the unit; a flag resolves it.
    let stopped = omuzaki(&dir, &["branch", "merge", "aria/oval"]);
    assert_eq!(stopped.code, Some(13), "{}{}", stopped.stdout, stopped.stderr);
    assert!(stopped.stdout.contains("conflict") && stopped.stdout.contains("project"), "{}", stopped.stdout);
    assert_eq!(note_on_disk(&dir).as_deref(), Some("main"), "nothing changed");
    let preview = expect(&dir, &["branch", "merge", "aria/oval", "--resolve", "project=theirs", "--dry-run"], 0);
    assert!(preview.starts_with("would merge aria/oval"), "{preview}");
    expect(&dir, &["branch", "merge", "aria/oval", "--resolve", "project=theirs"], 0);
    assert_eq!(note_on_disk(&dir).as_deref(), Some("oval"));
    // One undo takes the whole merge back.
    expect(&dir, &["undo"], 0);
    assert_eq!(note_on_disk(&dir).as_deref(), Some("main"));

    // Exit codes: an unknown branch is not found (4); a name that breaks the rule is invalid (3).
    expect(&dir, &["branch", "switch", "nope"], 4);
    expect(&dir, &["branch", "create", "Bad Name!"], 3);
}
