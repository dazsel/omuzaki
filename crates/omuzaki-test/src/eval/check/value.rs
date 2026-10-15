//! Small helpers the checks share: JSON pointers with wildcards, name globs, movement tests and the read-only call.

use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner, Scope};
use serde_json::{Map, Value};

use super::report::{Outcome, Verdict};

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

/// Whether `pointer` is a JSON pointer: empty, or starting with `/`.
///
/// # Errors
/// An `error` verdict when it is not: a pointer such as `counter` would otherwise read the whole output like the
/// empty pointer, and a broken check could pass.
pub(super) fn well_formed(pointer: &str) -> Result<(), Verdict> {
    if pointer.is_empty() || pointer.starts_with('/') {
        Ok(())
    } else {
        Err(Verdict::error(format!("pointer `{pointer}` is malformed: a non-empty pointer starts with `/`")))
    }
}

/// Every value a JSON pointer reaches, where a `*` segment stands for each array item or object value.
///
/// # Errors
/// An `error` verdict when `pointer` is not empty and does not start with `/`: a pointer such as `counter` would
/// otherwise read the whole output like the empty pointer, and a broken check could pass.
pub(super) fn pointer_all<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a Value>, Verdict> {
    well_formed(pointer)?;
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
    fn globs_match_prefixes_infixes_and_whole_names() {
        assert!(glob_match("face.*", "face.set_profile"));
        assert!(glob_match("*.set_*", "face.set_profile"));
        assert!(glob_match("face.set_profile", "face.set_profile"));
        assert!(!glob_match("face.*", "hair.set"));
        assert!(!glob_match("face", "face.set"));
    }
}
