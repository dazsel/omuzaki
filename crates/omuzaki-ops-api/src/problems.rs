//! Refusing only the problems an edit introduces (#659), in one copy for every op that edits a validated value (the
//! face style ops, the layout ops, and the next one). The op lists what is wrong with the value before and after the
//! edit, each problem at its own path; a problem the value already had is not blamed on the edit, so a value damaged
//! elsewhere still takes unrelated edits, and the refusal names the first new problem and lists all of them.

use std::fmt;

use crate::OpError;

/// One thing wrong with a value an op edits: where it is, what is wrong, and a fix when the problem has one of its own.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldProblem {
    /// The path, for example `face/eyes/iris/width`.
    pub path: String,
    /// What is wrong, in a sentence.
    pub message: String,
    /// The fix, when the problem has one of its own (a nearby key).
    pub hint: Option<String>,
}

impl FieldProblem {
    /// A problem at `path` with no fix of its own.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into(), hint: None }
    }

    /// The problem an op's refusal names: its field, message and hint.
    #[must_use]
    pub fn from_error(error: OpError) -> Self {
        Self { path: error.field.unwrap_or_default(), message: error.message, hint: error.hint }
    }

    /// Two problems are the same when they name the same path with the same message; the hint doesn't count.
    #[must_use]
    pub fn is_same(&self, other: &FieldProblem) -> bool {
        self.path == other.path && self.message == other.message
    }
}

impl fmt::Display for FieldProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// The problems in `after` that are not in `before`, in `after`'s order.
fn introduced(before: &[FieldProblem], after: Vec<FieldProblem>) -> Vec<FieldProblem> {
    after.into_iter().filter(|p| !before.iter().any(|b| b.is_same(p))).collect()
}

/// Refuses an edit whose value has a problem (`after`) the value before it did not (`before`).
///
/// # Errors
/// `Invalid` at the first new problem's path, with the message `<n> field(s) is/are invalid: <path>: <message>; ...`
/// listing every new problem, and the first one's own hint or else `hint`.
pub fn refuse_new_problems(before: &[FieldProblem], after: Vec<FieldProblem>, hint: &str) -> Result<(), OpError> {
    let found = introduced(before, after);
    let Some(first) = found.first() else {
        return Ok(());
    };
    let count = found.len();
    let noun = if count == 1 { "field is" } else { "fields are" };
    let list = found.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Err(OpError::invalid(first.path.clone(), format!("{count} {noun} invalid: {list}"))
        .hint(first.hint.clone().unwrap_or_else(|| hint.to_owned())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_new_problems_are_refused_and_listed() {
        let before = [FieldProblem::new("a/x", "too big")];
        let after = vec![
            FieldProblem::new("a/x", "too big"),
            FieldProblem::new("a/y", "too small"),
            FieldProblem::new("a/x", "not a number"),
        ];
        let error = refuse_new_problems(&before, after, "fix it").unwrap_err();
        assert_eq!(error.field.as_deref(), Some("a/y"));
        assert_eq!(error.message, "2 fields are invalid: a/y: too small; a/x: not a number");
        assert_eq!(error.hint.as_deref(), Some("fix it"));
    }

    #[test]
    fn a_problem_the_value_already_had_is_not_blamed_and_its_own_hint_wins() {
        let before = [FieldProblem::new("a/x", "too big")];
        assert!(refuse_new_problems(&before, before.to_vec(), "fix it").is_ok());
        let keyed = FieldProblem { hint: Some("did you mean `y`?".to_owned()), ..FieldProblem::new("a/z", "unknown") };
        let error = refuse_new_problems(&[], vec![keyed], "fix it").unwrap_err();
        assert_eq!(error.message, "1 field is invalid: a/z: unknown");
        assert_eq!(error.hint.as_deref(), Some("did you mean `y`?"));
    }
}
