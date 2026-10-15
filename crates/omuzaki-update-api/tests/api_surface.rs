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

/// Whether `text` starts a method declaration: `fn`, optionally after `pub`, `const`, `async`, `unsafe`, `default`
/// and `extern "ABI"` qualifiers in any combination.
fn is_method(text: &str) -> bool {
    let mut rest = text.trim_start();
    loop {
        if rest.starts_with("fn ") {
            return true;
        }
        if let Some(after) = rest.strip_prefix("extern ") {
            rest = after.trim_start();
            if rest.starts_with('"') {
                match rest[1..].split_once('"') {
                    Some((_, tail)) => rest = tail.trim_start(),
                    None => return false,
                }
            }
            continue;
        }
        match ["pub ", "const ", "async ", "unsafe ", "default "].iter().find_map(|q| rest.strip_prefix(q)) {
            Some(after) => rest = after.trim_start(),
            None => return false,
        }
    }
}

/// The snapshot lines of one source file: its `pub` declarations, then the method members of each `pub trait` it
/// declares.
fn scan(rel: &str, source: &str) -> String {
    // Items inside `#[cfg(test)] mod tests` are not public API.
    let body = source.split("#[cfg(test)]").next().unwrap_or("");
    let mut text = String::new();
    // The trait whose body is being read: its name, its brace depth (members sit at depth 1) and whether its opening
    // brace has been seen (a header can run over several lines).
    let mut in_trait: Option<(String, i32, bool)> = None;
    for line in body.lines().map(str::trim) {
        if let Some((name, depth, opened)) = in_trait.as_mut() {
            if *depth == 1 && is_method(line) {
                writeln!(text, "{rel}: {name}: {line}").unwrap();
            }
            *depth += braces(line);
            *opened |= line.contains('{');
            if *opened && *depth <= 0 {
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
            let depth = braces(line);
            if let (Some(open), Some(close)) = (line.find('{'), line.rfind('}')) {
                // The whole body is on this line: record its methods and stay out of trait mode.
                if open < close {
                    for member in line[open + 1..close].split(';').filter(|m| is_method(m)) {
                        writeln!(text, "{rel}: {name}: {};", member.trim()).unwrap();
                    }
                }
            }
            if depth > 0 || !line.contains('{') && !line.ends_with(';') {
                in_trait = Some((name.to_owned(), depth, line.contains('{')));
            }
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

#[test]
fn qualified_trait_methods_are_in_the_surface() {
    let source = "pub trait Io {\n    async fn a(&self);\n    unsafe fn b(&self);\n    const fn c();\n    pub fn d();\n    extern \"C\" fn e();\n    pub const unsafe extern \"C\" fn f();\n    const X: u8;\n}\n";
    let text = scan("src/io.rs", source);
    for name in ["a", "b", "c", "d", "e", "f"] {
        assert!(text.contains(&format!("{name}(")), "{name} missing: {text}");
    }
    assert!(!text.contains("const X"), "{text}");
    let renamed = source.replace("async fn a", "async fn z");
    let removed = source.replace("    unsafe fn b(&self);\n", "");
    assert_ne!(text, scan("src/io.rs", &renamed));
    assert_ne!(text, scan("src/io.rs", &removed));
}

#[test]
fn a_one_line_trait_does_not_swallow_later_items() {
    let text = scan("src/m.rs", "pub trait Marker {}\npub fn after() {}\npub struct S;\n");
    assert!(text.contains("src/m.rs: pub trait Marker {}\n"), "{text}");
    assert!(text.contains("src/m.rs: pub fn after() {}\n"), "{text}");
    assert!(text.contains("src/m.rs: pub struct S;\n"), "{text}");
    let text = scan("src/m.rs", "pub trait A { fn f(&self); }\npub fn after() {}\n");
    assert!(text.contains("src/m.rs: A: fn f(&self);\n"), "{text}");
    assert!(text.contains("src/m.rs: pub fn after() {}\n"), "{text}");
}

#[test]
fn a_trait_header_over_several_lines_is_followed() {
    let text = scan("src/m.rs", "pub trait T\nwhere\n    Self: Sized,\n{\n    fn f(&self);\n}\npub fn after() {}\n");
    assert!(text.contains("src/m.rs: T: fn f(&self);\n"), "{text}");
    assert!(text.contains("src/m.rs: pub fn after() {}\n"), "{text}");
}
