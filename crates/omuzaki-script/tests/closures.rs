//! Where a closure may appear: only as an argument of `try_op` or an array function, so none outlives its call
//! (the size limits don't count what a closure captures).

// A test file: a failed unwrap is the failure.
#![allow(clippy::unwrap_used)]

mod common;

use common::{FakeRunner, run};
use omuzaki_ops_api::Runner;
use omuzaki_script::{Limits, ScriptErrorKind, ScriptHost};

fn host() -> ScriptHost {
    ScriptHost::new(&FakeRunner::default().ops(), Limits::console())
}

#[test]
fn a_closure_passed_straight_to_try_op_or_an_array_function_is_allowed() {
    for source in [
        "try_op(|| 1)",
        "let s = \"a\"; try_op(|| s)",
        "let s = 2; [1, 2].map(|x| x + s)",
        "let s = 2; [1, 2, 3].filter(|x| x > s)",
        "let s = 1; [1, 2, 3].reduce(|acc, x| acc + x + s, 0)",
        "let s = 1; [3, 1, 2].sort(|a, b| a - b + s - s)",
        "[1, 2].for_each(|x| try_op(|| x))",
        "let s = 1; try_op(|| try_op(|| s))",
        "let s = 1; [1, 2].some(|x| x == s)",
        "let s = 1; [1, 2].all(|x| x >= s)",
    ] {
        let report = host().check(source);
        assert!(report.is_clean(), "{source}: {report:?}");
        let (_, result) = run(source);
        assert!(result.is_ok(), "{source}: {}", result.unwrap_err());
    }
}

#[test]
fn a_closure_anywhere_else_is_refused_by_the_check_and_by_the_run_before_anything_runs() {
    // (script, line, column) of the closure.
    let cases = [
        ("let f = || 1;", 1, 9),
        ("let s = 1;\nlet f = || s;", 2, 9),
        ("let s = 1;\nlet fs = [];\nfs.push(|| s);", 3, 9),
        ("let s = 1;\nlet m = #{};\nm.f = || s;", 3, 7),
        ("fn make(s) {\n    || s\n}\nmake(1)", 2, 5),
        ("fn keep(f) { f }\nkeep(|| 1)", 2, 6),
        ("try_op({ || 1 })", 1, 10),
        ("let s = 1;\ntry_op(|| {\n    let g = || s;\n    g\n})", 3, 13),
        ("[|| 1].map(|f| f)", 1, 2),
        ("return || 1;", 1, 8),
    ];
    for (source, line, column) in cases {
        let report = host().check(source);
        assert_eq!(report.parse_errors.len(), 1, "{source}: {report:?}");
        let found = &report.parse_errors[0];
        assert_eq!((found.line, found.column), (line, column), "{source}: {found:?}");
        assert!(found.message.contains("closure"), "{source}: {found:?}");
        let (runner, result) = run(source);
        let error = result.expect_err(source);
        assert_eq!(error.kind, ScriptErrorKind::Parse, "{source}: {error}");
        assert_eq!((error.line, error.column), (line, column), "{source}: {error}");
        assert_eq!(runner.requests(), vec![], "{source}: nothing was sent");
        assert_eq!(runner.begins(), 0, "{source}: nothing began");
    }
}

#[test]
fn closures_that_hold_large_captures_never_get_to_run() {
    // Rhai counts the values in arrays and maps, not what a closure captures: this would hold 2,000 megabyte strings.
    let source = "\
let k = \"x\";
while k.len < 500000 { k += k; }
let fs = [];
for i in 0..2000 {
    let s = k + i;
    fs.push(|| s);
}";
    let report = host().check(source);
    assert_eq!(report.parse_errors.len(), 1, "{report:?}");
    let (_, result) = run(source);
    assert_eq!(result.expect_err("refused").kind, ScriptErrorKind::Parse);
}

#[test]
fn a_function_named_like_a_holder_is_refused_when_the_script_uses_closures() {
    for name in ["map", "try_op", "filter"] {
        let source = format!("fn {name}(a, f) {{ a }}\ntry_op(|| 1)");
        let report = host().check(&source);
        assert_eq!(report.parse_errors.len(), 1, "{source}: {report:?}");
        assert!(report.parse_errors[0].message.contains(&format!("`{name}`")), "{source}: {report:?}");
        let (_, result) = run(&source);
        assert_eq!(result.expect_err("refused").kind, ScriptErrorKind::Parse, "{source}");
    }
    // Without a closure there is nothing to keep, so the name is free.
    let report = host().check("fn map(a, f) { a }\nmap(1, 2)");
    assert!(report.is_clean(), "{report:?}");
}
