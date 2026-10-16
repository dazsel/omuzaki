//! Building a fixture (design 4): its op script, run as the person `eval-setup` on an empty project.
//!
//! A fixture is code, never a committed folder, so it can only hold what the ops can build and schema changes cannot
//! strand it. The built folder is copied for every attempt; the build itself happens once per run.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use omuzaki_ops_api::{Actor, Grants, Mode, Request};
use serde_json::{Map, Value};

use super::fixture::{Fixture, SetItemRef};
use super::host::open_project_runner;

/// The project name every fixture is built under.
const PROJECT_NAME: &str = "eval";

/// A fixture, built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Built {
    /// The names a task's checks and scripts can use (`main`), resolved to ids.
    pub exports: BTreeMap<String, String>,
}

/// Why a fixture could not be built: the task is refused before any attempt, since no attempt of it could mean anything.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildError {
    /// The empty project could not be created or opened.
    #[error("couldn't create the project: {0}")]
    Project(String),
    /// A step's arguments refer to something that does not exist.
    #[error("step {step} (`{op}`): {reason}")]
    Reference {
        /// The step, from 0.
        step: usize,
        /// Its op.
        op: String,
        /// What does not resolve.
        reason: String,
    },
    /// A step's op failed.
    #[error("step {step} (`{op}`) failed: {reason}")]
    Step {
        /// The step, from 0.
        step: usize,
        /// Its op.
        op: String,
        /// The op's error.
        reason: String,
    },
    /// An export does not resolve.
    #[error("export `{name}`: {reason}")]
    Export {
        /// The export.
        name: String,
        /// What does not resolve.
        reason: String,
    },
}

/// Builds `fixture` in the empty folder `into`.
///
/// `item_path` turns a reference to an eval-set item into the path of its verified file, which is what an op that reads
/// a file takes; it refuses an item the loader refuses.
///
/// # Errors
/// [`BuildError`] naming the step that failed.
pub fn build(
    fixture: &Fixture,
    into: &Path,
    item_path: &dyn Fn(&SetItemRef) -> Result<String, String>,
) -> Result<Built, BuildError> {
    omuzaki_doc::Project::create(into, PROJECT_NAME).map_err(|e| BuildError::Project(e.to_string()))?;
    let runner = open_project_runner(into).map_err(|e| BuildError::Project(e.to_string()))?;
    let mut outputs: Vec<Value> = Vec::new();
    for (index, step) in fixture.steps.iter().enumerate() {
        let reference = |reason: String| BuildError::Reference { step: index, op: step.op.clone(), reason };
        let args = refer(&Value::Object(step.args.clone()), &outputs, item_path).map_err(reference)?;
        let request = Request {
            op: step.op.clone(),
            args,
            mode: Mode::Apply,
            actor: Actor::Person { name: "eval-setup".to_owned() },
            grants: Grants::person(),
            cause: None,
            tx: None,
            base: None,
        };
        let outcome = runner.run(request).map_err(|e| BuildError::Step {
            step: index,
            op: step.op.clone(),
            reason: e.to_string(),
        })?;
        outputs.push(outcome.plan.output.unwrap_or(Value::Null));
    }
    let mut exports = BTreeMap::new();
    for (name, template) in &fixture.exports {
        let value = refer(&Value::String(template.clone()), &outputs, item_path)
            .map_err(|reason| BuildError::Export { name: name.clone(), reason })?;
        let Value::String(id) = value else {
            return Err(BuildError::Export { name: name.clone(), reason: "an export is text".to_owned() });
        };
        exports.insert(name.clone(), id);
    }
    Ok(Built { exports })
}

/// `value` with every `{n.path}` replaced by the text at `path` in the output of step `n`, and every `{ set, item }` by
/// the path of the item's file.
fn refer(
    value: &Value,
    outputs: &[Value],
    item_path: &dyn Fn(&SetItemRef) -> Result<String, String>,
) -> Result<Value, String> {
    Ok(match value {
        Value::String(text) => Value::String(refer_text(text, outputs)?),
        Value::Array(items) => {
            Value::Array(items.iter().map(|v| refer(v, outputs, item_path)).collect::<Result<_, _>>()?)
        }
        Value::Object(map) => {
            if let (Some(Value::String(set)), Some(Value::String(item)), 2) =
                (map.get("set"), map.get("item"), map.len())
            {
                return item_path(&SetItemRef { set: set.clone(), item: item.clone() }).map(Value::String);
            }
            Value::Object(
                map.iter()
                    .map(|(k, v)| Ok((k.clone(), refer(v, outputs, item_path)?)))
                    .collect::<Result<Map<_, _>, String>>()?,
            )
        }
        other => other.clone(),
    })
}

fn refer_text(text: &str, outputs: &[Value]) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let Some(close) = after.find('}') else {
            out.push_str(&rest[open..]);
            return Ok(out);
        };
        let inner = &after[..close];
        if let Some((step, path)) = step_reference(inner) {
            out.push_str(&resolve(step, &path, outputs)?);
        } else {
            out.push('{');
            out.push_str(inner);
            out.push('}');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// `n.a.b` as step `n` and the keys `a`, `b`; anything else is not a reference.
fn step_reference(inner: &str) -> Option<(usize, Vec<&str>)> {
    let mut parts = inner.split('.');
    let step = parts.next()?.parse::<usize>().ok()?;
    let path: Vec<&str> = parts.collect();
    (!path.is_empty() && path.iter().all(|p| !p.is_empty())).then_some((step, path))
}

fn resolve(step: usize, path: &[&str], outputs: &[Value]) -> Result<String, String> {
    let output = outputs.get(step).ok_or_else(|| {
        format!("`{{{step}.{}}}` refers to step {step}, but only {} step(s) have run", path.join("."), outputs.len())
    })?;
    let mut at = output;
    for key in path {
        at = match at {
            Value::Object(map) => map.get(*key),
            Value::Array(items) => key.parse::<usize>().ok().and_then(|i| items.get(i)),
            _ => None,
        }
        .ok_or_else(|| format!("step {step} has no `{}` in its answer", path.join(".")))?;
    }
    match at {
        Value::String(text) => Ok(text.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!("`{}` of step {step} is not text, a number or a boolean", path.join("."))),
    }
}

/// Copies the folder `from` into the new folder `to`, file by file and in name order.
///
/// # Errors
/// The first I/O error.
pub fn copy_dir(from: &Path, to: &Path) -> io::Result<()> {
    fs::create_dir_all(to)?;
    let mut entries = fs::read_dir(from)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn no_items(_: &SetItemRef) -> Result<String, String> {
        Err("no sets here".to_owned())
    }

    #[test]
    fn step_references_are_filled_from_the_outputs_of_earlier_steps() {
        let outputs = vec![json!({"character": "aki", "n": 3, "list": ["x", "y"], "flag": true})];
        let filled = refer(
            &json!({"a": "{0.character}", "b": ["pre-{0.character}-{0.n}"], "c": "{0.list.1}", "d": "{0.flag}", "e": 5}),
            &outputs,
            &no_items,
        )
        .unwrap();
        assert_eq!(filled, json!({"a": "aki", "b": ["pre-aki-3"], "c": "y", "d": "true", "e": 5}));
        // Braces that are not a step reference stay as they were.
        let unchanged = json!("{main} {\"a\": 1} {x.y}");
        assert_eq!(refer(&unchanged, &outputs, &no_items).unwrap(), unchanged);
    }

    #[test]
    fn a_reference_that_does_not_resolve_names_what_is_missing() {
        let outputs = vec![json!({"character": "aki", "obj": {}})];
        for (text, word) in [("{1.id}", "only 1"), ("{0.missing}", "no `missing`"), ("{0.obj}", "not text")] {
            let error = refer(&json!(text), &outputs, &no_items).unwrap_err();
            assert!(error.contains(word), "{text}: {error}");
        }
    }

    #[test]
    fn a_set_item_becomes_the_path_of_its_file_or_the_refusal() {
        let item = json!({"sheet": {"set": "s", "item": "b3:aa"}, "other": {"set": "s", "item": "b3:bb", "extra": 1}});
        let path = |r: &SetItemRef| Ok(format!("/store/{}", r.item));
        let filled = refer(&item, &[], &path).unwrap();
        assert_eq!(filled["sheet"], json!("/store/b3:aa"));
        assert!(filled["other"].is_object(), "an object with more keys than set and item is not a reference");
        assert_eq!(refer(&item["sheet"], &[], &no_items).unwrap_err(), "no sets here");
    }

    #[test]
    fn a_folder_is_copied_whole() {
        let from = tempfile::tempdir().unwrap();
        fs::create_dir_all(from.path().join("a/b")).unwrap();
        fs::write(from.path().join("a/b/f.txt"), "x").unwrap();
        fs::write(from.path().join("g.txt"), "y").unwrap();
        let to = tempfile::tempdir().unwrap();
        copy_dir(from.path(), &to.path().join("copy")).unwrap();
        assert_eq!(fs::read_to_string(to.path().join("copy/a/b/f.txt")).unwrap(), "x");
        assert_eq!(fs::read_to_string(to.path().join("copy/g.txt")).unwrap(), "y");
    }
}
