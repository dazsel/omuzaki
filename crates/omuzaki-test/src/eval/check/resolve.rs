//! The one step every check goes through before it runs: its names filled in, nothing left unresolved, its pointers
//! well-formed. A check that fails here is broken, and is an `error`, never the attempt's `fail`: the checks themselves
//! are only reached through [`check`], so no kind can skip it.

use std::collections::BTreeMap;

use serde_json::Value;

use super::report::Verdict;
use super::value::well_formed;
use crate::eval::task::{CheckKind, Region};

/// The names a task's checks may use, by name: the fixture's exports and the task's bindings. A name that did not
/// resolve holds the verdict of the check that uses it.
pub(super) type Names = BTreeMap<String, Result<String, Verdict>>;

/// The check with its names filled in.
///
/// # Errors
/// An `error` verdict when a name it uses did not resolve, a `{name}` is left that nothing defines (a misspelt
/// export or binding), or a pointer is malformed.
pub(super) fn check(kind: &CheckKind, names: &Names) -> Result<CheckKind, Verdict> {
    let mut value =
        serde_json::to_value(kind).map_err(|e| Verdict::error(format!("check cannot be serialised: {e}")))?;
    substitute(&mut value, names)?;
    if let Some(left) = unresolved(&value) {
        let known: Vec<&str> = names.keys().map(String::as_str).collect();
        return Err(Verdict::error(format!(
            "`{{{left}}}` is not defined: a check may name {}",
            if known.is_empty() { "no exports or bindings".to_owned() } else { known.join(", ") }
        )));
    }
    let kind = serde_json::from_value::<CheckKind>(value)
        .map_err(|e| Verdict::error(format!("check with names filled in is invalid: {e}")))?;
    pointers(&kind).into_iter().try_for_each(well_formed)?;
    Ok(kind)
}

/// Replaces `{name}` in every string of `value` with the id the name resolved to.
fn substitute(value: &mut Value, names: &Names) -> Result<(), Verdict> {
    match value {
        Value::String(text) => {
            for (name, id) in names {
                let placeholder = format!("{{{name}}}");
                if text.contains(&placeholder) {
                    *text = text.replace(&placeholder, id.as_ref().map_err(Clone::clone)?);
                }
            }
            Ok(())
        }
        Value::Array(items) => items.iter_mut().try_for_each(|v| substitute(v, names)),
        Value::Object(map) => map.values_mut().try_for_each(|v| substitute(v, names)),
        _ => Ok(()),
    }
}

/// The first `{name}` still in a string or key of `value`: braces round a plain name, as a placeholder is written.
fn unresolved(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => placeholder_in(text),
        Value::Array(items) => items.iter().find_map(unresolved),
        Value::Object(map) => map.iter().find_map(|(key, v)| placeholder_in(key).or_else(|| unresolved(v))),
        _ => None,
    }
}

fn placeholder_in(text: &str) -> Option<String> {
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

/// Every JSON pointer into an op's output that the check names.
fn pointers(kind: &CheckKind) -> Vec<&str> {
    match kind {
        CheckKind::Query { at, .. } => vec![at],
        CheckKind::RenderDiff { region: Region::FromOp(from), .. } => vec![&from.at],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn placeholders_are_braces_round_a_plain_name() {
        assert_eq!(placeholder_in("a {main} b").as_deref(), Some("main"));
        assert_eq!(placeholder_in("{0.id}").as_deref(), Some("0.id"));
        assert_eq!(placeholder_in("x { y } {z-1}").as_deref(), Some("z-1"));
        assert_eq!(placeholder_in("{\"a\": 1} {}"), None);
        assert_eq!(placeholder_in("open { only"), None);
        assert_eq!(unresolved(&json!({"k": ["fine", {"{gone}": 1}]})).as_deref(), Some("gone"));
    }
}
