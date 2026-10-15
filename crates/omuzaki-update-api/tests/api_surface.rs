//! The public-API snapshot: every `pub` item declaration in `src/`, and every member of a `pub trait`, compared with
//! `tests/api_surface.txt`.
//!
//! A change to the contract shows up as a diff of that file in the PR, which is what the API review lane reads. After an
//! intended change, regenerate with `UPDATE_API_SURFACE=1 cargo test -p omuzaki-update-api --test api_surface`.
//!
//! It is a source scan, not a type-level check (`syn` is not a workspace dependency, and this crate adds none). Before
//! scanning, a small tokenizer-style pass blanks comments (line, block, nested, doc), string literals (plain, byte and
//! raw) and char literals, so braces and keywords inside them are never counted. Item lines are then matched on the
//! blanked text and recorded as written. Each `pub trait` / `pub unsafe trait` body is walked by its braces and every
//! member at depth 1 (methods with any qualifiers, consts, types) is recorded as its whole header, with the trait's
//! name, so a one-line trait and a multi-line signature are seen.
//!
//! Limits: the scan is line-oriented outside trait bodies, so an item that does not start its line (`{ pub fn a() {} }`),
//! or a `pub` item on the same line after a trait's closing brace, is missed; `pub(crate)` and other restricted
//! visibilities are skipped on purpose; items produced by macros are not seen; the scan stops at the first
//! `#[cfg(test)]`; a brace-delimited const expression in a trait member is recorded as `{ .. }`.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const KEYWORDS: [&str; 17] = [
    "pub fn",
    "pub const fn",
    "pub async fn",
    "pub unsafe fn",
    "pub const unsafe fn",
    "pub extern",
    "pub unsafe extern",
    "pub struct",
    "pub enum",
    "pub union",
    "pub trait",
    "pub unsafe trait",
    "pub type",
    "pub const",
    "pub static",
    "pub mod",
    "pub use",
];

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

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// If a raw string starts at `i` (`r"`, `r#"`, `br"`), the index just past its opening quote and its hash count.
fn raw_open(c: &[char], i: usize) -> Option<(usize, usize)> {
    let mut j = i;
    if c.get(j) == Some(&'b') {
        j += 1;
    }
    if c.get(j) != Some(&'r') {
        return None;
    }
    j += 1;
    let hashes = c[j..].iter().take_while(|&&ch| ch == '#').count();
    j += hashes;
    (c.get(j) == Some(&'"')).then_some((j + 1, hashes))
}

/// `source` with comments replaced by a space, string literals by `""` and char literals by `''`. Newlines are kept, so
/// line `n` of the result is line `n` of the input.
fn strip(source: &str) -> String {
    let c: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < c.len() {
        let prev_ident = out.chars().next_back().is_some_and(is_ident);
        if c[i] == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            out.push(' ');
        } else if c[i] == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if c[i] == '\n' {
                        out.push('\n');
                    }
                    i += 1;
                }
            }
            out.push(' ');
        } else if let Some((body, hashes)) = (!prev_ident).then(|| raw_open(&c, i)).flatten() {
            i = body;
            while i < c.len() && !(c[i] == '"' && c[i + 1..].iter().take_while(|&&ch| ch == '#').count() >= hashes) {
                if c[i] == '\n' {
                    out.push('\n');
                }
                i += 1;
            }
            i = (i + 1 + hashes).min(c.len());
            out.push_str("\"\"");
        } else if c[i] == '"' {
            i += 1;
            while i < c.len() && c[i] != '"' {
                if c[i] == '\\' {
                    i += 1;
                }
                if c.get(i) == Some(&'\n') {
                    out.push('\n');
                }
                i += 1;
            }
            i += 1;
            out.push_str("\"\"");
        } else if c[i] == '\'' && (c.get(i + 1) == Some(&'\\') || c.get(i + 2) == Some(&'\'')) {
            // A char literal; `'a` and `'static` (no closing quote two on) are lifetimes and fall through.
            i += if c[i + 1] == '\\' { 2 } else { 1 };
            while i < c.len() && c[i] != '\'' {
                i += 1;
            }
            i += 1;
            out.push_str("''");
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

/// `text` without leading `#[..]` attributes, whitespace collapsed.
fn header(text: &str) -> String {
    let mut rest = text.trim_start();
    while rest.starts_with('#') {
        let mut depth = 0;
        let end = rest.char_indices().find_map(|(i, ch)| {
            match ch {
                '[' => depth += 1,
                ']' => depth -= 1,
                _ => {}
            }
            (ch == ']' && depth == 0).then_some(i + 1)
        });
        rest = end.map_or("", |e| rest[e..].trim_start());
    }
    rest.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The members at depth 1 of the trait body that opens at the first `{` of `text` (already stripped), each with its
/// terminator (`;` or `{ .. }`), and the byte offset just past the body's closing brace.
fn trait_members(text: &str) -> (Vec<String>, usize) {
    let mut members = Vec::new();
    let Some(open) = text.find('{') else { return (members, text.len()) };
    let (mut depth, mut nest, mut start) = (1, 0, open + 1);
    let mut push = |segment: &str, end: &str| {
        let head = header(segment);
        if !head.is_empty() {
            members.push(format!("{head}{end}"));
        }
    };
    for (i, ch) in text.char_indices().skip_while(|&(i, _)| i <= open) {
        match ch {
            '(' | '[' => nest += 1,
            ')' | ']' => nest -= 1,
            '{' => {
                if depth == 1 && nest == 0 {
                    push(&text[start..i], " { .. }");
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return (members, i + 1);
                }
                if depth == 1 {
                    start = i + 1;
                }
            }
            ';' if depth == 1 && nest == 0 => {
                push(&text[start..i], ";");
                start = i + 1;
            }
            _ => {}
        }
    }
    (members, text.len())
}

/// The snapshot lines of one source file: its `pub` declarations and the members of each `pub trait` it declares.
fn scan(rel: &str, source: &str) -> String {
    let stripped = strip(source);
    // Items inside `#[cfg(test)] mod tests` are not public API.
    let stripped = stripped.split("#[cfg(test)]").next().unwrap_or("");
    let lines: Vec<&str> = stripped.lines().map(str::trim).collect();
    let originals: Vec<&str> = source.lines().map(str::trim).collect();
    let mut text = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let is_item = KEYWORDS.iter().any(|k| line.starts_with(k)) || line.starts_with("macro_rules!");
        if is_item {
            writeln!(text, "{rel}: {}", originals[i]).unwrap();
        }
        let trait_rest = line.strip_prefix("pub trait ").or_else(|| line.strip_prefix("pub unsafe trait "));
        if let Some(rest) = trait_rest {
            let name = rest.split(|c: char| !is_ident(c)).next().unwrap_or_default();
            let (members, end) = trait_members(&lines[i..].join("\n"));
            for member in members {
                writeln!(text, "{rel}: {name}: {member}").unwrap();
            }
            // Resume on the line after the one holding the closing brace.
            i += lines[i..].join("\n")[..end].matches('\n').count();
        }
        i += 1;
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
    assert!(text.contains("src/store.rs: Store: fn write(&self, v: u8) { .. }\n"), "{text}");
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
fn unsafe_traits_and_qualified_members_are_recorded() {
    let source = "pub unsafe trait Raw {\n    async fn a(&self);\n    unsafe fn b(&self);\n    const fn c();\n    unsafe extern \"C\" fn d();\n}\npub async fn later() {}\npub unsafe fn later2() {}\n";
    let text = scan("src/raw.rs", source);
    for want in [
        "src/raw.rs: pub unsafe trait Raw {\n",
        "src/raw.rs: Raw: async fn a(&self);\n",
        "src/raw.rs: Raw: unsafe fn b(&self);\n",
        "src/raw.rs: Raw: const fn c();\n",
        "src/raw.rs: Raw: unsafe extern \"\" fn d();\n",
        "src/raw.rs: pub async fn later() {}\n",
        "src/raw.rs: pub unsafe fn later2() {}\n",
    ] {
        assert!(text.contains(want), "missing {want:?} in {text}");
    }
}

#[test]
fn a_one_line_trait_and_the_item_after_it_are_recorded() {
    let text = scan("src/t.rs", "pub trait One { fn a(&self); fn b(&self) -> u8 { 1 } }\npub fn after() {}\n");
    assert!(text.contains("src/t.rs: One: fn a(&self);\n"), "{text}");
    assert!(text.contains("src/t.rs: One: fn b(&self) -> u8 { .. }\n"), "{text}");
    assert!(text.contains("src/t.rs: pub fn after() {}\n"), "{text}");
}

#[test]
fn braces_in_literals_and_comments_do_not_end_a_trait() {
    let source = concat!(
        "pub trait Tricky {\n",
        "    /// Doc with a stray } and a { brace.\n",
        "    const A: &'static str = \"}\";\n",
        "    const B: char = '}';\n",
        "    const C: &str = r#\"} \"quoted\" {\"#;\n",
        "    // line comment with }\n",
        "    /* block } comment { */\n",
        "    fn first(&self);\n",
        "}\n",
        "pub fn after_strings() {}\n",
        "pub trait Second {\n",
        "    const D: u8 = { 1 };\n",
        "    fn second(&self);\n",
        "}\n",
        "pub fn last() {}\n",
    );
    let text = scan("src/t.rs", source);
    for want in [
        "src/t.rs: Tricky: const A: &'static str = \"\";\n",
        "src/t.rs: Tricky: const B: char = '';\n",
        "src/t.rs: Tricky: fn first(&self);\n",
        "src/t.rs: pub fn after_strings() {}\n",
        "src/t.rs: Second: const D: u8 = { .. }\n",
        "src/t.rs: Second: fn second(&self);\n",
        "src/t.rs: pub fn last() {}\n",
    ] {
        assert!(text.contains(want), "missing {want:?} in {text}");
    }
    assert!(!text.contains("stray") && !text.contains("comment"), "{text}");
}

#[test]
fn pub_items_in_comments_and_strings_are_not_recorded() {
    let text = scan(
        "src/t.rs",
        "// pub fn ghost() {}\n/* pub struct Ghost; */\nconst S: &str = \"\n pub fn inner() {}\n\";\npub fn real() {}\n",
    );
    assert_eq!(text, "src/t.rs: pub fn real() {}\n");
}

#[test]
fn a_multi_line_signature_is_recorded_whole_and_arrays_do_not_split_it() {
    let text =
        scan("src/t.rs", "pub trait Wide {\n    fn put(\n        &self,\n        key: [u8; 4],\n    ) -> u8;\n}\n");
    assert!(text.contains("src/t.rs: Wide: fn put( &self, key: [u8; 4], ) -> u8;\n"), "{text}");
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
