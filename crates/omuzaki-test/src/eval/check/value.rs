//! Small helpers the checks share: JSON pointers with wildcards, name globs, movement tests and the read-only call.

use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner, Scope};
use serde_json::{Map, Value};

use super::report::{Outcome, Verdict};
use crate::eval::task::Change;

/// Runs a read-only op as the checker and returns its output.
///
/// The checker is a person named `eval-checker` holding `read` and `render` only: a person needs no session, and the
/// grants cap what any check can reach. Once #207 lands the checker becomes an agent principal with the same grants,
/// and this actor changes with it.
pub(super) fn call(runner: &dyn Runner, op: &str, args: &Map<String, Value>) -> Result<Value, OpError> {
    let request = Request {
        op: op.to_owned(),
        args: Value::Object(args.clone()),
        mode: Mode::Apply,
        actor: Actor::Person { name: "eval-checker".to_owned() },
        grants: Grants::of([Scope::Read, Scope::Render]),
        cause: None,
        tx: None,
        base: None,
    };
    let outcome = runner.run(request)?;
    outcome.plan.output.ok_or_else(|| OpError::internal(format!("`{op}` returned no output")))
}

/// The verdict for an op that failed on the **result** project: a fail when the agent broke what it reads, else an
/// error.
pub(super) fn result_failure(what: &str, error: &OpError) -> Verdict {
    let detail = format!("{what} on the result: {error}");
    match Outcome::of_result_error(error) {
        Outcome::Fail => Verdict::fail(detail),
        _ => Verdict::error(detail),
    }
}

/// Every value a JSON pointer reaches, where a `*` segment stands for each array item or object value.
///
/// # Errors
/// An `error` verdict when `pointer` is not empty and does not start with `/`: a pointer such as `counter` would
/// otherwise read the whole output like the empty pointer, and a broken check could pass.
pub(super) fn pointer_all<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a Value>, Verdict> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(Verdict::error(format!("pointer `{pointer}` is malformed: a non-empty pointer starts with `/`")));
    }
    let mut found = vec![value];
    for raw in pointer.split('/').skip(1) {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        found = found
            .into_iter()
            .flat_map(|v| match (v, segment.as_str()) {
                (Value::Array(items), "*") => items.iter().collect(),
                (Value::Object(map), "*") => map.values().collect(),
                (Value::Array(items), index) => {
                    index.parse::<usize>().ok().and_then(|i| items.get(i)).into_iter().collect()
                }
                (Value::Object(map), key) => map.get(key).into_iter().collect(),
                _ => Vec::new(),
            })
            .collect();
    }
    Ok(found)
}

/// Whether `text` matches `pattern`, where `*` stands for any run of characters (an op name or a name glob).
pub(super) fn glob_match(pattern: &str, text: &str) -> bool {
    let Some((head, rest)) = pattern.split_once('*') else { return pattern == text };
    let Some(tail) = text.strip_prefix(head) else { return false };
    (0..=tail.len()).filter(|&i| tail.is_char_boundary(i)).any(|i| glob_match(rest, &tail[i..]))
}

/// Whether a number moved from `before` to `after` the way `change` says, by at least `at_least` (a fraction of
/// `before` when `relative`). A move from zero counts as infinitely large when relative.
pub(super) fn moved(before: f64, after: f64, change: Change, at_least: Option<f64>, relative: bool) -> bool {
    let delta = after - before;
    let direction = match change {
        Change::Up => delta > 0.0,
        Change::Down => delta < 0.0,
        Change::Any => delta.abs() > f64::EPSILON,
        Change::None => return delta.abs() <= f64::EPSILON,
    };
    let size = match (relative, before.abs() <= f64::EPSILON) {
        (false, _) => delta.abs(),
        (true, false) => delta.abs() / before.abs(),
        (true, true) => f64::INFINITY,
    };
    direction && at_least.is_none_or(|least| size >= least)
}

/// A number as an exact integer, when it is one: a JSON integer, or a float with no fraction small enough that the
/// conversion loses nothing.
#[allow(clippy::cast_possible_truncation)] // the filter keeps the float whole and far inside `i128`
fn exact_integer(n: &serde_json::Number) -> Option<i128> {
    if let Some(i) = n.as_i64() {
        return Some(i128::from(i));
    }
    if let Some(u) = n.as_u64() {
        return Some(i128::from(u));
    }
    n.as_f64().filter(|f| f.is_finite() && f.fract() == 0.0 && f.abs() < 1.0e38).map(|f| f as i128)
}

/// Whether two JSON values are equal. Numbers are compared as numbers, so `1` equals `1.0`, but integers are compared
/// exactly: `9007199254740993` is not `9007199254740992`. Two non-integral floats must be the same float.
pub(super) fn same_value(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(l), Value::Number(r)) => match (exact_integer(l), exact_integer(r)) {
            (Some(li), Some(ri)) => li == ri,
            (None, None) => l.as_f64() == r.as_f64(),
            _ => false,
        },
        _ => left == right,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn pointers_expand_wildcards_over_arrays_and_objects() {
        let v = json!({"a": [{"id": "x"}, {"id": "y"}], "b": {"k": {"id": "z"}}});
        assert_eq!(pointer_all(&v, "/a/*/id").unwrap(), [&json!("x"), &json!("y")]);
        assert_eq!(pointer_all(&v, "/b/*/id").unwrap(), [&json!("z")]);
        assert_eq!(pointer_all(&v, "/a/1/id").unwrap(), [&json!("y")]);
        assert_eq!(pointer_all(&v, "/a/2/id").unwrap().len(), 0);
        assert_eq!(pointer_all(&v, "").unwrap(), [&v]);
    }

    #[test]
    fn a_pointer_without_a_leading_slash_is_a_malformed_check() {
        let v = json!({"counter": 5});
        for bad in ["counter", "a/b", "*", "~0"] {
            let verdict = pointer_all(&v, bad).unwrap_err();
            assert_eq!(verdict.outcome, Outcome::Error, "{bad}");
        }
    }

    #[test]
    fn equality_is_exact_for_integers_and_still_reads_one_as_one_point_zero() {
        let n = |text: &str| serde_json::from_str::<Value>(text).unwrap();
        assert!(same_value(&n("1"), &n("1.0")));
        assert!(same_value(&n("9007199254740993"), &n("9007199254740993")));
        assert!(!same_value(&n("9007199254740992"), &n("9007199254740993")));
        assert!(!same_value(&n("9007199254740993"), &n("9007199254740992")));
        assert!(!same_value(&n("9007199254740991"), &n("9007199254740992")));
        assert!(!same_value(&n("9007199254740993"), &n("9007199254740992.0")));
        assert!(same_value(&n("9007199254740992"), &n("9007199254740992.0")));
        assert!(!same_value(&n("18446744073709551615"), &n("18446744073709551614")));
        assert!(!same_value(&n("-9223372036854775808"), &n("-9223372036854775807")));
        assert!(!same_value(&n("1"), &n("1.0000000000000002")));
        assert!(!same_value(&n("0.1"), &n("0.10000000000000002")));
        assert!(same_value(&n("0.5"), &n("0.5")));
        assert!(!same_value(&n("1"), &json!("1")));
    }

    #[test]
    fn globs_match_prefixes_infixes_and_whole_names() {
        assert!(glob_match("face.*", "face.set_profile"));
        assert!(glob_match("*.set_*", "face.set_profile"));
        assert!(glob_match("face.set_profile", "face.set_profile"));
        assert!(!glob_match("face.*", "hair.set"));
        assert!(!glob_match("face", "face.set"));
    }

    #[test]
    fn movement_honours_direction_size_and_relative_size() {
        assert!(moved(1.0, 2.0, Change::Up, Some(1.0), false));
        assert!(!moved(1.0, 1.5, Change::Up, Some(1.0), false));
        assert!(moved(10.0, 5.0, Change::Down, Some(0.5), true));
        assert!(!moved(10.0, 9.0, Change::Down, Some(0.5), true));
        assert!(moved(3.0, 3.0, Change::None, None, false));
        assert!(!moved(3.0, 3.1, Change::None, None, false));
        assert!(moved(0.0, 0.1, Change::Any, Some(100.0), true));
    }
}
