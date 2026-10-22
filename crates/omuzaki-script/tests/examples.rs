//! The runnable examples in `examples/`: each passes the static check with nothing to report and runs over the fake
//! runner. Op names in the examples are illustrative; the fake registers the names and argument shapes they use.

// A test file: a failed unwrap is the failure, and reading the examples folder is what the test is for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::disallowed_methods)]

mod common;

use std::collections::BTreeSet;
use std::path::PathBuf;

use common::{FakeRunner, Flag, request, run_with};
use omuzaki_ops_api::{Runner, Scope};
use omuzaki_script::{Limits, ScriptHost, effective_grants};
use serde_json::{Value, json};

/// What an example needs to run, and what it must do.
struct Case {
    file: &'static str,
    params: Value,
    /// The op names the run sends, in order.
    sends: &'static [&'static str],
    /// The script's last value.
    value: Value,
    /// The lines it prints or logs.
    output: &'static [&'static str],
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            file: "hello.rhai",
            params: json!({}),
            sends: &[],
            value: Value::Null,
            output: &[
                "Hello from Omuzaki!",
                "a script runs as you, so everything it changes is one step in your undo history",
            ],
        },
        Case {
            file: "loop_with_try_op.rhai",
            params: json!({}),
            sends: &["face.set_profile", "face.set_profile", "face.set_profile"],
            value: json!(2),
            output: &["skipped nobody: no character called nobody", "updated 2 characters"],
        },
        Case {
            file: "read_with_get.rhai",
            params: json!({}),
            sends: &[],
            value: json!({ "blend": 0.25, "profile": "round" }),
            output: &["aki's profile is round at blend 0.25"],
        },
        Case {
            file: "params.rhai",
            params: json!({ "character": "ren" }),
            sends: &["face.set_profile"],
            value: json!({ "character": "ren", "profile": "egg-oval", "blend": 0.5 }),
            output: &[],
        },
        Case {
            file: "ceiling.rhai",
            params: json!({}),
            sends: &["face.set_profile", "face.set_profile"],
            value: Value::Null,
            output: &[],
        },
        Case {
            file: "try_catch.rhai",
            params: json!({}),
            sends: &["face.set_profile"],
            value: Value::Null,
            output: &[
                "face.set_profile failed (not_found): no character called nobody",
                "hint: list the characters with `get(\"characters\")`",
            ],
        },
        Case {
            file: "last_value.rhai",
            params: json!({}),
            sends: &["measure.likeness"],
            value: json!(50.0),
            output: &[],
        },
    ]
}

fn examples() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples")
}

fn source(file: &str) -> String {
    std::fs::read_to_string(examples().join(file)).unwrap_or_else(|e| panic!("{file}: {e}"))
}

fn runner() -> FakeRunner {
    let runner = FakeRunner::default();
    *runner.document.lock().unwrap() =
        json!({ "characters": { "aki": { "face": { "profile": "round", "blend": 0.25 } }, "ren": { "face": {} } } });
    runner
}

#[test]
fn every_example_is_covered_by_a_case() {
    let on_disk: BTreeSet<String> = std::fs::read_dir(examples())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.to_ascii_lowercase().ends_with(".rhai"))
        .collect();
    let covered: BTreeSet<String> = cases().iter().map(|case| case.file.to_owned()).collect();
    assert_eq!(on_disk, covered, "an example has no case, or a case has no example");
}

#[test]
fn every_example_checks_clean() {
    let host = ScriptHost::new(&runner().ops(), Limits::console());
    for case in cases() {
        let report = host.check(&source(case.file));
        assert!(report.is_clean(), "{}: {report:#?}", case.file);
        assert!(report.parse_errors.is_empty() && report.unknown_ops.is_empty() && report.field_errors.is_empty());
    }
}

#[test]
fn every_example_runs_and_does_what_it_says() {
    for case in cases() {
        let mut request = request(&source(case.file));
        request.params = case.params.as_object().unwrap().clone();
        let (runner, result) = run_with(request, &Flag::default(), Limits::console(), runner());
        let outcome = result.unwrap_or_else(|e| panic!("{}: {e}", case.file));
        assert_eq!(outcome.value, case.value, "{}", case.file);
        let printed: Vec<&str> = outcome.output.iter().map(|line| line.text.as_str()).collect();
        assert_eq!(printed, case.output, "{}", case.file);
        let sent: Vec<String> = runner.requests().into_iter().map(|r| r.op).collect();
        assert_eq!(sent, case.sends, "{}", case.file);
    }
}

#[test]
fn the_ceiling_example_narrows_to_edit_character() {
    let host = ScriptHost::new(&runner().ops(), Limits::console());
    let report = host.check(&source("ceiling.rhai"));
    assert_eq!(report.ceiling, Some(vec![Scope::EditCharacter]));
    let grants = effective_grants(&omuzaki_ops_api::Grants::person(), &report);
    assert_eq!(grants.scopes().collect::<Vec<_>>(), [Scope::Read, Scope::EditCharacter]);
}
