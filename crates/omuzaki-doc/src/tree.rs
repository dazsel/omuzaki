//! In-place edits of the document value, with the same rules as `omuzaki-doc-mock`, plus what TOML files add to them.

use omuzaki_doc_api::{Change, DocPath, PatchError};
use serde_json::Value;

use crate::toml_write::check_storable;

/// A document value as text for an error message.
pub(crate) fn show(v: Option<&Value>) -> String {
    v.map_or_else(|| "absent".to_owned(), Value::to_string)
}

/// Refuses changes that no set of TOML files could represent, before anything is touched.
///
/// - The root is the folder itself, so it can't be replaced; its entries can.
/// - A top-level entry is a file or folder, so it must be an object.
/// - TOML has no `null`, and its integers stop at `i64::MAX`.
pub(crate) fn check_change(change: &Change) -> Result<(), PatchError> {
    let refuse = |reason: String| PatchError::Unreachable { path: change.path.clone(), reason };
    match change.path.segments().len() {
        0 => return Err(refuse("the document root can't be replaced; change its entries instead".to_owned())),
        1 if change.after.as_ref().is_some_and(|v| !v.is_object()) => {
            return Err(refuse("top-level entries are files or folders, so they must be objects".to_owned()));
        }
        _ => {}
    }
    match &change.after {
        Some(after) => check_storable(after).map_err(|u| {
            let place = if u.at.is_empty() { change.path.to_string() } else { format!("{}/{}", change.path, u.at) };
            refuse(format!("{} (found at {place})", u.reason))
        }),
        None => Ok(()),
    }
}

/// Sets (or removes, for `None`) the value at `path`.
///
/// The parent must already exist: auto-creating parents would make undo inexact, because the inverse patch can't
/// know to remove them. Arrays change in place, grow by one at the end, or shrink by popping the last item.
pub(crate) fn set(root: &mut Value, path: &DocPath, value: Option<Value>) -> Result<(), PatchError> {
    let unreachable = |reason: &str| PatchError::Unreachable { path: path.clone(), reason: reason.to_owned() };
    let Some((parent_path, last)) = path.split_last() else {
        return Err(unreachable("the document root can't be replaced"));
    };
    let mut cur = root;
    for seg in parent_path.segments() {
        cur = match cur {
            Value::Object(map) => {
                map.get_mut(seg).ok_or_else(|| unreachable("a parent doesn't exist; create it first"))?
            }
            Value::Array(arr) => {
                let i: usize = seg.parse().map_err(|_| unreachable("array index isn't a number"))?;
                arr.get_mut(i).ok_or_else(|| unreachable("array index out of range"))?
            }
            _ => return Err(unreachable("a parent isn't an object or array")),
        };
    }
    match (cur, value) {
        (Value::Object(map), Some(v)) => {
            map.insert(last.to_owned(), v);
        }
        (Value::Object(map), None) => {
            map.remove(last);
        }
        (Value::Array(arr), v) => {
            let i: usize = last.parse().map_err(|_| unreachable("array index isn't a number"))?;
            match v {
                Some(v) if i < arr.len() => arr[i] = v,
                Some(v) if i == arr.len() => arr.push(v),
                None if i + 1 == arr.len() => {
                    arr.pop();
                }
                _ => return Err(unreachable("arrays only change in place, append at the end or pop the last item")),
            }
        }
        _ => return Err(unreachable("the parent isn't an object or array")),
    }
    Ok(())
}

/// The smallest set of paths at which `before` and `after` differ: objects are compared key by key, and anything
/// else (a number, a list, an object that appeared or disappeared) is one path.
pub(crate) fn changed_paths(before: &Value, after: &Value) -> Vec<DocPath> {
    fn walk(before: &Value, after: &Value, at: &DocPath, out: &mut Vec<DocPath>) {
        match (before, after) {
            (Value::Object(a), Value::Object(b)) => {
                for key in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
                    match (a.get(key), b.get(key)) {
                        (Some(x), Some(y)) => walk(x, y, &at.join(key.as_str()), out),
                        _ => out.push(at.join(key.as_str())),
                    }
                }
            }
            _ if before != after => out.push(at.clone()),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(before, after, &DocPath::root(), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn changed_paths_are_minimal() {
        let before = json!({"a": {"x": 1, "y": [1, 2]}, "b": {"z": 1}, "c": 1});
        let after = json!({"a": {"x": 1, "y": [1, 3]}, "d": {}, "c": 1});
        let paths: Vec<String> = changed_paths(&before, &after).iter().map(ToString::to_string).collect();
        assert_eq!(paths, ["a/y", "b", "d"]);
        assert_eq!(changed_paths(&before, &before), Vec::<DocPath>::new());
    }

    fn change(path: &str, after: Option<Value>) -> Change {
        Change { path: DocPath::parse(path).unwrap(), before: None, after }
    }

    #[test]
    fn the_root_and_top_level_scalars_are_refused() {
        assert!(check_change(&change("", Some(json!({})))).is_err());
        assert!(check_change(&change("a", Some(json!(1)))).is_err());
        assert!(check_change(&change("a", Some(json!({"b": 1})))).is_ok());
        assert!(check_change(&change("a/b", Some(json!(1)))).is_ok());
        assert!(check_change(&change("a", None)).is_ok());
    }

    #[test]
    fn null_is_refused_and_named() {
        let err = check_change(&change("a/b", Some(json!({"c": [null]})))).unwrap_err();
        let PatchError::Unreachable { path, reason } = err else { panic!("wrong error") };
        assert_eq!(path.to_string(), "a/b");
        assert!(reason.contains("null") && reason.contains("a/b/c/0"), "{reason}");
    }

    #[test]
    fn set_never_creates_parents() {
        let mut root = json!({});
        assert!(set(&mut root, &DocPath::parse("a/b").unwrap(), Some(json!(1))).is_err());
        assert_eq!(root, json!({}));
    }
}
