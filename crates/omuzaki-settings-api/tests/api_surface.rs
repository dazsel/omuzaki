//! The public-API snapshot: every public declaration in `src/`, compared with `tests/api_surface.txt`.
//!
//! A change to the contract shows up as a diff of that file in the PR, which is what the API review lane reads. After an
//! intended change, regenerate with `UPDATE_API_SURFACE=1 cargo test -p omuzaki-settings-api --test api_surface`.
//!
//! It is a line scanner, not a syntax tree (`syn` is not a workspace dependency), but it records the whole declaration
//! and every member a caller can reach, each under the type that holds it:
//!
//! - items: `pub fn` (the full signature, however many lines), `pub struct`, `pub enum`, `pub trait`, `pub const`
//!   (with its value), `pub static`, `pub type`, `pub mod`, `pub use`, exported `macro_rules!` (the name and each arm's
//!   matcher) and module-level macro invocations that define items (`slug_id!`);
//! - members: public struct fields, every enum variant, every trait method, type and constant, public inherent methods
//!   and constants, and the header of each trait impl;
//! - the attributes that change the contract: `derive`, `non_exhaustive`, `serde`, `must_use`, `repr`, `macro_export`,
//!   `doc(hidden)`, `deprecated` and non-test `cfg`.
//!
//! It skips everything private (`pub(crate)` included), `#[cfg(test)]` items, comments and function bodies. Raw
//! strings and multi-line string literals are not understood; `scans_each_kind_of_declaration` pins what is.

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const KEPT_ATTRS: [&str; 10] = [
    "#[derive",
    "#[non_exhaustive",
    "#[serde",
    "#[must_use",
    "#[repr",
    "#[macro_export",
    "#[doc(hidden",
    "#[deprecated",
    "#[cfg(",
    "#[track_caller",
];

/// A statement that goes on past a `;`-less `{` only if it is one of these: its value or list is the declaration.
const WHOLE_STATEMENT: [&str; 6] = ["pub const", "const", "pub static", "static", "pub use", "use"];

/// A guard against a construct the scanner does not understand swallowing the rest of a file.
const STATEMENT_MAX: usize = 4000;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A module body, the file itself at the root.
    Module,
    Struct,
    Enum,
    Trait,
    /// An inherent `impl`: only its `pub` members are reachable.
    Impl,
    /// An exported `macro_rules!` body, whose arms are recorded.
    Macro,
    /// A block that holds nothing public: a function body, a private item, a test module.
    Skip,
}

struct Ctx {
    kind: Kind,
    /// The type or trait the members are recorded under.
    label: String,
    /// Net open braces, for `Skip` and `Macro`.
    depth: i32,
}

impl Ctx {
    fn new(kind: Kind, label: impl Into<String>, depth: i32) -> Self {
        Self { kind, label: label.into(), depth }
    }
}

/// The line without its comment (strings kept) and with strings and char literals blanked, so counting brackets sees
/// only code.
fn scan_line(line: &str) -> (String, String) {
    let (mut kept, mut counted) = (String::new(), String::new());
    let mut chars = line.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if in_string {
            kept.push(c);
            match c {
                '\\' => kept.extend(chars.next()),
                '"' => {
                    in_string = false;
                    counted.push('"');
                }
                _ => {}
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                kept.push(c);
                counted.push(c);
            }
            '/' if chars.peek() == Some(&'/') => break,
            '\'' => {
                // `'x'` and `'\n'` are char literals; `'a` is a lifetime and has no closing quote.
                let mut ahead = chars.clone();
                let literal = matches!((ahead.next(), ahead.next()), (Some('\\'), _) | (Some(_), Some('\'')));
                kept.push(c);
                counted.push(c);
                if literal {
                    for next in chars.by_ref() {
                        kept.push(next);
                        if next == '\'' && !kept.ends_with("\\'") {
                            break;
                        }
                    }
                    counted.push('\'');
                }
            }
            _ => {
                kept.push(c);
                counted.push(c);
            }
        }
    }
    (kept, counted)
}

fn net(text: &str, open: char, close: char) -> i32 {
    let opened = i32::try_from(text.matches(open).count()).unwrap();
    opened - i32::try_from(text.matches(close).count()).unwrap()
}

fn braces(counted: &str) -> i32 {
    net(counted, '{', '}')
}

fn brackets_balanced(counted: &str) -> bool {
    net(counted, '(', ')') == 0 && net(counted, '[', ']') == 0
}

/// One line per statement: spaces collapsed, and the spacing a wrapped signature leaves behind removed.
fn tidy(statement: &str) -> String {
    let mut text = statement.split_whitespace().collect::<Vec<_>>().join(" ");
    for (from, to) in [("( ", "("), (" )", ")"), ("[ ", "["), (" ]", "]"), (",)", ")"), (",]", "]"), (", }", " }")] {
        text = text.replace(from, to);
    }
    if text.starts_with("pub use ") || text.starts_with("use ") {
        // A use list reads the same wrapped or on one line.
        text = text.replace("{ ", "{").replace(" }", "}");
    }
    text.trim_end_matches(['{', ';', ',', ' ']).to_owned()
}

fn is_public(text: &str) -> bool {
    text.starts_with("pub ") && !text.starts_with("pub(")
}

/// Whether the statement being assembled is finished, given the kind of block it sits in.
fn complete(statement: &str, counted: &str, kind: Kind) -> bool {
    if statement.starts_with("#[") {
        return brackets_balanced(counted);
    }
    if !brackets_balanced(counted) {
        return statement.len() > STATEMENT_MAX;
    }
    let depth = braces(counted);
    match kind {
        Kind::Struct | Kind::Enum => depth == 0 && (statement.ends_with(',') || statement.ends_with('}')),
        _ => {
            let whole =
                WHOLE_STATEMENT.iter().any(|k| statement.starts_with(&format!("{k} "))) && !statement.contains(" fn ");
            if whole {
                depth == 0 && statement.ends_with(';')
            } else {
                statement.ends_with('{') || (depth == 0 && (statement.ends_with(';') || statement.ends_with('}')))
            }
        }
    }
}

/// Reads a source file line by line and records what a caller of the crate can reach.
struct Scanner {
    out: Vec<String>,
    stack: Vec<Ctx>,
    /// The attributes seen since the last item, applied to the next one.
    attrs: Vec<String>,
    /// The statement being assembled across lines, and the same with strings blanked.
    statement: String,
    counted: String,
}

/// What a finished statement opens: nothing, or a block of this kind.
type Opened = Option<Ctx>;

impl Scanner {
    fn new() -> Self {
        Self {
            out: Vec::new(),
            stack: vec![Ctx::new(Kind::Module, "", 0)],
            attrs: Vec::new(),
            statement: String::new(),
            counted: String::new(),
        }
    }

    fn top(&mut self) -> &mut Ctx {
        self.stack.last_mut().unwrap()
    }

    /// Records `text` under the current block's label, with the pending attributes in front.
    fn record(&mut self, text: &str) {
        let attrs = self.attrs.iter().map(String::as_str).chain([text]).collect::<Vec<_>>().join(" ");
        let label = &self.stack.last().unwrap().label;
        self.out.push(if label.is_empty() { attrs } else { format!("{label}: {attrs}") });
    }

    fn line(&mut self, raw: &str) {
        let (kept, code) = scan_line(raw);
        let (kept, code) = (kept.trim(), code.trim());
        if kept.is_empty() {
            return;
        }
        let top = self.top();
        match top.kind {
            Kind::Skip | Kind::Macro => {
                if top.kind == Kind::Macro && top.depth == 1 && kept.starts_with(['(', '[']) && !top.label.is_empty() {
                    let arm = tidy(kept.split("=>").next().unwrap_or(kept));
                    let line = format!("{}: {arm}", top.label);
                    self.out.push(line);
                }
                let top = self.top();
                top.depth += braces(code);
                if top.depth <= 0 {
                    self.stack.pop();
                }
                return;
            }
            _ => {}
        }
        if self.statement.is_empty() && kept.starts_with("#![") {
            return; // crate-level attributes configure the build, they are not API
        }
        if self.statement.is_empty() && kept.starts_with('}') {
            if self.stack.len() > 1 {
                self.stack.pop();
            }
            self.attrs.clear();
            return;
        }
        self.statement.push(' ');
        self.statement.push_str(kept);
        self.counted.push(' ');
        self.counted.push_str(code);
        let (statement, counted) = (self.statement.trim().to_owned(), self.counted.trim().to_owned());
        if !complete(&statement, &counted, self.top().kind) {
            return;
        }
        self.statement.clear();
        self.counted.clear();
        self.finished(&statement, &counted);
    }

    /// A whole statement has been read.
    fn finished(&mut self, statement: &str, counted: &str) {
        if statement.starts_with("#[") {
            if KEPT_ATTRS.iter().any(|a| statement.starts_with(a)) {
                self.attrs.push(tidy(statement));
            }
            return;
        }
        let depth = braces(counted);
        let opens = counted.ends_with('{') && depth > 0;
        let text = tidy(statement);
        let skip = || Some(Ctx::new(Kind::Skip, "", depth));
        let tested = self.attrs.iter().any(|a| a == "#[cfg(test)]");
        let opened: Opened = if tested {
            opens.then(skip).flatten()
        } else {
            match self.top().kind {
                Kind::Module => self.in_module(&text, opens, depth),
                Kind::Struct => {
                    if is_public(&text) {
                        self.record(&text);
                    }
                    None
                }
                Kind::Enum => {
                    self.record(&text);
                    None
                }
                Kind::Trait | Kind::Impl => {
                    if self.top().kind == Kind::Trait || is_public(&text) {
                        self.record(&text);
                    }
                    opens.then(skip).flatten()
                }
                Kind::Macro | Kind::Skip => None,
            }
        };
        self.attrs.clear();
        self.stack.extend(opened);
    }

    /// A statement at module level: an item, an `impl`, a macro definition or a macro call that defines items.
    fn in_module(&mut self, text: &str, opens: bool, depth: i32) -> Opened {
        let block = |kind: Kind, label: String| opens.then(|| Ctx::new(kind, label, 0));
        let named = |keyword: &str| -> String {
            let name: String = text
                .split_once(&format!("{keyword} "))
                .map(|(_, rest)| rest.chars().take_while(|c| c.is_alphanumeric() || *c == '_').collect())
                .unwrap_or_default();
            format!("{keyword} {name}")
        };
        if text.starts_with("impl ") || text.starts_with("impl<") {
            if text.contains(" for ") {
                self.record(text);
                return opens.then(|| Ctx::new(Kind::Skip, "", depth));
            }
            return block(Kind::Impl, text.to_owned());
        }
        if text.starts_with("macro_rules!") {
            let exported = self.attrs.iter().any(|a| a.starts_with("#[macro_export"));
            if exported {
                self.record(text);
            }
            return opens.then(|| Ctx::new(Kind::Macro, if exported { text.to_owned() } else { String::new() }, depth));
        }
        if is_public(text) {
            self.record(text);
            return if text.starts_with("pub struct ") {
                block(Kind::Struct, named("struct"))
            } else if text.starts_with("pub enum ") {
                block(Kind::Enum, named("enum"))
            } else if text.starts_with("pub trait ") || text.starts_with("pub unsafe trait ") {
                block(Kind::Trait, named("trait"))
            } else if text.starts_with("pub mod ") {
                block(Kind::Module, named("mod"))
            } else {
                opens.then(|| Ctx::new(Kind::Skip, "", depth))
            };
        }
        let calls_macro = text.split('!').next().is_some_and(|name| {
            text.contains('!') && !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase() || c == '_')
        });
        if calls_macro {
            // `slug_id!(Name);` and the like define public items.
            self.record(text);
            return None;
        }
        opens.then(|| Ctx::new(Kind::Skip, "", depth))
    }
}

/// Every public declaration and member in `source`, one per entry.
fn scan(source: &str) -> Vec<String> {
    let mut scanner = Scanner::new();
    for raw in source.lines() {
        scanner.line(raw);
    }
    scanner.out
}

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
        // Joined with `/` on every platform, so the snapshot is the same on Windows.
        let rel: Vec<_> =
            file.strip_prefix(root).unwrap().components().map(|c| c.as_os_str().to_string_lossy()).collect();
        let rel = rel.join("/");
        let source = std::fs::read_to_string(&file).unwrap();
        for line in scan(&source) {
            writeln!(text, "{rel}: {line}").unwrap();
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
    let before = std::fs::read_to_string(&snapshot).unwrap_or_default();
    let diff = |from: &str, to: &str, mark: char| -> String {
        let kept: std::collections::HashSet<_> = to.lines().collect();
        from.lines().filter(|l| !kept.contains(l)).map(|l| format!("\n{mark} {l}")).collect::<Vec<_>>().concat()
    };
    assert!(
        before == now,
        "the public API changed: review the diff, then regenerate with UPDATE_API_SURFACE=1{}{}",
        diff(&before, &now, '-'),
        diff(&now, &before, '+')
    );
}

const SAMPLE: &str = r#"
#![deny(missing_docs)]

use std::fmt;

/// A doc comment with `pub fn not_this()` in it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Meta {
    /// The key.
    pub key: &'static str, // trailing comment
    hidden: u8,
    pub(crate) crate_only: u8,
}

pub struct Wrapper(pub String);

#[derive(Debug)]
pub enum Kind {
    A,
    B(u8),
    C {
        x: u8,
        y: Option<u8>,
    },
}

pub trait Service: Send {
    type Out;
    const LIMIT: usize;
    fn get(&self, key: &str) -> Option<u8>;
    fn provided(&self) { let _ = "}"; }
    fn long(
        &self,
        a: u8,
    ) -> u8 {
        a
    }
}

impl Meta {
    pub const MAX: usize = 48;
    #[must_use]
    pub fn new(
        key: &'static str,
    ) -> Self {
        Self { key, hidden: 0, crate_only: '{' as u8 }
    }
    fn private(&self) {}
    pub(crate) fn crate_only(&self) {}
}

impl fmt::Display for Meta {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.key)
    }
}

pub const NAME_MAX: usize = 48;
pub const LIST: &[&str] = &[
    "a",
    "b",
];
pub use inner::{
    One,
    Two,
};
pub mod inner;
pub fn longer<'a, T>(a: &'a T) -> &'a T
where
    T: Clone,
{
    a
}
pub(crate) fn crate_only() {}
fn private() {
    if true {
    }
}
slug_id!(
    /// Docs.
    ProfileId
);

#[macro_export]
macro_rules! check {
    ($link:expr) => {
        fn inner() {}
    };
    () => {};
}
macro_rules! private_macro {
    ($x:expr) => {};
}

#[cfg(test)]
mod tests {
    pub fn not_api() {}
}
"#;

#[test]
fn scans_each_kind_of_declaration() {
    let expected = [
        "#[derive(Clone, Debug)] #[non_exhaustive] pub struct Meta",
        "struct Meta: pub key: &'static str",
        "pub struct Wrapper(pub String)",
        "#[derive(Debug)] pub enum Kind",
        "enum Kind: A",
        "enum Kind: B(u8)",
        "enum Kind: C { x: u8, y: Option<u8> }",
        "pub trait Service: Send",
        "trait Service: type Out",
        "trait Service: const LIMIT: usize",
        "trait Service: fn get(&self, key: &str) -> Option<u8>",
        "trait Service: fn provided(&self) { let _ = \"}\"; }",
        "trait Service: fn long(&self, a: u8) -> u8",
        "impl Meta: pub const MAX: usize = 48",
        "impl Meta: #[must_use] pub fn new(key: &'static str) -> Self",
        "impl fmt::Display for Meta",
        "pub const NAME_MAX: usize = 48",
        "pub const LIST: &[&str] = &[\"a\", \"b\"]",
        "pub use inner::{One, Two}",
        "pub mod inner",
        "pub fn longer<'a, T>(a: &'a T) -> &'a T where T: Clone",
        "slug_id!(ProfileId)",
        "#[macro_export] macro_rules! check",
        "macro_rules! check: ($link:expr)",
        "macro_rules! check: ()",
    ];
    let got = scan(SAMPLE);
    for line in &expected {
        assert!(got.iter().any(|g| g == line), "missing `{line}` in\n{}", got.join("\n"));
    }
    assert_eq!(got.len(), expected.len(), "unexpected extra entries in\n{}", got.join("\n"));
}

#[test]
fn a_removed_variant_field_or_trait_method_changes_the_surface() {
    let before = scan(SAMPLE);
    for (from, to) in [
        ("    B(u8),\n", ""),
        ("    pub key: &'static str, // trailing comment\n", ""),
        ("    fn get(&self, key: &str) -> Option<u8>;\n", ""),
        ("    pub const MAX: usize = 48;\n", "    pub const MAX: usize = 49;\n"),
        (
            "        key: &'static str,\n    ) -> Self {",
            "        key: &'static str,\n        extra: u8,\n    ) -> Self {",
        ),
        ("#[derive(Debug)]\npub enum Kind", "pub enum Kind"),
    ] {
        assert!(SAMPLE.contains(from), "the sample no longer has `{from}`");
        assert_ne!(scan(&SAMPLE.replace(from, to)), before, "`{from}` changed to `{to}` is invisible");
    }
    assert_eq!(scan(&SAMPLE.replace("    hidden: u8,\n", "    hidden: u16,\n")), before, "a private field is not API");
}

/// The signatures of the methods of `trait_name` declared in `file`, as the scanner records them.
fn trait_methods(file: &str, trait_name: &str) -> Vec<(String, String)> {
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(file)).unwrap();
    let label = format!("trait {trait_name}: fn ");
    scan(&source)
        .into_iter()
        .filter_map(|line| line.strip_prefix(&label).map(str::to_owned))
        .map(|sig| (sig.split(['(', '<']).next().unwrap().to_owned(), sig))
        .collect()
}

#[test]
fn every_mutating_settings_method_takes_a_mode_and_an_actor() {
    // The methods that only read. A new method is mutating until it is added here on purpose.
    const READS: [&str; 8] = ["snapshot", "resolve", "list", "profiles", "changes", "findings", "subscribe", "session"];
    let methods = trait_methods("src/service.rs", "Settings");
    assert!(methods.len() >= 15, "the scan found only {} methods: {methods:?}", methods.len());
    for (name, sig) in &methods {
        let reads = READS.contains(&name.as_str());
        let mutating_shape = sig.contains("mode: Mode, by: &Actor");
        assert!(reads != mutating_shape, "`{name}` must either be a read, or take `mode: Mode, by: &Actor`: {sig}");
    }
    for read in READS {
        assert!(methods.iter().any(|(name, _)| name == read), "`{read}` is listed as a read but is not a method");
    }
}

#[test]
fn every_session_store_method_is_a_read_a_durable_write_with_a_result_or_a_named_debounced_write() {
    const DEBOUNCED: [&str; 3] = ["forget", "set_window", "mark_version_seen"];
    let methods = trait_methods("src/session.rs", "SessionStore");
    assert!(methods.len() >= 6, "the scan found only {} methods: {methods:?}", methods.len());
    for (name, sig) in &methods {
        let returns_result = sig.contains("-> Result<");
        let debounced = DEBOUNCED.contains(&name.as_str());
        // `state` is the one read; a debounced write cannot fail the caller; everything else promises durability.
        assert!(
            name == "state" || (debounced != returns_result),
            "`{name}` must return a Result (a durable write) or be one of the debounced writes {DEBOUNCED:?}: {sig}"
        );
    }
}
