//! The public-API snapshot: every `pub` item declaration in `src/`, and every member of a `pub trait`, compared with
//! `tests/api_surface.txt`.
//!
//! A change to the contract shows up as a diff of that file in the PR, which is what the API review lane reads. After an
//! intended change, regenerate with `UPDATE_API_SURFACE=1 cargo test -p omuzaki-update-api --test api_surface`.
//!
//! It is a source scan, not a type-level check (`syn` is not a workspace dependency, and this crate adds none), so it
//! FAILS CLOSED: whatever it cannot place with confidence makes the test panic with a message naming the construct and
//! saying to extend the scanner, and never drops a member silently. Before scanning, a small tokenizer-style pass blanks
//! comments (line, block, nested, doc), string literals (plain, byte and raw) and char literals, so braces and keywords
//! inside them are never counted; an unterminated one panics. Item lines are then matched on the blanked text, after
//! leading `#[..]` attributes, and recorded as written. The body of each `pub trait` / `pub unsafe trait` opens at the
//! first `{` outside `<..>`, `(..)` and `[..]` after the header (so `Parent<{ 1 + 1 }>`, where-clauses and defaults like
//! `T = [u8; 4]` are read correctly); every member at depth 1 (methods with any qualifiers, consts, types) is recorded
//! as its whole header, with the trait's name.
//!
//! Panics (fail closed): an unbalanced bracket in a trait header or body, end of file before a body or `;`, a macro
//! invocation as a trait member, `macro_rules!` anywhere, a `pub` item that does not start its line (`{ pub fn a() {} }`,
//! or one after a trait's closing brace), a `#[cfg(test)]` that is not a trailing `mod` running to the end of the file
//! (everything after it is dropped as test code), an unterminated comment or literal.
//!
//! Limits that remain: `pub(crate)` and other restricted visibilities are skipped on purpose; items produced by
//! macros defined elsewhere are not seen; a brace-delimited const expression in a trait member is recorded as `{ .. }`.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const KEYWORDS: [&str; 16] = [
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

fn fail_strip(rel: &str, what: &str) -> ! {
    panic!("api_surface: unterminated {what} in {rel}; extend the scanner in tests/api_surface.rs, do not skip it")
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
/// line `n` of the result is line `n` of the input. An unterminated comment or literal panics.
fn strip(rel: &str, source: &str) -> String {
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
            loop {
                if i >= c.len() {
                    fail_strip(rel, "block comment");
                }
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
            if i >= c.len() {
                fail_strip(rel, "raw string");
            }
            i += 1 + hashes;
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
            if i >= c.len() {
                fail_strip(rel, "string literal");
            }
            i += 1;
            out.push_str("\"\"");
        } else if c[i] == '\'' && (c.get(i + 1) == Some(&'\\') || c.get(i + 2) == Some(&'\'')) {
            // A char literal; `'a` and `'static` (no closing quote two on) are lifetimes and fall through.
            i += if c[i + 1] == '\\' { 2 } else { 1 };
            while i < c.len() && c[i] != '\'' {
                i += 1;
            }
            if i >= c.len() {
                fail_strip(rel, "char literal");
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

/// `text` without its leading `#[..]` attributes.
fn skip_attrs(text: &str) -> &str {
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
    rest
}

/// `text` without leading `#[..]` attributes, whitespace collapsed.
fn header(text: &str) -> String {
    skip_attrs(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The byte offsets in `line` where a `pub` item declaration starts.
fn item_starts(line: &str) -> Vec<usize> {
    let mut starts = Vec::new();
    for key in KEYWORDS.iter().chain(&["pub use"]) {
        for (p, _) in line.match_indices(key) {
            let before = line[..p].chars().next_back().is_none_or(|ch| !is_ident(ch));
            let after = line[p + key.len()..].chars().next().is_none_or(|ch| !is_ident(ch));
            if before && after {
                starts.push(p);
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

/// Whether `segment` ends or contains a macro invocation (`name!(..)`, `name! {`), which can declare members unseen.
fn has_macro_call(segment: &str) -> bool {
    let chars: Vec<char> = segment.chars().collect();
    chars.iter().enumerate().any(|(k, &ch)| {
        ch == '!'
            && k > 0
            && is_ident(chars[k - 1])
            && chars[k + 1..].iter().find(|c| !c.is_whitespace()).is_none_or(|c| "([{".contains(*c))
    })
}

/// Everything before the trailing `#[cfg(test)] mod tests { .. }`, which is not public API. Panics when the first
/// `#[cfg(test)]` is not a `mod` that runs to the end of the file, since the rest of the file would be dropped.
fn cut_tests<'a>(rel: &str, stripped: &'a str) -> &'a str {
    const CFG: &str = "#[cfg(test)]";
    let Some(at) = stripped.find(CFG) else { return stripped };
    let tail = &stripped[at + CFG.len()..];
    let fail = |why: &str| -> ! {
        panic!("api_surface: {CFG} in {rel} {why}; extend the scanner in tests/api_surface.rs, do not skip it")
    };
    if !header(tail).starts_with("mod ") {
        fail("is not on a `mod`, so code after it would be dropped");
    }
    let mut depth = 0;
    for (i, ch) in tail.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if !tail[i + 1..].trim().is_empty() {
                        fail("is followed by more code after its module");
                    }
                    return &stripped[..at];
                }
            }
            ';' if depth == 0 => {
                if !tail[i + 1..].trim().is_empty() {
                    fail("is followed by more code after its module");
                }
                return &stripped[..at];
            }
            _ => {}
        }
    }
    fail("has an unbalanced module body")
}

/// The members at depth 1 of the `pub trait` whose declaration starts `text` (already stripped), each with its
/// terminator (`;` or `{ .. }`), and the byte offset just past the body's closing brace (or the `;` of a bodiless trait).
///
/// The body opens at the first `{` outside `<..>`, `(..)` and `[..]`; a `{` met inside them (a const-generic argument) and
/// its matching `}` are skipped. Anything it cannot place panics, naming the trait.
fn trait_members(name: &str, text: &str) -> (Vec<String>, usize) {
    let fail = |why: &str| -> ! {
        panic!(
            "api_surface: cannot place the body of `pub trait {name}`: {why}; extend the scanner in tests/api_surface.rs, \
             do not skip it"
        )
    };
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut members = Vec::new();
    // `depth` is 0 in the header and 1 at member level; angle/nest/skip are bracket depths (`skip` counts the braces of
    // an expression nested inside brackets).
    let (mut depth, mut angle, mut nest, mut skip, mut start) = (0, 0, 0, 0, 0);
    for (k, &(i, ch)) in chars.iter().enumerate() {
        let prev = k.checked_sub(1).map(|p| chars[p].1);
        let after = chars.get(k + 1).map(|&(_, c)| c);
        if skip > 0 {
            match ch {
                '{' => skip += 1,
                '}' => skip -= 1,
                _ => {}
            }
            continue;
        }
        let mut push = |segment: &str, end: &str| {
            let head = header(segment);
            if has_macro_call(&head) {
                fail(&format!("the member `{head}` is a macro invocation"));
            }
            if !head.is_empty() {
                members.push(format!("{head}{end}"));
            }
        };
        match ch {
            '<' if depth <= 1 && after != Some('=') => angle += 1,
            '>' if depth <= 1 && prev != Some('-') && prev != Some('=') && after != Some('=') => {
                if angle == 0 && depth == 0 {
                    fail("unbalanced `>` in the header");
                }
                angle = (angle - 1).max(0);
            }
            '(' | '[' => nest += 1,
            ')' | ']' => {
                nest -= 1;
                if nest < 0 {
                    fail("unbalanced `)` or `]`");
                }
            }
            '{' if angle > 0 || nest > 0 => skip = 1,
            '{' if depth == 0 => {
                depth = 1;
                start = i + 1;
            }
            '{' => {
                if depth == 1 {
                    push(&text[start..i], " { .. }");
                }
                depth += 1;
            }
            '}' => {
                if depth == 0 {
                    fail("unbalanced `}` in the header");
                }
                depth -= 1;
                if depth == 0 {
                    return (members, i + 1);
                }
                if depth == 1 {
                    start = i + 1;
                    angle = 0;
                }
            }
            ';' if depth == 0 && nest == 0 => {
                if angle != 0 {
                    fail("`;` inside an unbalanced `<` in the header");
                }
                return (members, i + 1);
            }
            ';' if depth == 1 && nest == 0 => {
                push(&text[start..i], ";");
                start = i + 1;
                angle = 0;
            }
            _ => {}
        }
    }
    if depth == 0 {
        fail("end of file before a body or `;`");
    }
    fail("end of file inside the body")
}

/// The snapshot lines of one source file: its `pub` declarations and the members of each `pub trait` it declares.
fn scan(rel: &str, source: &str) -> String {
    let stripped = strip(rel, source);
    let stripped = cut_tests(rel, &stripped);
    let lines: Vec<&str> = stripped.lines().map(str::trim).collect();
    let originals: Vec<&str> = source.lines().collect();
    let stray = |line: &str, what: &str| -> ! {
        panic!(
            "api_surface: {what} in {rel}: `{line}`; a pub item must start its line (and `macro_rules!` is not scanned); \
             extend the scanner in tests/api_surface.rs, do not skip it"
        )
    };
    let mut text = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = header(lines[i]);
        if line.contains("macro_rules!") {
            stray(&line, "a `macro_rules!` definition can declare items the scan cannot see");
        }
        let starts = item_starts(&line);
        if starts.iter().any(|&p| p != 0) {
            stray(&line, "a `pub` item that does not start its line");
        }
        if !starts.is_empty() {
            writeln!(text, "{rel}: {}", header(originals[i])).unwrap();
        }
        let trait_rest = line.strip_prefix("pub trait ").or_else(|| line.strip_prefix("pub unsafe trait "));
        if let Some(rest) = trait_rest {
            let name = rest.split(|c: char| !is_ident(c)).next().unwrap_or_default();
            if name.is_empty() {
                stray(&line, "a `pub trait` whose name is not on its line");
            }
            let joined = lines[i..].join("\n");
            let (members, end) = trait_members(name, &joined);
            for member in members {
                writeln!(text, "{rel}: {name}: {member}").unwrap();
            }
            let after = joined[end..].lines().next().unwrap_or_default();
            if !item_starts(after).is_empty() || after.contains("macro_rules!") {
                stray(after, "a `pub` item after a trait's closing brace on the same line");
            }
            // Resume on the line after the one holding the closing brace.
            i += joined[..end].matches('\n').count();
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

#[test]
fn a_const_generic_expression_in_the_header_is_not_the_body() {
    let text = scan("src/c.rs", "pub trait Child: Parent<{ 1 + 1 }> { fn kept(&self); }\npub fn after() {}\n");
    assert!(text.contains("src/c.rs: Child: fn kept(&self);\n"), "{text}");
    assert!(text.contains("src/c.rs: pub fn after() {}\n"), "{text}");
    let text = scan("src/c.rs", "pub trait Child: Parent<{ N < 2 }>\n{\n    fn kept(&self);\n}\n");
    assert!(text.contains("src/c.rs: Child: fn kept(&self);\n"), "{text}");
}

#[test]
fn a_where_clause_header_is_followed() {
    let source =
        "pub trait W<T>: Sized\nwhere\n    T: Fn(Vec<u8>) -> [u8; 4],\n    [u8; 4]: Send,\n{\n    fn kept(&self);\n}\n";
    let text = scan("src/w.rs", source);
    assert!(text.contains("src/w.rs: W: fn kept(&self);\n"), "{text}");
}

#[test]
fn a_generic_default_with_an_array_is_followed() {
    let text = scan("src/d.rs", "pub trait D<T = [u8; 4]> { fn kept(&self) -> T; }\n");
    assert!(text.contains("src/d.rs: D: fn kept(&self) -> T;\n"), "{text}");
}

#[test]
fn braces_and_angles_inside_a_member_signature_do_not_split_it() {
    let text = scan(
        "src/m.rs",
        "pub trait M {\n    fn a(&self) -> Foo<{ 1 }>;\n    fn b(&self) -> [u8; { 2 }];\n    fn c(&self);\n}\n",
    );
    assert!(text.contains("src/m.rs: M: fn a(&self) -> Foo<{ 1 }>;\n"), "{text}");
    assert!(text.contains("src/m.rs: M: fn b(&self) -> [u8; { 2 }];\n"), "{text}");
    assert!(text.contains("src/m.rs: M: fn c(&self);\n"), "{text}");
}

#[test]
#[should_panic(expected = "cannot place the body of `pub trait Broken`")]
fn an_unbalanced_header_fails_closed() {
    scan("src/b.rs", "pub trait Broken: Parent<A { fn lost(&self); }\npub fn after() {}\n");
}

#[test]
#[should_panic(expected = "extend the scanner")]
fn a_trait_with_no_body_before_the_end_of_the_file_fails_closed() {
    scan("src/b.rs", "pub trait Cut: Parent\n");
}

#[test]
#[should_panic(expected = "macro invocation")]
fn a_macro_invocation_as_a_trait_member_fails_closed() {
    scan("src/b.rs", "pub trait Gen {\n    make_members!();\n}\n");
}

#[test]
#[should_panic(expected = "macro_rules!")]
fn macro_rules_fails_closed() {
    scan("src/b.rs", "macro_rules! m { () => { pub trait Hidden { fn x(); } }; }\n");
}

#[test]
#[should_panic(expected = "does not start its line")]
fn a_pub_item_inside_a_macro_invocation_fails_closed() {
    scan("src/b.rs", "decl! { pub trait Hidden { fn x(); } }\n");
}

#[test]
#[should_panic(expected = "after a trait's closing brace")]
fn a_pub_item_after_a_trait_on_the_same_line_fails_closed() {
    scan("src/b.rs", "pub trait T {\n} pub fn lost() {}\n");
}

#[test]
#[should_panic(expected = "unterminated string literal")]
fn an_unterminated_string_fails_closed() {
    scan("src/b.rs", "const S: &str = \"open;\npub trait T { fn x(); }\n");
}

#[test]
#[should_panic(expected = "code after it would be dropped")]
fn a_cfg_test_that_is_not_a_module_fails_closed() {
    scan("src/b.rs", "#[cfg(test)]\nuse x::y;\npub fn lost() {}\n");
}

#[test]
#[should_panic(expected = "more code after its module")]
fn code_after_the_test_module_fails_closed() {
    scan("src/b.rs", "#[cfg(test)]\nmod tests {}\npub fn lost() {}\n");
}

#[test]
fn an_attribute_on_the_item_line_and_field_names_are_read() {
    let text = scan("src/a.rs", "#[must_use] pub fn f() {}\npub struct S {\n    pub type_id: u8,\n}\n");
    assert!(text.contains("src/a.rs: pub fn f() {}\n"), "{text}");
    assert!(!text.contains("type_id"), "{text}");
}
