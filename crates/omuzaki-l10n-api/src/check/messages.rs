//! The declared-messages check (design 10.1): each message of a package has a translator note, an id of the crate's
//! prefix, English that parses to one message, and placeables that are exactly its declared arguments.

use std::collections::{BTreeMap, BTreeSet};

use fluent_syntax::ast::Entry;
use fluent_syntax::parser::parse;

use super::{Failure, variables};
use crate::extract::PackageText;
use crate::registry::MessageDecl;

/// The check's name in a [`Failure`].
const CHECK: &str = "messages";

/// Design 10.1, "declared messages": the rules of every `messages!` message of the package, and that ids are unique.
pub fn messages(text: &PackageText) -> Vec<Failure> {
    let prefixes = prefixes(&text.package);
    let mut failures = Vec::new();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for decl in &text.messages {
        *counts.entry(decl.id).or_default() += 1;
        check_message(decl, &prefixes, &mut failures);
    }
    for (id, count) in counts {
        if count > 1 {
            failures.push(Failure::new(CHECK, id, format!("is declared {count} times; message ids are unique")));
        }
    }
    failures
}

/// The message-id prefixes of a package: its name without `omuzaki-`, then `-` (`omuzaki-char` gives `char-`,
/// `omuzaki-io-ops` gives `io-ops-`). `omuzaki-ui` has two: `ds-`, for the design system's messages, and `ui-`.
pub fn prefixes(package: &str) -> Vec<String> {
    match package {
        "omuzaki-ui" => vec!["ds-".to_owned(), "ui-".to_owned()],
        _ => vec![format!("{}-", package.strip_prefix("omuzaki-").unwrap_or(package))],
    }
}

/// The per-message rules. The English is parsed once, and the placeable rule runs only on a message that parses.
fn check_message(decl: &MessageDecl, prefixes: &[String], failures: &mut Vec<Failure>) {
    if decl.note_text().is_empty() {
        failures.push(Failure::new(CHECK, decl.id, "has no translator note: add a `///` line to the declaration"));
    }
    if let Some(problem) = id_problem(decl.id, prefixes) {
        failures.push(Failure::new(CHECK, decl.id, problem));
    }

    let source = format!("{} = {}", decl.id, decl.english);
    let resource = match parse(source.as_str()) {
        Ok(resource) => resource,
        Err((_, errors)) => {
            let errors: Vec<String> = errors.iter().map(ToString::to_string).collect();
            failures.push(Failure::new(CHECK, decl.id, format!("English does not parse: {}", errors.join("; "))));
            return;
        }
    };
    let [Entry::Message(message)] = resource.body.as_slice() else {
        failures.push(Failure::new(CHECK, decl.id, "English must be one message, with no junk"));
        return;
    };
    let Some(value) = &message.value else {
        failures.push(Failure::new(CHECK, decl.id, "English has no value"));
        return;
    };

    let mut used = variables(value);
    for attribute in &message.attributes {
        used.extend(variables(&attribute.value));
    }
    let declared: BTreeSet<&str> = decl.args.iter().map(|(name, _)| *name).collect();
    let missing: Vec<&str> = declared.difference(&used).copied().collect();
    let extra: Vec<&str> = used.difference(&declared).copied().collect();
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("missing {}", quoted(&missing)));
    }
    if !extra.is_empty() {
        parts.push(format!("extra {}", quoted(&extra)));
    }
    if !parts.is_empty() {
        failures.push(Failure::new(
            CHECK,
            decl.id,
            format!("placeables do not match the arguments: {}", parts.join("; ")),
        ));
    }
}

/// Why a message id is not allowed, or `None` when it is. The reserved prefixes are checked first, because a registered
/// id also fails the package prefix, and one id should give one failure.
fn id_problem(id: &str, prefixes: &[String]) -> Option<String> {
    if id.starts_with("op-") || id.starts_with("kind-") {
        return Some("uses a reserved prefix: `op-` and `kind-` belong to registered text".to_owned());
    }
    if prefixes.iter().any(|prefix| id.starts_with(prefix.as_str())) {
        return None;
    }
    let allowed: Vec<String> = prefixes.iter().map(|prefix| format!("`{prefix}`")).collect();
    Some(format!("must start with {}", allowed.join(" or ")))
}

/// Names in backticks, separated by commas.
fn quoted(names: &[&str]) -> String {
    names.iter().map(|name| format!("`{name}`")).collect::<Vec<_>>().join(", ")
}
