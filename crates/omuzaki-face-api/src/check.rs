//! Parameter validation shared by every editable struct (spec §6: errors are structured and actionable).
//!
//! Ops edit parameters by path (`face.set_style`, `part.set_param`): they write the new value into a copy of the typed
//! struct, then call [`Validate::validate`]. The errors name the *path of the field* (`eyes/iris/width`), so the op can
//! hand an agent exactly what to fix. Paths use `/`, like document paths.

use std::fmt;

/// One invalid field.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParamError {
    /// Where, as a `/`-separated path from the struct that was validated (for example `eyes/iris/width`).
    pub path: String,
    /// What is wrong and what would be valid.
    pub message: String,
}

impl fmt::Display for ParamError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Every invalid field found in one validation pass (reporting all of them saves agents a round trip per mistake).
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub struct ParamErrors(pub Vec<ParamError>);

impl fmt::Display for ParamErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, e) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str("; ")?;
            }
            write!(f, "{e}")?;
        }
        Ok(())
    }
}

/// The interval one numeric field accepts, as data an op can show (JSON Schema's `minimum` and `maximum`).
#[derive(Clone, Debug, PartialEq)]
pub struct FieldRange {
    /// Where, as a `/`-separated path from the struct that was asked (for example `iris/width`).
    pub path: String,
    /// The smallest accepted value.
    pub lo: f64,
    /// The largest accepted value.
    pub hi: f64,
    /// A rule between fields that also applies to this one (`mouth_y` must be below `eye_y`), when there is one.
    pub note: Option<String>,
}

/// Collects errors while walking a struct tree.
#[derive(Debug, Default)]
pub struct Check {
    path: Vec<String>,
    errors: Vec<ParamError>,
    /// `Some` while ranges are being read: the checks then test nothing and name their intervals here, so they are the
    /// numbers that validation uses.
    recorded: Option<Vec<FieldRange>>,
}

impl Check {
    /// A fresh collector at the root.
    pub fn new() -> Self {
        Self::default()
    }

    fn at(&self, name: &str) -> String {
        let leaf = (!name.is_empty()).then_some(name);
        self.path.iter().map(String::as_str).chain(leaf).collect::<Vec<_>>().join("/")
    }

    /// A collector that records ranges instead of testing values (see [`Validate::ranges`]).
    fn recording() -> Self {
        Self { recorded: Some(Vec::new()), ..Self::default() }
    }

    /// Records a failure for field `name` (an empty name means the current section itself).
    pub fn fail(&mut self, name: &str, message: impl Into<String>) {
        let path = self.at(name);
        self.errors.push(ParamError { path, message: message.into() });
    }

    /// A rule between the fields `names`: `ok` must hold. A failure is reported at the first name. When ranges are
    /// being read, `message` becomes the note of the range of every field in `names`, so an agent sees the rule next
    /// to each interval it limits.
    pub fn rule(&mut self, names: &[&str], ok: bool, message: &str) {
        let paths: Vec<String> = names.iter().map(|name| self.at(name)).collect();
        match &mut self.recorded {
            Some(ranges) => {
                for range in ranges.iter_mut().filter(|r| paths.contains(&r.path)) {
                    range.note = Some(message.to_owned());
                }
            }
            None if !ok => {
                let path = paths.into_iter().next().unwrap_or_default();
                self.errors.push(ParamError { path, message: message.to_owned() });
            }
            None => {}
        }
    }

    /// Validates `item` as the section `name`; its errors get the section as a path prefix.
    pub fn nested(&mut self, name: &str, item: &impl Validate) {
        self.path.push(name.to_owned());
        item.check(self);
        self.path.pop();
    }

    /// `value` must be finite and within `lo..=hi`.
    pub fn range(&mut self, name: &str, value: f64, lo: f64, hi: f64) {
        let path = self.at(name);
        if let Some(ranges) = &mut self.recorded {
            ranges.push(FieldRange { path, lo, hi, note: None });
            return;
        }
        if !(value.is_finite() && (lo..=hi).contains(&value)) {
            self.fail(name, format!("{value} is out of range: use a number from {lo} to {hi}"));
        }
    }

    /// `value` must be finite and greater than zero.
    pub fn positive(&mut self, name: &str, value: f64) {
        if !(value.is_finite() && value > 0.0) {
            self.fail(name, format!("{value} must be greater than 0"));
        }
    }

    /// Ends the pass: `Ok` if nothing failed.
    ///
    /// # Errors
    /// All recorded failures, in the order they were found.
    pub fn finish(self) -> Result<(), ParamErrors> {
        if self.errors.is_empty() { Ok(()) } else { Err(ParamErrors(self.errors)) }
    }
}

/// A parameter struct that can say whether its values are usable.
pub trait Validate {
    /// Records every problem in `c`. Implementations only describe their own fields and call [`Check::nested`] for
    /// sub-structs, so path prefixes are right everywhere.
    fn check(&self, c: &mut Check);

    /// Every interval this struct's [`check`](Validate::check) names, in the order it names them, with the paths
    /// `validate` reports. They are read from the checks themselves, so they cannot drift from what is enforced. A
    /// rule between fields ([`Check::rule`]) appears as the `note` of each field it names.
    fn ranges(&self) -> Vec<FieldRange> {
        let mut c = Check::recording();
        self.check(&mut c);
        c.recorded.unwrap_or_default()
    }

    /// Validates on its own.
    ///
    /// # Errors
    /// Every invalid field, with its path.
    fn validate(&self) -> Result<(), ParamErrors> {
        let mut c = Check::new();
        self.check(&mut c);
        c.finish()
    }
}

/// One problem found in an edited value, for the "refuse only the problems an edit introduces" check that the face and
/// layout ops share: where it is (the full path an agent edits, such as `face/eyes/iris/width`), what is wrong, and a fix
/// when the problem names one of its own (a nearby key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    /// The path, for example `face/layout/eye_y`.
    pub path: String,
    /// What is wrong, in a sentence.
    pub message: String,
    /// The fix, when the problem has one of its own.
    pub hint: Option<String>,
}

impl Problem {
    /// A problem at `path` with no fix of its own.
    pub fn new(path: impl Into<String>, message: impl Into<String>) -> Self {
        Self { path: path.into(), message: message.into(), hint: None }
    }

    /// Two problems are the same when they name the same path with the same message: a fix of its own doesn't make a
    /// problem new.
    fn is_same(&self, other: &Problem) -> bool {
        self.path == other.path && self.message == other.message
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// What an op says when it refuses an edit: the field to name, the message listing every problem the edit introduces,
/// and the first one's own fix, when it has one (the op supplies its general hint otherwise).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Refusal {
    /// The path of the first problem the edit introduces.
    pub field: String,
    /// `"<n> fields are invalid: <path>: <message>; …"`.
    pub message: String,
    /// The first problem's own fix.
    pub hint: Option<String>,
}

/// The refusal for an edit whose value has the problems `after`, when the value before it had `before`; `None` when the
/// edit introduces none. Problems the value already had are not blamed on the edit, so a value damaged elsewhere still
/// takes an unrelated change, and a damaged field the edit leaves damaged is refused only if it is wrong in a new way.
pub fn refuse_introduced(before: &[Problem], after: Vec<Problem>) -> Option<Refusal> {
    let introduced: Vec<Problem> = after.into_iter().filter(|p| !before.iter().any(|b| b.is_same(p))).collect();
    let first = introduced.first()?;
    let count = introduced.len();
    let noun = if count == 1 { "field is" } else { "fields are" };
    let list = introduced.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Some(Refusal {
        field: first.path.clone(),
        message: format!("{count} {noun} invalid: {list}"),
        hint: first.hint.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Inner(f64);
    impl Validate for Inner {
        fn check(&self, c: &mut Check) {
            c.range("width", self.0, 0.0, 1.0);
        }
    }
    struct Outer(Inner);
    impl Validate for Outer {
        fn check(&self, c: &mut Check) {
            c.nested("iris", &self.0);
            c.positive("size", -1.0);
        }
    }

    #[test]
    fn ranges_are_read_from_the_checks_with_their_paths_and_rules() {
        struct Rule(f64);
        impl Validate for Rule {
            fn check(&self, c: &mut Check) {
                c.range("outer", self.0, 0.0, 2.0);
                c.rule(&["outer"], self.0 > 1.0, "must be above the inner end");
            }
        }
        let r = Outer(Inner(2.0)).ranges();
        assert_eq!(r, [FieldRange { path: "iris/width".into(), lo: 0.0, hi: 1.0, note: None }]);
        let r = Rule(0.0).ranges();
        assert_eq!(r[0].note.as_deref(), Some("must be above the inner end"));
        assert!(Rule(0.0).validate().is_err() && Rule(1.5).validate().is_ok());
    }

    #[test]
    fn errors_carry_the_full_field_path_and_all_are_reported() {
        let e = Outer(Inner(2.0)).validate().unwrap_err();
        let paths: Vec<_> = e.0.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["iris/width", "size"]);
        assert!(Outer(Inner(0.5)).validate().is_err(), "size is still invalid");
        assert!(Inner(f64::NAN).validate().is_err(), "NaN is never in range");
        assert!(Inner(0.5).validate().is_ok());
    }

    #[test]
    fn only_the_problems_an_edit_introduces_are_refused() {
        let old = Problem::new("face/layout/eye_y", "too low");
        let before = vec![old.clone()];
        assert_eq!(refuse_introduced(&before, vec![old.clone()]), None, "an existing problem is not the edit's");
        let new = Problem { hint: Some("try 0.4".into()), ..Problem::new("face/layout/mouth_y", "above the eyes") };
        let other = Problem::new("face/layout/nose_y", "out of range");
        let refusal = refuse_introduced(&before, vec![old, new, other]).expect("two new problems");
        assert_eq!(refusal.field, "face/layout/mouth_y");
        assert_eq!(
            refusal.message,
            "2 fields are invalid: face/layout/mouth_y: above the eyes; face/layout/nose_y: out of range"
        );
        assert_eq!(refusal.hint.as_deref(), Some("try 0.4"));
        let one = refuse_introduced(&[], vec![Problem::new("a", "b")]).expect("one new problem");
        assert_eq!((one.message.as_str(), one.hint), ("1 field is invalid: a: b", None));
    }
}
