//! The `doc.*` checks: each kind passes on a right result and fails on a wrong one, and a fixture that lacks what the
//! check reads is the harness's error, never the agent's fail.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::Runner;
use omuzaki_ops_mock::MockRunner;
use omuzaki_test::eval::check::{Checker, Outcome};
use omuzaki_test::eval::task::Task;
use omuzaki_test::eval::transcript::Transcript;
use serde_json::{Value, json};

fn runner(doc: &Value) -> Arc<dyn Runner> {
    Arc::new(MockRunner::new(MemDoc::from_value(doc.clone())))
}

fn fixture() -> Value {
    json!({
        "stature": 100,
        "name": "Aki",
        "chars": {
            "a": {"name": "A", "head": {"w": 10, "h": 12}, "hair": {"colour": "brown"}},
            "b": {"name": "B", "head": {"w": 11, "h": 13}},
        },
    })
}

/// The standard right result: the head of `a` is wider and `b` is gone.
fn result() -> Value {
    json!({
        "stature": 104,
        "name": "Aki",
        "chars": {
            "a": {"name": "A", "head": {"w": 10.5, "h": 12}, "hair": {"colour": "brown"}},
            "b": {"name": "B", "head": {"w": 11, "h": 13}},
            "fox": {"name": "Fox"},
        },
    })
}

fn grade_on(fixture: &Value, result: &Value, checks: &str) -> Vec<(Outcome, String)> {
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    let task: Task = toml::from_str(&format!("{head}{checks}")).unwrap();
    let report = Checker::new(runner(fixture), runner(result), Transcript::default()).grade(&task);
    report.results.iter().map(|r| (r.outcome, r.detail.clone())).collect()
}

fn one(checks: &str) -> (Outcome, String) {
    grade_on(&fixture(), &result(), checks).remove(0)
}

fn check(body: &str) -> String {
    format!("[[check]]\nid='c'\n{body}")
}

fn outcome(body: &str) -> Outcome {
    one(&check(body)).0
}

#[test]
fn equals_passes_on_the_right_value_and_fails_on_a_wrong_one_or_a_missing_one() {
    assert_eq!(outcome("kind='doc.equals'\npath='chars/fox/name'\nvalue='Fox'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.equals'\npath='chars/fox/name'\nvalue='Vixen'\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.equals'\npath='chars/ghost/name'\nvalue='Fox'\n"), Outcome::Fail);
    // Numbers are numbers: `104` equals `104.0`.
    assert_eq!(outcome("kind='doc.equals'\npath='stature'\nvalue=104.0\n"), Outcome::Pass);
    // A glob must hold for every value it reaches.
    assert_eq!(outcome("kind='doc.equals'\npath='chars/*/name'\nvalue='A'\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.equals'\npath='chars/*/head/h'\nvalue=12\n"), Outcome::Fail);
}

#[test]
fn range_passes_inside_the_bounds_and_fails_outside_or_on_a_non_number() {
    assert_eq!(outcome("kind='doc.range'\npath='stature'\nmin=100.0\nmax=110.0\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.range'\npath='stature'\nmin=105.0\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.range'\npath='stature'\nmax=103.0\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.range'\npath='name'\nmin=0.0\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.range'\npath='chars/*/head/w'\nmin=10.0\nmax=11.0\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.range'\npath='chars/nobody/head/w'\nmin=0.0\n"), Outcome::Fail);
}

#[test]
fn exists_and_absent_are_opposites_and_take_globs() {
    assert_eq!(outcome("kind='doc.exists'\npath='chars/fox'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.exists'\npath='chars/*/hair'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.exists'\npath='chars/ghost'\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.absent'\npath='chars/ghost'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.absent'\npath='chars/fox'\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.absent'\npath='chars/*/head/missing'\n"), Outcome::Pass);
}

#[test]
fn change_reads_the_move_from_the_fixture() {
    assert_eq!(outcome("kind='doc.change'\npath='stature'\nchange='up'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.change'\npath='stature'\nchange='down'\n"), Outcome::Fail);
    assert_eq!(outcome("kind='doc.change'\npath='stature'\nchange='up'\nat_least=3.0\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.change'\npath='stature'\nchange='up'\nat_least=5.0\n"), Outcome::Fail);
    assert_eq!(
        outcome("kind='doc.change'\npath='stature'\nchange='up'\nat_least=0.04\nrelative=true\n"),
        Outcome::Pass
    );
    assert_eq!(
        outcome("kind='doc.change'\npath='stature'\nchange='up'\nat_least=0.05\nrelative=true\n"),
        Outcome::Fail
    );
    assert_eq!(outcome("kind='doc.change'\npath='chars/a/head/w'\nchange='any'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.change'\npath='chars/a/head/h'\nchange='none'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.change'\npath='chars/a/head/w'\nchange='none'\n"), Outcome::Fail);
    // `none` and `any` also read what is not a number.
    assert_eq!(outcome("kind='doc.change'\npath='name'\nchange='none'\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.change'\npath='chars/a/name'\nchange='any'\n"), Outcome::Fail);
}

#[test]
fn change_blames_the_agent_for_a_lost_value_and_the_task_for_a_missing_baseline() {
    let broken = json!({"stature": "tall", "name": "Aki", "chars": {}});
    let up = check("kind='doc.change'\npath='stature'\nchange='up'\n");
    assert_eq!(grade_on(&fixture(), &broken, &up)[0].0, Outcome::Fail);
    let gone = json!({"name": "Aki"});
    assert_eq!(grade_on(&fixture(), &gone, &up)[0].0, Outcome::Fail);
    // The fixture lacks the path, or has several values there, or a string where `up` needs a number.
    assert_eq!(outcome("kind='doc.change'\npath='chars/fox/name'\nchange='any'\n"), Outcome::Error);
    assert_eq!(outcome("kind='doc.change'\npath='chars/*/name'\nchange='any'\n"), Outcome::Error);
    assert_eq!(outcome("kind='doc.change'\npath='name'\nchange='up'\n"), Outcome::Error);
}

#[test]
fn changed_only_passes_when_every_difference_lies_under_the_paths() {
    let allowed = "kind='doc.changed_only'\npaths=['stature', 'chars/a/head/**', 'chars/fox/**']\n";
    assert_eq!(outcome(allowed), Outcome::Pass);
    let (outcome_, detail) = one(&check("kind='doc.changed_only'\npaths=['stature', 'chars/a/head/**']\n"));
    assert_eq!(outcome_, Outcome::Fail);
    assert!(detail.contains("chars/fox"), "{detail}");
    // A glob in the middle, and a change the paths do not reach.
    assert_eq!(outcome("kind='doc.changed_only'\npaths=['stature', 'chars/*/head/w', 'chars/fox']\n"), Outcome::Pass);
    assert_eq!(outcome("kind='doc.changed_only'\npaths=['chars/**']\n"), Outcome::Fail);
}

#[test]
fn changed_only_sees_removed_and_added_keys_and_array_items() {
    let before = json!({"tags": ["x", "y"], "keep": 1, "gone": true});
    let after = json!({"tags": ["x", "y", "z"], "keep": 1});
    let checks = |paths: &str| check(&format!("kind='doc.changed_only'\npaths={paths}\n"));
    assert_eq!(grade_on(&before, &after, &checks("['tags/**', 'gone']"))[0].0, Outcome::Pass);
    assert_eq!(grade_on(&before, &after, &checks("['tags/**']"))[0].0, Outcome::Fail);
    assert_eq!(grade_on(&before, &after, &checks("['gone']"))[0].0, Outcome::Fail);
}

#[test]
fn unchanged_passes_when_nothing_under_the_paths_differs() {
    assert_eq!(outcome("kind='doc.unchanged'\npaths=['name', 'chars/a/name', 'chars/b/**']\n"), Outcome::Pass);
    let (outcome_, detail) = one(&check("kind='doc.unchanged'\npaths=['chars/a/**']\n"));
    assert_eq!(outcome_, Outcome::Fail);
    assert!(detail.contains("chars/a/head/w"), "{detail}");
    // The whole document when no paths are given.
    assert_eq!(outcome("kind='doc.unchanged'\n"), Outcome::Fail);
    assert_eq!(grade_on(&fixture(), &fixture(), &check("kind='doc.unchanged'\n"))[0].0, Outcome::Pass);
}

#[test]
fn unchanged_counts_a_value_the_agent_removed_or_added_under_a_pattern() {
    let before = json!({"a": {"x": 1}, "b": 2});
    let removed = json!({"b": 2});
    let added = json!({"a": {"x": 1}, "b": 2, "c": 3});
    assert_eq!(grade_on(&before, &removed, &check("kind='doc.unchanged'\npaths=['a/x']\n"))[0].0, Outcome::Fail);
    assert_eq!(grade_on(&before, &removed, &check("kind='doc.unchanged'\npaths=['b']\n"))[0].0, Outcome::Pass);
    assert_eq!(grade_on(&before, &added, &check("kind='doc.unchanged'\npaths=['*']\n"))[0].0, Outcome::Fail);
    assert_eq!(grade_on(&before, &added, &check("kind='doc.unchanged'\npaths=['a/**']\n"))[0].0, Outcome::Pass);
}

#[test]
fn doc_checks_use_names_and_refuse_malformed_paths() {
    let named = "kind='doc.equals'\npath='chars/{main}/name'\nvalue='A'\n";
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    let task: Task = toml::from_str(&format!("{head}{}", check(named))).unwrap();
    let exports = [("main".to_owned(), "a".to_owned())].into();
    let report =
        Checker::new(runner(&fixture()), runner(&result()), Transcript::default()).with_exports(exports).grade(&task);
    assert_eq!(report.results[0].outcome, Outcome::Pass);
    for bad in ["/chars", "chars/", "chars//a"] {
        assert_eq!(outcome(&format!("kind='doc.exists'\npath='{bad}'\n")), Outcome::Error, "{bad}");
    }
    assert_eq!(outcome("kind='doc.changed_only'\npaths=[]\n"), Outcome::Error);
}

#[test]
fn doc_reports_are_byte_identical_between_gradings() {
    let checks = format!(
        "{}{}",
        check("kind='doc.changed_only'\npaths=['stature']\n"),
        "[[check]]\nid='d'\nkind='doc.change'\npath='stature'\nchange='up'\n"
    );
    let head = "id = 't'\nfeature = 'QG-03'\ntitle = 't'\nprompt = 'p'\nfixture = 'f'\nscopes = ['read']\n";
    let task: Task = toml::from_str(&format!("{head}{checks}")).unwrap();
    let run =
        || Checker::new(runner(&fixture()), runner(&result()), Transcript::default()).grade(&task).to_json().unwrap();
    assert_eq!(run(), run());
}
