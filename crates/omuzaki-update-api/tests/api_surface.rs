//! The public-API snapshot: every `pub` item declaration in `src/`, and every method of a `pub trait`, compared with
//! `tests/api_surface.txt`.
//!
//! A change to the contract shows up as a diff of that file in the PR, which is what the API review lane reads. After an
//! intended change, regenerate with `UPDATE_API_SURFACE=1 cargo test -p omuzaki-update-api --test api_surface`.
//! It is a source scan, not a type-level check, so it sees declarations (and their signatures' first lines), which is
//! what a reviewer needs to notice an added, removed or renamed item. Trait members have no `pub` of their own, so the
//! scan follows each `pub trait` body by its braces and records its `fn` lines, prefixed with the trait's name.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const KEYWORDS: [&str; 9] =
    ["pub fn", "pub const fn", "pub struct", "pub enum", "pub trait", "pub type", "pub const", "pub static", "pub mod"];

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// A snapshot-relative path with `/` separators on every platform (`Path::display` uses `\\` on Windows).
fn slashed(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

/// The net count of `{` minus `}` on a line.
fn braces(line: &str) -> i32 {
    line.bytes()
        .map(|b| match b {
            b'{' => 1,
            b'}' => -1,
            _ => 0,
        })
        .sum()
}

/// The snapshot lines of one source file: its `pub` declarations, then the `fn` members of each `pub trait` it declares.
fn scan(rel: &str, source: &str) -> String {
    // Items inside `#[cfg(test)] mod tests` are not public API.
    let body = source.split("#[cfg(test)]").next().unwrap_or("");
    let mut text = String::new();
    // The trait whose body is being read, with its brace depth: members sit at depth 1.
    let mut in_trait: Option<(String, i32)> = None;
    for line in body.lines().map(str::trim) {
        if let Some((name, depth)) = in_trait.as_mut() {
            if *depth == 1 && line.starts_with("fn ") {
                writeln!(text, "{rel}: {name}: {line}").unwrap();
            }
            *depth += braces(line);
            if *depth <= 0 {
                in_trait = None;
            }
            continue;
        }
        let is_item = KEYWORDS.iter().any(|k| line.starts_with(k))
            || line.starts_with("macro_rules!")
            || line.starts_with("pub use");
        if is_item && !line.starts_with("pub(crate)") {
            writeln!(text, "{rel}: {line}").unwrap();
        }
        if let Some(rest) = line.strip_prefix("pub trait ") {
            let name = rest.split(|c: char| !(c.is_alphanumeric() || c == '_')).next().unwrap_or_default();
            in_trait = Some((name.to_owned(), braces(line)));
        }
    }
    text
}

fn surface() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut text = String::new();
    for file in files {
        let rel = slashed(file.strip_prefix(root).unwrap());
        let source = std::fs::read_to_string(&file).unwrap();
        text.push_str(&scan(&rel, &source));
    }
    text
}

#[test]
fn the_public_surface_matches_the_snapshot() {
    let snapshot = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/api_surface.txt");
    let now = surface();
    if std::env::var_os("UPDATE_API_SURFACE").is_some() {
        std::fs::write(&snapshot, &now).unwrap();
        return;
    }
    let before = std::fs::read_to_string(&snapshot).unwrap_or_default();
    assert!(before == now, "the public API changed: review the diff, then regenerate with UPDATE_API_SURFACE=1");
}

#[test]
fn snapshot_paths_use_forward_slashes() {
    assert_eq!(slashed(Path::new("src\\error.rs")), "src/error.rs");
    assert_eq!(slashed(Path::new("src/error.rs")), "src/error.rs");
}

#[test]
fn trait_members_are_in_the_surface() {
    let source = "pub trait Store: Send {\n    /// Reads.\n    fn read(&self) -> u8;\n    fn write(&self, v: u8) {\n        let x = 1;\n    }\n}\npub fn after() {}\n";
    let text = scan("src/store.rs", source);
    assert!(text.contains("src/store.rs: Store: fn read(&self) -> u8;\n"), "{text}");
    assert!(text.contains("src/store.rs: Store: fn write(&self, v: u8) {\n"), "{text}");
    assert!(text.contains("src/store.rs: pub fn after() {}\n"), "{text}");
    assert!(!text.contains("let x"), "a default body is not a declaration: {text}");
}

#[test]
fn renaming_or_removing_a_trait_method_changes_the_surface() {
    let before = "pub trait Store {\n    fn download(&self) -> u8;\n}\n";
    let renamed = "pub trait Store {\n    fn fetch(&self) -> u8;\n}\n";
    let removed = "pub trait Store {\n}\n";
    let text = scan("src/store.rs", before);
    assert_ne!(text, scan("src/store.rs", renamed));
    assert_ne!(text, scan("src/store.rs", removed));
}
