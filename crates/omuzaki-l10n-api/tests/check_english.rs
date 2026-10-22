//! The `english_current` check and blessing (`check::english_current`), in temporary folders.

#![allow(clippy::expect_used)] // helper fns outside `#[test]` fns (clippy.toml only exempts tests)

use std::fs;
use std::path::Path;

use omuzaki_l10n_api::check::{Failure, english_current};
use omuzaki_l10n_api::extract::{PackageText, english_of};
use omuzaki_l10n_api::{MessageDecl, SHIPPED};

const DECLS: [MessageDecl; 2] = [
    MessageDecl { id: "fixture-a", english: "Alpha.", note: " First.", args: &[], verbatim: false },
    MessageDecl { id: "fixture-b", english: "Beta.", note: " Second.", args: &[], verbatim: false },
];

fn text() -> PackageText {
    PackageText::new("omuzaki-fixture").with_messages(&DECLS)
}

/// Asserts that `failures` is empty.
fn assert_clean(failures: &[Failure]) {
    assert!(failures.is_empty(), "{failures:?}");
}

fn english_path(dir: &Path) -> std::path::PathBuf {
    dir.join("locales/en/omuzaki-fixture.ftl")
}

fn write(path: &Path, content: &str) {
    fs::create_dir_all(path.parent().expect("a file has a parent")).expect("create the folder");
    fs::write(path, content).expect("write the file");
}

#[test]
fn a_current_file_passes() {
    let dir = tempfile::tempdir().expect("a temp dir");
    write(&english_path(dir.path()), &english_of(&text()));
    assert_clean(&english_current(&text(), dir.path(), false));
}

#[test]
fn a_stale_file_fails_naming_the_first_different_line() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let expected = english_of(&text());
    let stale = expected.replace("Beta.", "Gamma.");
    write(&english_path(dir.path()), &stale);
    let line = expected.lines().position(|l| l.contains("Beta.")).expect("the line") + 1;
    let failures = english_current(&text(), dir.path(), false);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].id, "locales/en/omuzaki-fixture.ftl");
    assert_eq!(
        failures[0].problem,
        format!(
            "is out of date (first difference at line {line}): run `OMUZAKI_BLESS=1 cargo test -p omuzaki-fixture --test l10n`"
        )
    );
}

#[test]
fn a_file_with_a_missing_tail_names_the_line_after_it() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let expected = english_of(&text());
    write(&english_path(dir.path()), &expected[..expected.len() - 1]);
    let failures = english_current(&text(), dir.path(), false);
    assert_eq!(failures.len(), 1);
    assert!(failures[0].problem.contains(&format!("line {}", expected.lines().count())), "{}", failures[0].problem);
}

#[test]
fn a_missing_file_fails() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let failures = english_current(&text(), dir.path(), false);
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].problem, "is out of date: run `OMUZAKI_BLESS=1 cargo test -p omuzaki-fixture --test l10n`");
}

#[test]
fn bless_writes_the_english_and_stubs_only_the_missing_languages() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let ja = dir.path().join("locales/ja/omuzaki-fixture.ftl");
    write(&ja, "fixture-a = アルファ\n");
    assert_clean(&english_current(&text(), dir.path(), true));
    assert_eq!(fs::read_to_string(english_path(dir.path())).expect("english"), english_of(&text()));
    assert_eq!(fs::read_to_string(&ja).expect("ja"), "fixture-a = アルファ\n");
    for locale in SHIPPED.iter().filter(|l| l.as_str() != "en" && l.as_str() != "ja") {
        let stub = dir.path().join(format!("locales/{}/omuzaki-fixture.ftl", locale.as_str()));
        assert_eq!(fs::read_to_string(&stub).expect("a stub"), "", "{}", locale.as_str());
    }
    assert_clean(&english_current(&text(), dir.path(), false));
}

#[test]
fn bless_twice_changes_nothing() {
    let dir = tempfile::tempdir().expect("a temp dir");
    assert_clean(&english_current(&text(), dir.path(), true));
    let files: Vec<_> =
        SHIPPED.iter().map(|l| dir.path().join(format!("locales/{}/omuzaki-fixture.ftl", l.as_str()))).collect();
    let before: Vec<_> = files.iter().map(|f| fs::metadata(f).and_then(|m| m.modified()).expect("mtime")).collect();
    assert_clean(&english_current(&text(), dir.path(), true));
    let after: Vec<_> = files.iter().map(|f| fs::metadata(f).and_then(|m| m.modified()).expect("mtime")).collect();
    assert_eq!(before, after);
}

#[test]
fn bless_reports_a_write_that_fails() {
    let dir = tempfile::tempdir().expect("a temp dir");
    // A file where the `locales` folder should be.
    write(&dir.path().join("locales"), "");
    let failures = english_current(&text(), dir.path(), true);
    assert!(!failures.is_empty(), "a blocked write must fail");
    assert!(failures.iter().all(|f| f.problem.contains("cannot be")), "{failures:?}");
}
