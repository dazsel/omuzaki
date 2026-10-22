//! What a face edit may introduce. A face is read section by section, and everything wrong with it is listed as a
//! [`Problem`] with its path: a value of the wrong kind, a key the default doesn't have, a section or mark that won't
//! read, and every out-of-range field the validator reports. An edit is refused only for the problems it adds, compared
//! with the stored face by path and message, with the comparison `omuzaki-char`'s layout ops use too (`omuzaki_face_api::refuse_introduced`). A face that is already damaged
//! elsewhere therefore still takes unrelated edits.

use omuzaki_face_api::{FaceMark, FaceProfile, Problem, Validate, refuse_introduced};
use omuzaki_ops_api::OpError;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use serde_path_to_error::Segment;

use super::path::{Template, channel, kind_of, unknown_key};

/// The fix for a field whose problem names none of its own.
const GENERIC_HINT: &str = "give each named field a value of the type it holds, inside the range its message gives";

/// A problem found in a face, from an op's own refusal (its field, message and hint).
fn from_error(error: OpError) -> Problem {
    Problem { path: error.field.unwrap_or_default(), message: error.message, hint: error.hint }
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
    let Some(refusal) = refuse_introduced(&before, diagnose(working, defaults)) else {
        return Ok(());
    };
    Err(OpError::invalid(refusal.field, refusal.message).hint(refusal.hint.unwrap_or_else(|| GENERIC_HINT.to_owned())))
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
        problems.extend(errors.0.into_iter().map(|e| Problem::new(format!("face/{}", e.path), e.message)));
    }
    problems
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
    read(repaired, template, &at, problems)
}

/// Reads `value` as a `T` and lists every leaf that doesn't read, each at its own path (`face/nose/kind`, not
/// `face/nose`). `serde_path_to_error` names the first bad leaf; that leaf is put back to its default in `template` and
/// the value read again, so the next one is found too. Each round removes one bad leaf of a finite value, so it ends.
/// A failure with no leaf to name or no default to put back is reported at `at` and reads as the default.
fn read<T: DeserializeOwned + Default>(mut value: Value, template: &Value, at: &str, problems: &mut Vec<Problem>) -> T {
    loop {
        let error = match serde_path_to_error::deserialize::<_, T>(value.clone()) {
            Ok(read) => return read,
            Err(error) => error,
        };
        let steps = leaf_steps(error.path());
        let leaf = steps
            .as_ref()
            .map_or_else(|| at.to_owned(), |steps| steps.iter().fold(at.to_owned(), |p, s| format!("{p}/{s}")));
        problems.push(Problem::new(leaf, error.inner().to_string()));
        let restored = steps.is_some_and(|steps| restore_default(&mut value, template, &steps));
        if !restored {
            return T::default();
        }
    }
}

/// The keys and positions `path` goes through, or `None` when it passes through something a JSON value has no key for.
fn leaf_steps(path: &serde_path_to_error::Path) -> Option<Vec<String>> {
    path.iter()
        .map(|segment| match segment {
            Segment::Map { key } => Some(key.clone()),
            Segment::Seq { index } => Some(index.to_string()),
            Segment::Enum { .. } | Segment::Unknown => None,
        })
        .collect()
}

/// Puts the leaf at `steps` of `value` back to what `template` holds there. `false` when there is no such leaf in
/// either, or it already holds the default, so a caller that retries cannot loop.
fn restore_default(value: &mut Value, template: &Value, steps: &[String]) -> bool {
    let (mut here, mut default) = (value, template);
    for step in steps {
        let (Some(next_value), Some(next_default)) = (child_mut(here, step), child(default, step)) else {
            return false;
        };
        (here, default) = (next_value, next_default);
    }
    if *here == *default {
        return false;
    }
    *here = default.clone();
    true
}

/// The child of `node` at key or position `step`.
fn child<'v>(node: &'v Value, step: &str) -> Option<&'v Value> {
    match node {
        Value::Object(map) => map.get(step),
        Value::Array(items) => items.get(step.parse::<usize>().ok()?),
        _ => None,
    }
}

/// The child of `node` at key or position `step`, to change.
fn child_mut<'v>(node: &'v mut Value, step: &str) -> Option<&'v mut Value> {
    match node {
        Value::Object(map) => map.get_mut(step),
        Value::Array(items) => items.get_mut(step.parse::<usize>().ok()?),
        _ => None,
    }
}

/// The resting bus layer: a map of channel name to number. Each name must be a channel and each value a number inside the
/// channel's range. Every bad channel is reported, with its own path.
fn rest(face: &Value, problems: &mut Vec<Problem>) {
    let Some(value) = face.get("rest") else {
        return;
    };
    let Value::Object(given) = value else {
        problems.push(Problem::new("face/rest", format!("expected a map of channels, found {}", kind_of(value))));
        return;
    };
    for (name, child) in given {
        let at = format!("face/rest/{name}");
        let found = match channel(name) {
            Ok(found) => found,
            Err(error) => {
                problems.push(from_error(error));
                continue;
            }
        };
        match child.as_f64() {
            None => problems.push(Problem::new(at, format!("expected a number, found {}", kind_of(child)))),
            Some(number) => {
                let (lo, hi) = found.range();
                if !(number.is_finite() && (f64::from(lo)..=f64::from(hi)).contains(&number)) {
                    problems.push(Problem::new(
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
        problems.push(Problem::new("face/marks", format!("expected a list, found {}", kind_of(value))));
        return Vec::new();
    };
    items
        .iter()
        .enumerate()
        .map(|(i, item)| {
            let at = format!("face/marks/{i}");
            let repaired = repair(item, &defaults.mark, &at, problems);
            read(repaired, &defaults.mark, &at, problems)
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
                    None => problems.push(from_error(unknown_key(at, key, defaults.keys()))),
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
            problems.push(Problem::new(at, format!("expected {} items, found {}", defaults.len(), given.len())));
            template.clone()
        }
        // A scalar of the kind the default holds is kept as it is.
        (Value::Null, _)
        | (Value::Number(_), Value::Number(_))
        | (Value::String(_), Value::String(_))
        | (Value::Bool(_), Value::Bool(_)) => value.clone(),
        _ => {
            problems.push(Problem::new(at, format!("expected {}, found {}", kind_of(template), kind_of(value))));
            template.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops_shared::path::template;
    use serde_json::json;

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

    #[test]
    fn a_leaf_that_cannot_be_put_back_is_reported_once_and_reads_as_the_default() {
        let mut problems = Vec::new();
        // The template has no `kind` to restore, so the read gives up after naming the leaf instead of looping.
        let read: omuzaki_face_api::NoseStyle =
            super::read(json!({ "kind": "dott" }), &Value::Null, "face/nose", &mut problems);
        assert_eq!(read, omuzaki_face_api::NoseStyle::default());
        let paths: Vec<&str> = problems.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(paths, ["face/nose/kind"]);
    }
}
