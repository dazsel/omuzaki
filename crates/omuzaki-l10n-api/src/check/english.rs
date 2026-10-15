//! The `english` check (design 10.1) and blessing (design 3.3): the committed English file is the generated one.

use std::fs;
use std::io::{self, ErrorKind, Write};
use std::path::Path;

use super::Failure;
use crate::extract::{PackageText, english_of};
use crate::{Locale, SHIPPED};

/// The check's name in a [`Failure`].
const CHECK: &str = "english";

/// Compares `<manifest_dir>/locales/en/<package>.ftl` with [`english_of`]`(text)`.
///
/// With `bless`, writes that file instead, and creates an empty `locales/<lang>/<package>.ftl` for each locale of
/// [`SHIPPED`] (by [`Locale::as_str`]) that lacks one. It returns no failures unless a write fails. Blessing writes
/// only when the content differs and never touches an existing translation, so modification times stay put.
pub fn english_current(text: &PackageText, manifest_dir: &Path, bless: bool) -> Vec<Failure> {
    let expected = english_of(text);
    let relative = locale_file(&Locale::EN, &text.package);
    let path = manifest_dir.join(&relative);
    let current = match fs::read_to_string(&path) {
        Ok(current) => Some(current),
        Err(error) if error.kind() == ErrorKind::NotFound => None,
        Err(error) => return vec![Failure::new(CHECK, relative, format!("cannot be read: {error}"))],
    };
    if bless {
        return write_all(text, manifest_dir, &relative, &expected, current.as_deref());
    }
    match current {
        Some(current) if current == expected => Vec::new(),
        found => {
            let line =
                found.map(|current| format!(" (first difference at line {})", first_difference(&current, &expected)));
            let rerun = format!("OMUZAKI_BLESS=1 cargo test -p {} --test l10n", text.package);
            vec![Failure::new(CHECK, relative, format!("is out of date{}: run `{rerun}`", line.unwrap_or_default()))]
        }
    }
}

/// `locales/<lang>/<package>.ftl`, with `/` on every platform so the failure reads the same everywhere.
fn locale_file(locale: &Locale, package: &str) -> String {
    format!("locales/{}/{package}.ftl", locale.as_str())
}

/// The 1-based line of the first byte at which `a` and `b` differ.
fn first_difference(a: &str, b: &str) -> usize {
    let at = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    1 + b.bytes().take(at).filter(|&byte| byte == b'\n').count()
}

/// Writes the English file when it differs, then the missing stubs.
fn write_all(
    text: &PackageText,
    manifest_dir: &Path,
    relative: &str,
    expected: &str,
    current: Option<&str>,
) -> Vec<Failure> {
    let mut failures = Vec::new();
    if current != Some(expected)
        && let Err(error) = write_file(&manifest_dir.join(relative), expected)
    {
        failures.push(Failure::new(CHECK, relative, format!("cannot be written: {error}")));
    }
    for locale in SHIPPED.iter().filter(|locale| **locale != Locale::EN) {
        let stub = locale_file(locale, &text.package);
        if let Err(error) = create_stub(&manifest_dir.join(&stub)) {
            failures.push(Failure::new(CHECK, stub, format!("cannot be created: {error}")));
        }
    }
    failures
}

/// Writes `content` to `path`, creating its folder.
fn write_file(path: &Path, content: &str) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    fs::write(path, content)
}

/// Creates an empty file at `path` unless one is there, whatever it holds.
fn create_stub(path: &Path) -> io::Result<()> {
    if let Some(folder) = path.parent() {
        fs::create_dir_all(folder)?;
    }
    match fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.flush(),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error),
    }
}
