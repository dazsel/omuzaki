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
    let malformed = |why: String| Verdict::error(format!("pointer `{pointer}` is malformed: {why}"));
    if pointer.is_empty() {
        return Ok(());
    }
    let Some(rest) = pointer.strip_prefix('/') else {
        return Err(malformed("a non-empty pointer starts with `/`".to_owned()));
    };
    rest.split('/').try_for_each(|token| valid_token(token).map_err(malformed))
}

/// One pointer token, by RFC 6901: `~` is `~0` or `~1` and nothing else, and a token that reads as a number is a
/// canonical array index, `0` or a non-zero digit followed by digits (no leading zeros, `+`, `-`, spaces or fraction).
/// A token that looks like a number but is not one is rejected rather than read as the index it resembles.
fn valid_token(token: &str) -> Result<(), String> {
    let mut chars = token.chars();
    while let Some(c) = chars.next() {
        if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
            return Err(format!("in `{token}`, `~` must be `~0` or `~1`"));
        }
    }
    let number_like = !token.is_empty()
        && token.chars().all(|c| c.is_ascii_digit() || matches!(c, '+' | '-' | '.') || c.is_whitespace())
        && (token == "-" || token.chars().any(|c| c.is_ascii_digit()));
    let canonical = token == "0"
        || (token.starts_with(|c: char| ('1'..='9').contains(&c)) && token.chars().all(|c| c.is_ascii_digit()));
    if number_like && !canonical {
        return Err(format!("`{token}` is not a canonical array index (`0`, or a non-zero digit then digits)"));
    }
    Ok(())
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
    fn pointer_tokens_follow_rfc_6901() {
        for good in ["", "/", "/a", "/a/0", "/a/10", "/a//b", "/a~0b", "/a~1b", "/~01", "/*", "/a-b", "/x.y", "/+"] {
            assert!(well_formed(good).is_ok(), "{good}");
        }
        for bad in [
            "a", "/a/01", "/a/00", "/a/+1", "/a/-1", "/a/-", "/a/ 1", "/a/1 ", "/a/1.0", "/a/\t1", "/a~", "/a~2",
            "/a~/b",
        ] {
            let verdict = well_formed(bad).unwrap_err();
            assert_eq!(verdict.outcome, Outcome::Error, "{bad}");
        }
        // A malformed pointer is never traversed: `/items/01` does not read element 1.
        let v = json!({"items": ["zero", "one"]});
        assert!(pointer_all(&v, "/items/01").is_err());
        assert_eq!(pointer_all(&v, "/items/1").unwrap(), [&json!("one")]);
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
