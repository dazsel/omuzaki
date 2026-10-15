//! The `registered` check (design 10.1): every op and part kind of a package has extractable, well-formed text.

use std::collections::BTreeMap;

use schemars::Schema;

use super::Failure;
use crate::extract::PackageText;
use crate::ids;
use crate::registry::RegisteredText;
use crate::schema_text::{self, walk};

/// The check's name in a [`Failure`].
const CHECK: &str = "registered";

/// Design 10.1, "extractable text for every op and kind of the package". It checks each owner's text and argument
/// schema, and that every id the package writes is written once.
pub fn registered(text: &PackageText) -> Vec<Failure> {
    let mut failures = Vec::new();
    // Every id written by the package, with where each write came from, to find an id written twice.
    let mut sources: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for item in &text.registered {
        if !item.owner.starts_with("op-") && !item.owner.starts_with("kind-") {
            // A later registry needs its own rules here before its owners are accepted.
            failures.push(Failure::new(
                CHECK,
                &item.owner,
                "only `op-` and `kind-` owners are checked; a later registry needs its own rules first",
            ));
            continue;
        }
        sources.entry(item.owner.clone()).or_default().push(format!("the owner `{}`", item.owner));
        if item.owner.starts_with("op-") {
            check_op(item, &mut failures);
        } else {
            check_kind(item, &mut failures);
        }
        if let Some(schema) = &item.schema {
            check_schema(item, schema, &mut failures, &mut sources);
        }
    }
    for (id, mut written) in sources {
        if written.len() > 1 {
            written.sort();
            failures.push(Failure::new(CHECK, id, format!("is written by {}", written.join(" and "))));
        }
    }
    failures
}

/// An op's name must follow the op-name grammar, and its summary must be one non-empty line.
fn check_op(item: &RegisteredText, failures: &mut Vec<Failure>) {
    // `op-face-set_profile` is the op `face.set_profile`: the dashes of the id are the dots of the name.
    let name = item.owner.strip_prefix("op-").unwrap_or(&item.owner).replace('-', ".");
    if let Err(error) = ids::check_op_name(&name) {
        failures.push(Failure::new(CHECK, &item.owner, error.to_string()));
    }
    if let Some(problem) = line_problem(&item.value) {
        failures.push(Failure::new(CHECK, &item.owner, format!("the summary {problem}")));
    }
}

/// A part kind needs a non-empty title and a one-line `summary` attribute.
fn check_kind(item: &RegisteredText, failures: &mut Vec<Failure>) {
    if item.value.trim().is_empty() {
        failures.push(Failure::new(CHECK, &item.owner, "the title is empty"));
    }
    match item.attributes.iter().find(|(name, _)| name.as_str() == "summary") {
        None => failures.push(Failure::new(CHECK, &item.owner, "has no `summary` attribute")),
        Some((_, summary)) => {
            if let Some(problem) = line_problem(summary) {
                failures.push(Failure::new(CHECK, &item.owner, format!("the summary {problem}")));
            }
        }
    }
}

/// Walks an owner's argument schema: every node below the root needs a description, and its key must be a legal
/// id part. Each node also records the id it writes, so a repeated id is found.
fn check_schema(
    item: &RegisteredText,
    schema: &Schema,
    failures: &mut Vec<Failure>,
    sources: &mut BTreeMap<String, Vec<String>>,
) {
    let mut node_number = 0;
    walk(schema, |path, node| {
        node_number += 1;
        // Each visited key is the last one of its path, so checking the last key checks each key once.
        let Some(&key) = path.last() else {
            return;
        };
        let id = ids::arg(&item.owner, path);
        if !valid_key(key) {
            failures.push(Failure::new(
                CHECK,
                &id,
                format!("key `{key}` must be letters, digits, `_` or a single `-`"),
            ));
        }
        if schema_text::description(node).is_none() {
            failures.push(Failure::new(CHECK, &id, "has no description: add a doc comment to the field or variant"));
        }
        sources.entry(id).or_default().push(format!("the schema of `{}`, node {node_number}", item.owner));
    });
}

/// Why `text` is not a one-line, non-empty summary, or `None` when it is one.
fn line_problem(text: &str) -> Option<&'static str> {
    let text = text.trim();
    if text.is_empty() {
        Some("is empty")
    } else if text.contains('\n') {
        Some("spans more than one line")
    } else {
        None
    }
}

/// A key is letters, digits, `_` or a single `-`: no `--`, and no `-` at either end (`ids::arg` joins keys with `--`).
fn valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with('-')
        && !key.ends_with('-')
        && !key.contains("--")
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}
