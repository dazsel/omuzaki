//! The English-file check and blessing (`check::english_current`), in temporary crate directories.

use std::fs;
use std::path::Path;

use omuzaki_l10n_api::check::{Failure, english_current, report};
use omuzaki_l10n_api::extract::{PackageText, english_of};
use omuzaki_l10n_api::{ArgKind, MessageDecl, SHIPPED};

/// The fixture package: one message, so the English file has content to compare.
fn text() -> PackageText {
    PackageText::new("omuzaki-fixture").with_messages(&[MessageDecl {
        id: "fixture-part-missing",
        english: "{ $part } is missing.",
        note: " Shown when a part is missing.",
        args: &[("part", ArgKind::Id)],
        verbatim: false,
    }])
}

/// The English file's path in a crate directory.
fn en_file(dir: &Path) -> std::path::PathBuf {
    dir.join("locales/en/omuzaki-fixture.ftl")
}

/// Asserts that a check found nothing, printing what it found otherwise.
fn assert_clean(failures: &[Failure]) {
    assert!(failures.is_empty(), "{}", report(failures));
}

/// Asserts that `failures` is exactly one failure about the English file, with a problem containing `part`.
fn assert_one(failures: &[Failure], part: &str) {
    assert_eq!(failures.len(), 1, "expected one failure, got:\n{}", report(failures));
    assert_eq!(failures[0].check, "english");
    assert_eq!(failures[0].id, "locales/en/omuzaki-fixture.ftl");
    assert!(failures[0].problem.contains(part), "problem `{}` lacks `{part}`", failures[0].problem);
}

#[test]
fn a_current_file_passes() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    fs::create_dir_all(dir.path().join("locales/en")).expect("a folder");
    fs::write(en_file(dir.path()), english_of(&text())).expect("a file");

    assert_clean(&english_current(&text(), dir.path(), false));
}

#[test]
fn a_stale_file_fails_naming_the_first_different_line() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    fs::create_dir_all(dir.path().join("locales/en")).expect("a folder");
    // Only the English changes: the note also says "is missing", so the match is on the whole English value.
    let stale = english_of(&text()).replace("{ $part } is missing.", "{ $part } went missing.");
    fs::write(en_file(dir.path()), stale).expect("a file");

    let failures = english_current(&text(), dir.path(), false);
    assert_one(&failures, "first difference at line 8");
    assert!(failures[0].problem.contains("OMUZAKI_BLESS=1 cargo test -p omuzaki-fixture --test l10n"));
}

#[test]
fn a_missing_file_fails() {
    let dir = tempfile::tempdir().expect("a temporary directory");

    let failures = english_current(&text(), dir.path(), false);
    assert_one(&failures, "is out of date: run");
    assert!(!failures[0].problem.contains("line"));
}

#[test]
fn bless_writes_the_english_and_stubs_only_the_missing_languages() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    fs::create_dir_all(dir.path().join("locales/ja")).expect("a folder");
    fs::write(dir.path().join("locales/ja/omuzaki-fixture.ftl"), "existing = translation\n").expect("a file");

    assert_clean(&english_current(&text(), dir.path(), true));

    assert_eq!(fs::read_to_string(en_file(dir.path())).expect("the English file"), english_of(&text()));
    assert_eq!(
        fs::read_to_string(dir.path().join("locales/ja/omuzaki-fixture.ftl")).expect("the Japanese file"),
        "existing = translation\n",
        "an existing translation is never touched"
    );
    for locale in SHIPPED.iter().filter(|l| l.as_str() != "en" && l.as_str() != "ja") {
        let stub = dir.path().join("locales").join(locale.as_str()).join("omuzaki-fixture.ftl");
        assert_eq!(fs::read_to_string(stub).expect("a stub"), "", "{} stub", locale.as_str());
    }
    // The check passes once blessed.
    assert_clean(&english_current(&text(), dir.path(), false));
}

#[test]
fn bless_twice_changes_nothing() {
    let dir = tempfile::tempdir().expect("a temporary directory");
    english_current(&text(), dir.path(), true);
    let before = fs::metadata(en_file(dir.path())).expect("metadata").modified().expect("mtime");

    assert_clean(&english_current(&text(), dir.path(), true));

    let after = fs::metadata(en_file(dir.path())).expect("metadata").modified().expect("mtime");
    assert_eq!(before, after, "an unchanged file keeps its modification time");
    assert_eq!(fs::read_to_string(en_file(dir.path())).expect("the English file"), english_of(&text()));
}
