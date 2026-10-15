//! Document paths with globs, and the differences between two document snapshots: what the `doc.*` checks stand on.
//!
//! A path is `a/b/c` as [`omuzaki_doc_api::DocPath`] writes it. In a pattern a segment `*` stands for any one key or
//! array index and `**` for any number of segments, none included (so `characters/x/parts/**` is `parts` itself and
//! everything below it). An array item is addressed by its index, as [`omuzaki_doc_api::lookup`] does.

use std::collections::BTreeMap;

use serde_json::Value;

use super::number::same_value;

/// A document path as segments.
pub(super) type Segments = Vec<String>;

/// Splits a pattern into segments; the empty pattern is the root.
pub(super) fn split(pattern: &str) -> Segments {
    if pattern.is_empty() { Vec::new() } else { pattern.split('/').map(str::to_owned).collect() }
}

/// The joined form of a path, for messages and reports.
pub(super) fn join(path: &[String]) -> String {
    path.join("/")
}

/// Every value `pattern` reaches in `root`, by path, in path order.
pub(super) fn matches<'a>(root: &'a Value, pattern: &[String]) -> BTreeMap<Segments, &'a Value> {
    let mut found = BTreeMap::new();
    walk(root, pattern, &mut Vec::new(), &mut found);
    found
}

fn walk<'a>(value: &'a Value, pattern: &[String], at: &mut Segments, found: &mut BTreeMap<Segments, &'a Value>) {
    let Some((head, rest)) = pattern.split_first() else {
        found.insert(at.clone(), value);
        return;
    };
    if head == "**" {
        // Zero segments here, or one segment and `**` again below it.
        walk(value, rest, at, found);
        for (key, child) in children(value) {
            at.push(key);
            walk(child, pattern, at, found);
            at.pop();
        }
    } else if head == "*" {
        for (key, child) in children(value) {
            at.push(key);
            walk(child, rest, at, found);
            at.pop();
        }
    } else if let Some(child) = child(value, head) {
        at.push(head.clone());
        walk(child, rest, at, found);
        at.pop();
    }
}

fn children(value: &Value) -> Vec<(String, &Value)> {
    match value {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v)).collect(),
        Value::Array(items) => items.iter().enumerate().map(|(i, v)| (i.to_string(), v)).collect(),
        _ => Vec::new(),
    }
}

fn child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(key),
        Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    }
}

/// The value at an exact path, if any.
pub(super) fn at<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(root, |v, key| child(v, key))
}

/// Whether `pattern` matches some prefix of `path`: `path` is at or below something the pattern names.
pub(super) fn covers(pattern: &[String], path: &[String]) -> bool {
    match pattern.split_first() {
        None => true,
        Some((head, rest)) if head == "**" => {
            covers(rest, path) || path.split_first().is_some_and(|(_, tail)| covers(pattern, tail))
        }
        Some((head, rest)) => {
            path.split_first().is_some_and(|(first, tail)| (head == "*" || head == first) && covers(rest, tail))
        }
    }
}

/// Every place two documents differ, as the shallowest path that tells it: a changed leaf, a key or item only one side
/// has, or a value whose kind differs. Numbers compare as numbers (`1` is `1.0`). In path order.
pub(super) fn differences(before: &Value, after: &Value) -> Vec<Segments> {
    let mut out = Vec::new();
    diff(Some(before), Some(after), &mut Vec::new(), &mut out);
    out
}

fn diff(before: Option<&Value>, after: Option<&Value>, at: &mut Segments, out: &mut Vec<Segments>) {
    match (before, after) {
        (None, None) => {}
        (Some(b), Some(a)) => match (b, a) {
            (Value::Object(bm), Value::Object(am)) => {
                let keys: std::collections::BTreeSet<&String> = bm.keys().chain(am.keys()).collect();
                for key in keys {
                    at.push(key.clone());
                    diff(bm.get(key), am.get(key), at, out);
                    at.pop();
                }
            }
            (Value::Array(bi), Value::Array(ai)) => {
                for index in 0..bi.len().max(ai.len()) {
                    at.push(index.to_string());
                    diff(bi.get(index), ai.get(index), at, out);
                    at.pop();
                }
            }
            _ if same_value(b, a) => {}
            _ => out.push(at.clone()),
        },
        _ => out.push(at.clone()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn pat(text: &str) -> Segments {
        split(text)
    }

    #[test]
    fn globs_reach_keys_indexes_and_any_depth() {
        let doc = json!({"chars": {"a": {"name": "A", "tags": ["x", "y"]}, "b": {"name": "B"}}});
        let names: Vec<String> = matches(&doc, &pat("chars/*/name")).keys().map(|p| join(p)).collect();
        assert_eq!(names, ["chars/a/name", "chars/b/name"]);
        assert_eq!(matches(&doc, &pat("chars/a/tags/1")).into_values().collect::<Vec<_>>(), [&json!("y")]);
        // `**` includes the node itself and everything below it.
        let below: Vec<String> = matches(&doc, &pat("chars/b/**")).keys().map(|p| join(p)).collect();
        assert_eq!(below, ["chars/b", "chars/b/name"]);
        assert_eq!(matches(&doc, &pat("")).len(), 1);
        assert!(matches(&doc, &pat("chars/c")).is_empty());
        assert!(matches(&doc, &pat("chars/a/name/deeper")).is_empty());
    }

    #[test]
    fn a_pattern_covers_the_paths_at_or_below_what_it_names() {
        assert!(covers(&pat("chars/a/**"), &pat("chars/a/parts/head")));
        assert!(covers(&pat("chars/a"), &pat("chars/a/parts/head")));
        assert!(covers(&pat("chars/*/name"), &pat("chars/b/name")));
        assert!(covers(&pat("**/name"), &pat("chars/b/name")));
        assert!(!covers(&pat("chars/a/parts"), &pat("chars/a")));
        assert!(!covers(&pat("chars/a/**"), &pat("chars/b/name")));
        assert!(covers(&pat(""), &pat("anything")));
    }

    #[test]
    fn differences_are_the_shallowest_paths_that_tell_them() {
        let before = json!({"a": {"x": 1, "y": [1, 2]}, "gone": true, "n": 1});
        let after = json!({"a": {"x": 2, "y": [1, 2, 3]}, "new": {"deep": 1}, "n": 1.0});
        let found: Vec<String> = differences(&before, &after).iter().map(|p| join(p)).collect();
        assert_eq!(found, ["a/x", "a/y/2", "gone", "new"]);
        assert_eq!(differences(&before, &before).len(), 0);
    }
}
