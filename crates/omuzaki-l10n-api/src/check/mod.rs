//! The checks of design 10.1 (`docs/design/l10n-ops.md`): the rules a package's text must meet before it ships.
//!
//! Each check returns its [`Failure`]s, so a test can assert on them, and a planted fault shows that its rule fails.
//! The module compiles always, with no cargo feature: `cargo xtask check` runs the workspace tests without per-crate
//! features, so tests behind a feature would not run in the gate.

mod registered;

pub use registered::registered;

/// One problem a check found: which check, which id, and what to fix.
#[derive(Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Failure {
    /// The check that found it (`registered`).
    pub check: &'static str,
    /// The id the problem is about: an op or kind id, or an argument id.
    pub id: String,
    /// What is wrong and how to fix it, in one line.
    pub problem: String,
}

impl Failure {
    /// A failure of `check` about `id`.
    pub fn new(check: &'static str, id: impl Into<String>, problem: impl Into<String>) -> Self {
        Self { check, id: id.into(), problem: problem.into() }
    }
}

/// The failures as one message for a test assertion: one line each (`<check> <id>: <problem>`), sorted.
///
/// The id and the problem may hold text from a package, so they are escaped to keep each failure on one physical
/// line: `\` becomes `\\`, a line feed becomes `\n`, a carriage return `\r`, a tab `\t`, and any other control
/// character or Unicode line separator `\u{..}`.
pub fn report(failures: &[Failure]) -> String {
    let mut lines: Vec<String> = failures
        .iter()
        .map(|failure| format!("{} {}: {}", escape(failure.check), escape(&failure.id), escape(&failure.problem)))
        .collect();
    lines.sort();
    lines.join("\n")
}

/// `text` with backslashes, control characters and line separators written as escapes, so it stays on one line.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() || matches!(c, '\u{2028}' | '\u{2029}') => {
                out.push_str(&c.escape_unicode().to_string());
            }
            c => out.push(c),
        }
    }
    out
}
