//! The English-file check (design 10.1) and blessing (design 3.3): the committed `locales/en/<package>.ftl` must be what
//! extraction generates, and `OMUZAKI_BLESS=1` rewrites it and adds stubs for the shipped languages that have no file.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::Failure;
use crate::SHIPPED;
use crate::extract::{PackageText, english_of};

/// The check's name in a [`Failure`].
const CHECK: &str = "english";

/// Compares `<manifest_dir>/locales/en/<package>.ftl` with `extract::english_of(text)`.
///
/// With `bless`, writes that file instead, and creates an empty `locales/<lang>/<package>.ftl` for each locale of
/// `SHIPPED` (by `Locale::as_str`) that lacks one. It returns no failures unless a write fails.
pub fn english_current(text: &PackageText, manifest_dir: &Path, bless: bool) -> Vec<Failure> {
    let expected = english_of(text);
    if bless {
        return bless_files(text, manifest_dir, &expected);
    }
    let path = file_path(manifest_dir, "en", &text.package);
    let id = format!("locales/en/{}.ftl", text.package);
    let command = format!("run `OMUZAKI_BLESS=1 cargo test -p {} --test l10n`", text.package);
    match fs::read_to_string(&path) {
        Ok(actual) if actual == expected => Vec::new(),
        Ok(actual) => {
            let line = first_difference(&actual, &expected);
            vec![Failure::new(CHECK, id, format!("is out of date (first difference at line {line}): {command}"))]
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            vec![Failure::new(CHECK, id, format!("is out of date: {command}"))]
        }
        Err(error) => vec![Failure::new(CHECK, id, format!("cannot be read: {error}"))],
    }
}

/// The path of `locales/<locale>/<package>.ftl` under the crate's manifest directory.
fn file_path(manifest_dir: &Path, locale: &str, package: &str) -> PathBuf {
    manifest_dir.join("locales").join(locale).join(format!("{package}.ftl"))
}

/// The 1-based line where `actual` first differs from `expected`. A file that is a prefix of the other differs on the
/// first line the shorter one lacks.
fn first_difference(actual: &str, expected: &str) -> usize {
    let mut line = 1;
    let mut actual_lines = actual.lines();
    let mut expected_lines = expected.lines();
    loop {
        match (actual_lines.next(), expected_lines.next()) {
            (Some(a), Some(e)) if a == e => line += 1,
            _ => return line,
        }
    }
}

/// Writes the English file when its content differs, and an empty file for each shipped language that has none. A
/// translation that exists is never touched.
fn bless_files(text: &PackageText, manifest_dir: &Path, expected: &str) -> Vec<Failure> {
    let mut failures = Vec::new();
    let english = file_path(manifest_dir, "en", &text.package);
    if let Err(error) = write_if_changed(&english, expected) {
        failures.push(Failure::new(
            CHECK,
            format!("locales/en/{}.ftl", text.package),
            format!("cannot be written: {error}"),
        ));
    }
    for locale in SHIPPED {
        let tag = locale.as_str();
        if tag == "en" {
            continue;
        }
        let stub = file_path(manifest_dir, tag, &text.package);
        // A translation that exists is left alone; only a missing language gets a stub.
        if stub.exists() {
            continue;
        }
        if let Err(error) = write_if_changed(&stub, "") {
            failures.push(Failure::new(
                CHECK,
                format!("locales/{tag}/{}.ftl", text.package),
                format!("cannot be written: {error}"),
            ));
        }
    }
    failures
}

/// Writes `content` to `path`, creating its folder, unless the file already holds exactly `content`. Leaving an
/// unchanged file alone keeps its modification time.
fn write_if_changed(path: &Path, content: &str) -> io::Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)
}
