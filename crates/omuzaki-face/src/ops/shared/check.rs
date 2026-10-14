//! What a face edit may introduce. A face is read section by section, and everything wrong with it is listed as a
//! [`Problem`] with its path: a value of the wrong kind, a key the default doesn't have, a section or mark that won't
//! read, and every out-of-range field the validator reports. An edit is refused only for the problems it adds, compared
//! with the stored face by path and message, as `omuzaki-char`'s `check_edit` does. A face that is already damaged
//! elsewhere therefore still takes unrelated edits.

use std::fmt;

use omuzaki_face_api::{FaceMark, FaceProfile, Validate};
use omuzaki_ops_api::OpError;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use super::path::{Template, channel, kind_of, unknown_key};

/// The fix for a field whose problem names none of its own.
const GENERIC_HINT: &str = "give each named field a value of the type it holds, inside the range its message gives";

/// One thing wrong with a face: where it is, what is wrong, and a fix when the problem names one.
#[derive(Clone, Debug)]
pub(crate) struct Problem {
    /// The path, for example `face/eyes/iris/width`.
    pub(crate) path: String,
    /// What is wrong, in a sentence.
    pub(crate) message: String,
    /// The fix, when the problem has one of its own (a nearby key).
    pub(crate) hint: Option<String>,
}

impl Problem {
    fn at(path: &str, message: impl Into<String>) -> Self {
        Self { path: path.to_owned(), message: message.into(), hint: None }
    }

    fn from_error(error: OpError) -> Self {
        Self { path: error.field.unwrap_or_default(), message: error.message, hint: error.hint }
    }

    /// Two problems are the same when they name the same path with the same message.
    fn is_same(&self, other: &Problem) -> bool {
        self.path == other.path && self.message == other.message
    }
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path, self.message)
    }
}

/// Refuses an edit that introduces a problem. The problems the stored face already has are not blamed on the edit, so
/// a face damaged elsewhere can still be edited, and an edit that leaves a damaged field damaged is refused only if its
/// own value is wrong in a new way.
///
/// # Errors
/// `Invalid`, naming the first problem the edit introduces and listing all of them.
pub(crate) fn check_edit(stored: Option<&Value>, working: &Value, defaults: &Template) -> Result<(), OpError> {
    let empty = Value::Object(Map::new());
    let before = diagnose(stored.unwrap_or(&empty), defaults);
    let introduced = added(&before, diagnose(working, defaults));
    let Some(first) = introduced.first() else {
        return Ok(());
    };
    let count = introduced.len();
    let noun = if count == 1 { "field is" } else { "fields are" };
    let list = introduced.iter().map(ToString::to_string).collect::<Vec<_>>().join("; ");
    Err(OpError::invalid(first.path.clone(), format!("{count} {noun} invalid: {list}"))
        .hint(first.hint.clone().unwrap_or_else(|| GENERIC_HINT.to_owned())))
}

/// Every problem in `face`, read against the defaults. Nothing stops at the first one: each section is read on its own,
/// each mark likewise, and the validator reports every field out of range.
pub(crate) fn diagnose(face: &Value, defaults: &Template) -> Vec<Problem> {
    let mut problems = Vec::new();
    let profile = FaceProfile {
        eyes: section(face, "eyes", defaults, &mut problems),
        brows: section(face, "brows", defaults, &mut problems),
        mouth: section(face, "mouth", defaults, &mut problems),
        nose: section(face, "nose", defaults, &mut problems),
        cheeks: section(face, "cheeks", defaults, &mut problems),
        marks: marks(face, defaults, &mut problems),
        mapping: section(face, "mapping", defaults, &mut problems),
        ..FaceProfile::default()
    };
    rest(face, &mut problems);
    if let Err(errors) = profile.validate() {
        problems.extend(errors.0.into_iter().map(|e| Problem::at(&format!("face/{}", e.path), e.message)));
    }
    problems
}

/// The problems in `after` that are not in `before`.
fn added(before: &[Problem], after: Vec<Problem>) -> Vec<Problem> {
    after.into_iter().filter(|p| !before.iter().any(|b| b.is_same(p))).collect()
}

/// One of the five sections that are a single struct, read from its part of the face.
fn section<T: DeserializeOwned + Default>(
    face: &Value,
    name: &str,
    defaults: &Template,
    problems: &mut Vec<Problem>,
) -> T {
    let Some(value) = face.get(name) else {
        return T::default();
    };
    let at = format!("face/{name}");
    let template = defaults.profile.get(name).unwrap_or(&Value::Null);
    let repaired = repair(value, template, &at, problems);
    T::deserialize(repaired).unwrap_or_else(|e| {
        problems.push(Problem::at(&at, format!("doesn't read: {e}")));
        T::default()
    })
}

/// The resting bus layer: a map of channel name to number. Each name must be a channel and each value a number inside the
/// channel's range. Every bad channel is reported, with its own path.
fn rest(face: &Value, problems: &mut Vec<Problem>) {
    let Some(value) = face.get("rest") else {
        return;
    };
    let Value::Object(given) = value else {
        problems.push(Problem::at("face/rest", format!("expected a map of channels, found {}", kind_of(value))));
        return;
    };
    for (name, child) in given {
        let at = format!("face/rest/{name}");
        let found = match channel(name) {
            Ok(found) => found,
            Err(error) => {
                problems.push(Problem::from_error(error));
                continue;
            }
        };
        match child.as_f64() {
            None => problems.push(Problem::at(&at, format!("expected a number, found {}", kind_of(child)))),
            Some(number) => {
                let (lo, hi) = found.range();
                if !(number.is_finite() && (f64::from(lo)..=f64::from(hi)).contains(&number)) {
                    problems.push(Problem::at(
                        &at,
                        format!("{name} = {number} is out of range: use a number from {lo} to {hi}"),
                    ));
                }
            }
        }
    }
}

/// The marks list, each entry read on its own, so a bad mark is named by its position.
fn marks(face: &Value, defaults: &Template, problems: &mut Vec<Problem>) -> Vec<FaceMark> {
    let Some(value) = face.get("marks") else {
        return Vec::new();
    };
    let Value::Array(items) = value else {
        problems.push(Problem::at("face/marks", format!("expected a list, found {}", kind_of(value))));
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let at = format!("face/marks/{i}");
            let repaired = repair(item, &defaults.mark, &at, problems);
            FaceMark::deserialize(repaired).unwrap_or_else(|e| {
                problems.push(Problem::at(&at, format!("doesn't read: {e}")));
                FaceMark::default()
            })
        })
        .collect()
}

/// Walks `value` against the default `template` at `at`. Each key the default doesn't have, and each value of the wrong
/// kind or the wrong length, becomes a problem and is put back to its default, so the rest of the value still reads.
fn repair(value: &Value, template: &Value, at: &str, problems: &mut Vec<Problem>) -> Value {
    match (template, value) {
        (Value::Object(defaults), Value::Object(given)) => {
            let mut kept = Map::new();
            for (key, child) in given {
                match defaults.get(key) {
                    Some(default) => {
                        let repaired = repair(child, default, &format!("{at}/{key}"), problems);
                        kept.insert(key.clone(), repaired);
                    }
                    None => problems.push(Problem::from_error(unknown_key(at, key, defaults.keys()))),
                }
            }
            Value::Object(kept)
        }
        (Value::Array(defaults), Value::Array(given)) if defaults.len() == given.len() => Value::Array(
            given
                .iter()
                .zip(defaults)
                .enumerate()
                .map(|(i, (child, default))| repair(child, default, &format!("{at}/{i}"), problems))
                .collect(),
        ),
        (Value::Array(defaults), Value::Array(given)) => {
            problems.push(Problem::at(at, format!("expected {} items, found {}", defaults.len(), given.len())));
            template.clone()
        }
        // A scalar of the kind the default holds is kept as it is.
        (Value::Null, _)
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_)) => value.clone(),
        _ => {
            problems.push(Problem::at(at, format!("expected {}, found {}", kind_of(template), kind_of(value))));
            template.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::shared::path::template;

    #[test]
    fn a_default_face_has_no_problems() {
        let defaults = template().expect("the default face serialises");
        assert!(diagnose(&defaults.profile, &defaults).is_empty());
    }

    #[test]
    fn wrong_kinds_and_unknown_keys_are_all_listed_with_their_paths() {
        let defaults = template().expect("the default face serialises");
        let face = serde_json::json!({ "eyes": { "scale": 99, "iris": { "width": "big", "bogus": 1 } } });
        let paths: Vec<String> = diagnose(&face, &defaults).into_iter().map(|p| p.path).collect();
        assert_eq!(paths.len(), 3, "{paths:?}");
        for expected in ["face/eyes/scale", "face/eyes/iris/width", "face/eyes/iris/bogus"] {
            assert!(paths.iter().any(|p| p == expected), "{expected} missing from {paths:?}");
        }
    }
}
