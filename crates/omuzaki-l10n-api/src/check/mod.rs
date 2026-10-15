//! The checks of design 10.1 (`docs/design/l10n-ops.md`): the rules a package's text must meet before it ships.
//!
//! Each check returns its [`Failure`]s, so a test can assert on them, and a planted fault shows that its rule fails.
//! The module compiles always, with no cargo feature: `cargo xtask check` runs the workspace tests without per-crate
//! features, so tests behind a feature would not run in the gate.

mod messages;
mod registered;

use std::collections::BTreeSet;

use fluent_syntax::ast::{CallArguments, Expression, InlineExpression, Pattern, PatternElement};
pub use messages::{messages, prefixes};
pub use registered::registered;

/// Every variable a Fluent pattern references (`$name`), in placeables and selectors, at any depth. The translations
/// check uses it too, so both checks agree on what a placeable is.
pub(crate) fn variables<'s>(pattern: &Pattern<&'s str>) -> BTreeSet<&'s str> {
    let mut names = BTreeSet::new();
    collect_pattern(pattern, &mut names);
    names
}

fn collect_pattern<'s>(pattern: &Pattern<&'s str>, names: &mut BTreeSet<&'s str>) {
    for element in &pattern.elements {
        if let PatternElement::Placeable { expression } = element {
            collect_expression(expression, names);
        }
    }
}

fn collect_expression<'s>(expression: &Expression<&'s str>, names: &mut BTreeSet<&'s str>) {
    match expression {
        Expression::Inline(inline) => collect_inline(inline, names),
        Expression::Select { selector, variants } => {
            collect_inline(selector, names);
            for variant in variants {
                collect_pattern(&variant.value, names);
            }
        }
    }
}

fn collect_inline<'s>(inline: &InlineExpression<&'s str>, names: &mut BTreeSet<&'s str>) {
    match inline {
        InlineExpression::VariableReference { id } => {
            names.insert(id.name);
        }
        InlineExpression::Placeable { expression } => collect_expression(expression, names),
        InlineExpression::FunctionReference { arguments, .. }
        | InlineExpression::TermReference { arguments: Some(arguments), .. } => collect_call(arguments, names),
        InlineExpression::StringLiteral { .. }
        | InlineExpression::NumberLiteral { .. }
        | InlineExpression::MessageReference { .. }
        | InlineExpression::TermReference { arguments: None, .. } => {}
    }
}

fn collect_call<'s>(arguments: &CallArguments<&'s str>, names: &mut BTreeSet<&'s str>) {
    for value in &arguments.positional {
        collect_inline(value, names);
    }
    for named in &arguments.named {
        collect_inline(&named.value, names);
    }
}

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
pub fn report(failures: &[Failure]) -> String {
    let mut lines: Vec<String> =
        failures.iter().map(|failure| format!("{} {}: {}", failure.check, failure.id, failure.problem)).collect();
    lines.sort();
    lines.join("\n")
}
