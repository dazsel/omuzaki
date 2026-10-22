//! The checks of design 10.1 (`docs/design/l10n-ops.md`): the rules a package's text must meet before it ships.
//!
//! Each check returns its [`Failure`]s, so a test can assert on them, and a planted fault shows that its rule fails.
//! The module compiles always, with no cargo feature: `cargo xtask check` runs the workspace tests without per-crate
//! features, so tests behind a feature would not run in the gate.

mod english;
mod messages;
mod pseudo;
mod registered;
mod translations;

use std::collections::BTreeSet;

use fluent_syntax::ast::{CallArguments, Expression, InlineExpression, Pattern, PatternElement};

use crate::Resource;

pub use english::english_current;
pub use messages::{messages, prefixes};
pub use pseudo::{pseudo, samples};
pub use registered::registered;
pub use translations::translations;

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

/// Every variable a Fluent pattern references (`$name`), in placeables and selectors, at any depth.
pub(crate) fn variables<'s>(pattern: &Pattern<&'s str>) -> BTreeSet<&'s str> {
    let mut found = BTreeSet::new();
    pattern_variables(pattern, &mut found);
    found
}

fn pattern_variables<'s>(pattern: &Pattern<&'s str>, found: &mut BTreeSet<&'s str>) {
    for element in &pattern.elements {
        if let PatternElement::Placeable { expression } = element {
            expression_variables(expression, found);
        }
    }
}

fn expression_variables<'s>(expression: &Expression<&'s str>, found: &mut BTreeSet<&'s str>) {
    match expression {
        Expression::Inline(inline) => inline_variables(inline, found),
        Expression::Select { selector, variants } => {
            inline_variables(selector, found);
            for variant in variants {
                pattern_variables(&variant.value, found);
            }
        }
    }
}

fn inline_variables<'s>(inline: &InlineExpression<&'s str>, found: &mut BTreeSet<&'s str>) {
    match inline {
        InlineExpression::VariableReference { id } => {
            found.insert(id.name);
        }
        InlineExpression::FunctionReference { arguments, .. }
        | InlineExpression::TermReference { arguments: Some(arguments), .. } => call_variables(arguments, found),
        InlineExpression::Placeable { expression } => expression_variables(expression, found),
        InlineExpression::StringLiteral { .. }
        | InlineExpression::NumberLiteral { .. }
        | InlineExpression::MessageReference { .. }
        | InlineExpression::TermReference { arguments: None, .. } => {}
    }
}

fn call_variables<'s>(arguments: &CallArguments<&'s str>, found: &mut BTreeSet<&'s str>) {
    for positional in &arguments.positional {
        inline_variables(positional, found);
    }
    for named in &arguments.named {
        inline_variables(&named.value, found);
    }
}

/// The registered [`Resource`]s of `package` (from `inventory`), for `check_crate!`.
pub fn resources(package: &str) -> Vec<Resource> {
    inventory::iter::<Resource>.into_iter().filter(|resource| resource.package == package).copied().collect()
}

/// The l10n tests of the crate it is invoked in (design 10.1). It goes in `crates/<crate>/tests/l10n.rs`; the crate
/// needs only `omuzaki-l10n-api` as a dev-dependency. Each test calls `<krate>::link()` first, then one check over
/// `PackageText::registered(env!("CARGO_PKG_NAME"))`, and asserts no failures with [`report`].
///
/// `OMUZAKI_BLESS=1` makes the English-file test rewrite `locales/en/<crate>.ftl` and create the missing stubs
/// instead of failing.
///
/// ```no_run
/// mod my_crate {
///     pub fn link() {}
/// }
/// omuzaki_l10n_api::check_crate!(my_crate);
/// ```
#[macro_export]
macro_rules! check_crate {
    ($krate:ident) => {
        /// The registered ops and part kinds have well-formed text.
        #[test]
        fn l10n_registered_text() {
            $krate::link();
            let text = $crate::extract::PackageText::registered(env!("CARGO_PKG_NAME"));
            let failures = $crate::check::registered(&text);
            assert!(failures.is_empty(), "\n{}", $crate::check::report(&failures));
        }

        /// The declared messages have notes, prefixes, parsing English and matching placeables.
        #[test]
        fn l10n_messages() {
            $krate::link();
            let text = $crate::extract::PackageText::registered(env!("CARGO_PKG_NAME"));
            let failures = $crate::check::messages(&text);
            assert!(failures.is_empty(), "\n{}", $crate::check::report(&failures));
        }

        /// The committed English file is the generated one.
        #[test]
        fn l10n_english_is_current() {
            $krate::link();
            let text = $crate::extract::PackageText::registered(env!("CARGO_PKG_NAME"));
            let bless = ::std::env::var("OMUZAKI_BLESS").is_ok_and(|value| value == "1");
            let failures =
                $crate::check::english_current(&text, ::std::path::Path::new(env!("CARGO_MANIFEST_DIR")), bless);
            assert!(failures.is_empty(), "\n{}", $crate::check::report(&failures));
        }

        /// Every translation file parses and matches the English.
        #[test]
        fn l10n_translations() {
            $krate::link();
            let text = $crate::extract::PackageText::registered(env!("CARGO_PKG_NAME"));
            let resources = $crate::check::resources(env!("CARGO_PKG_NAME"));
            let failures = $crate::check::translations(&text, &resources);
            assert!(failures.is_empty(), "\n{}", $crate::check::report(&failures));
        }

        /// Every message renders in the pseudo-locale, with no plain text and some growth.
        #[test]
        fn l10n_pseudo() {
            $krate::link();
            let text = $crate::extract::PackageText::registered(env!("CARGO_PKG_NAME"));
            let pseudo = $crate::Catalog::new($crate::Locale::PSEUDO);
            let english = $crate::Catalog::new($crate::Locale::EN);
            let failures = $crate::check::pseudo(&text, &pseudo, &english);
            assert!(failures.is_empty(), "\n{}", $crate::check::report(&failures));
        }
    };
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
