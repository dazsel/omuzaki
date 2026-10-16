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

/// Whether `pointer` is a JSON pointer: empty, or `/` tokens whose `~` escapes are `~0` or `~1` and nothing else.
///
/// Whether a token is a valid *array index* depends on what it indexes, so that rule runs in [`pointer_all`], where
/// the value is known: `01` is a legal object key and an illegal array index.
///
/// # Errors
/// An `error` verdict when it is not: a pointer such as `counter` would otherwise read the whole output like the
/// empty pointer, and a broken check could pass.
pub(super) fn well_formed(pointer: &str) -> Result<(), Verdict> {
    let malformed = |why: String| Verdict::error(format!("pointer `{pointer}` is malformed: {why}"));
    if pointer.is_empty() {
        return Ok(());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(malformed("a non-empty pointer starts with `/`".to_owned()));
    };
    for token in rest.split('/') {
        let mut chars = token.chars();
        while let Some(c) = chars.next() {
            if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err(malformed(format!("in `{token}`, `~` must be `~0` or `~1`")));
            }
        }
    }
    Ok(())
}

/// An array index by RFC 6901: `0`, or a non-zero digit followed by digits (no leading zero, sign, space or fraction).
fn array_index(token: &str) -> Option<usize> {
    let canonical = token == "0"
        || (token.starts_with(|c: char| ('1'..='9').contains(&c)) && token.bytes().all(|b| b.is_ascii_digit()));
    if canonical { token.parse().ok() } else { None }
}

/// Every value a JSON pointer reaches, where a `*` segment stands for each array item or object value.
///
/// # Errors
/// An `error` verdict when the pointer is malformed (see [`well_formed`]), or when a token that indexes an array is not
/// a canonical index: the pointer is never traversed on a guess at what it meant. Object keys may be anything.
pub(super) fn pointer_all<'a>(value: &'a Value, pointer: &str) -> Result<Vec<&'a Value>, Verdict> {
    well_formed(pointer)?;
    let mut found = vec![value];
    for raw in pointer.split('/').skip(1) {
        let segment = raw.replace("~1", "/").replace("~0", "~");
        let mut next = Vec::new();
        for v in found {
            match (v, segment.as_str()) {
                (Value::Array(items), "*") => next.extend(items),
                (Value::Object(map), "*") => next.extend(map.values()),
                (Value::Array(items), token) => {
                    let index = array_index(token).ok_or_else(|| {
                        Verdict::error(format!(
                            "pointer `{pointer}` is malformed: `{token}` indexes an array but is not a canonical index (`0`, or a non-zero digit then digits)"
                        ))
                    })?;
                    next.extend(items.get(index));
                }
                (Value::Object(map), key) => next.extend(map.get(key)),
                _ => {}
            }
        }
        found = next;
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
    fn pointer_escapes_are_checked_everywhere_and_indexes_only_where_they_index_an_array() {
        for good in ["", "/", "/a", "/a/0", "/a//b", "/a~0b", "/a~1b", "/~01", "/*", "/a-b", "/x.y", "/+", "/01", "/-1"]
        {
            assert!(well_formed(good).is_ok(), "{good}");
        }
        for bad in ["a", "/a~", "/a~2", "/a~/b"] {
            assert_eq!(well_formed(bad).unwrap_err().outcome, Outcome::Error, "{bad}");
        }
        // Object keys may look like numbers: `01`, `-1`, `1.0` and `+1` are keys here.
        let frames = json!({"frames": {"01": "a", "-1": "b", "1.0": "c", "+1": "d", " 1": "e"}});
        for (pointer, want) in
            [("/frames/01", "a"), ("/frames/-1", "b"), ("/frames/1.0", "c"), ("/frames/+1", "d"), ("/frames/ 1", "e")]
        {
            assert_eq!(pointer_all(&frames, pointer).unwrap(), [&json!(want)], "{pointer}");
        }
        // The same tokens index an array only when canonical, and a bad one is never read as the index it resembles.
        let items = json!({"items": ["zero", "one"]});
        for ok in [("/items/0", "zero"), ("/items/1", "one")] {
            assert_eq!(pointer_all(&items, ok.0).unwrap(), [&json!(ok.1)]);
        }
        for bad in
            ["/items/01", "/items/00", "/items/+1", "/items/-1", "/items/-", "/items/ 1", "/items/1 ", "/items/1.0"]
        {
            let verdict = pointer_all(&items, bad).unwrap_err();
            assert_eq!(verdict.outcome, Outcome::Error, "{bad}");
            assert!(verdict.detail.contains("canonical"), "{bad}: {}", verdict.detail);
        }
        assert_eq!(pointer_all(&items, "/items/2").unwrap().len(), 0);
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
