//! The public-API snapshot: every `pub` item declaration in `src/`, compared with `tests/api_surface.txt`.
//!
//! A change to the contract shows up as a diff of that file in the PR, which is what the API review lane reads. After an
//! intended change, regenerate with `UPDATE_API_SURFACE=1 cargo test -p omuzaki-llm-mock --test api_surface`.
//! It is a source scan, not a type-level check, so it sees declarations (and their signatures' first lines), which is
//! what a reviewer needs to notice an added, removed or renamed item.

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

fn surface() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    sources(&root.join("src"), &mut files);
    let mut text = String::new();
    for file in files {
        // `/` on every platform, so the snapshot is the same on Windows (`display()` would give a backslash there).
        let rel = file.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
        let source = std::fs::read_to_string(&file).unwrap();
        // Items inside `#[cfg(test)] mod tests` are not public API.
        let body = source.split("#[cfg(test)]").next().unwrap_or("");
        for line in body.lines().map(str::trim) {
            let is_item = KEYWORDS.iter().any(|k| line.starts_with(k))
                || line.starts_with("macro_rules!")
                || line.starts_with("pub use");
            if is_item && !line.starts_with("pub(crate)") {
                writeln!(text, "{rel}: {line}").unwrap();
            }
        }
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
    // Git is told to check text out with LF (`.gitattributes`); the replace keeps a CRLF checkout from failing here.
    let before = std::fs::read_to_string(&snapshot).unwrap_or_default().replace("\r\n", "\n");
    assert!(before == now, "the public API changed: review the diff, then regenerate with UPDATE_API_SURFACE=1");
}
