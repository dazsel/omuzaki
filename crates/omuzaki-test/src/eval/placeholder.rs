//! `{name}` placeholders: how a task, a fixture and a script refer to the ids the fixture exports or the agent made.

use std::collections::BTreeMap;

use serde_json::Value;

/// The first `{name}` in `text`: braces round a plain name (letters, digits, `_`, `.`, `-`), so JSON text such as
/// `{"a": 1}` is left alone.
pub(crate) fn first_in(text: &str) -> Option<String> {
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        let after = &rest[open + 1..];
        let close = after.find('}')?;
        let name = &after[..close];
        let plain = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-');
        if !name.is_empty() && name.chars().all(plain) {
            return Some(name.to_owned());
        }
        rest = after;
    }
    None
}

/// `value` with every `{name}` of `names` replaced by its id, in strings and in object keys alike.
///
/// # Errors
/// The first `{name}` that nothing in `names` defines: a misspelt export must stop the script, not reach the server as
/// text.
pub(crate) fn fill(value: &Value, names: &BTreeMap<String, String>) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => Value::String(fill_text(text, names)?),
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, names)).collect::<Result<_, _>>()?),
        Value::Object(map) => Value::Object(
            map.iter().map(|(k, v)| Ok((fill_text(k, names)?, fill(v, names)?))).collect::<Result<_, String>>()?,
        ),
        other => other.clone(),
    })
}

fn fill_text(text: &str, names: &BTreeMap<String, String>) -> Result<String, String> {
    let mut out = text.to_owned();
    for (name, id) in names {
        out = out.replace(&format!("{{{name}}}"), id);
    }
    match first_in(&out) {
        Some(left) => {
            let known: Vec<&str> = names.keys().map(String::as_str).collect();
            Err(format!(
                "`{{{left}}}` is not defined: a script may name {}",
                if known.is_empty() { "no exports".to_owned() } else { known.join(", ") }
            ))
        }
        None => Ok(out),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn placeholders_are_braces_round_a_plain_name() {
        assert_eq!(first_in("a {main} b").as_deref(), Some("main"));
        assert_eq!(first_in("{0.id}").as_deref(), Some("0.id"));
        assert_eq!(first_in("x { y } {z-1}").as_deref(), Some("z-1"));
        assert_eq!(first_in("{\"a\": 1} {}"), None);
        assert_eq!(first_in("open { only"), None);
    }

    #[test]
    fn names_are_filled_everywhere_and_an_undefined_one_is_an_error() {
        let names: BTreeMap<String, String> = [("main".to_owned(), "c1".to_owned())].into();
        let filled = fill(&json!({"character": "{main}", "list": ["x-{main}"], "{main}": 1, "n": 2}), &names).unwrap();
        assert_eq!(filled, json!({"character": "c1", "list": ["x-c1"], "c1": 1, "n": 2}));
        let error = fill(&json!({"character": "{mian}"}), &names).unwrap_err();
        assert!(error.contains("{mian}") && error.contains("main"), "{error}");
        assert_eq!(fill(&json!("{\"a\": 1}"), &names).unwrap(), json!("{\"a\": 1}"));
    }
}
