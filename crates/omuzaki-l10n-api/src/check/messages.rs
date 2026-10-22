//! The `messages` check (design 10.1): every declared message has a translator note and the crate's prefix, English
//! that parses, and placeables that are exactly its arguments.

use std::collections::BTreeSet;

use fluent_syntax::ast::Entry;
use fluent_syntax::parser;

use super::{Failure, variables};
use crate::MessageDecl;
use crate::extract::PackageText;

/// The check's name in a [`Failure`].
const CHECK: &str = "messages";

/// The ids derived for ops and part kinds, which a message may not use.
const RESERVED: [&str; 2] = ["op-", "kind-"];

/// Design 10.1, "declared messages": per message, a translator note, a prefix of the package, English that parses to
/// one message with a value, placeables exactly equal to the declared arguments, and an id no other message has.
pub fn messages(text: &PackageText) -> Vec<Failure> {
    let prefixes = prefixes(&text.package);
    let mut failures = Vec::new();
    let mut seen = BTreeSet::new();
    let mut reported = BTreeSet::new();
    for decl in &text.messages {
        if !seen.insert(decl.id) && reported.insert(decl.id) {
            failures.push(Failure::new(CHECK, decl.id, "is declared more than once"));
        }
        check_message(decl, &prefixes, &mut failures);
    }
    failures
}

/// The message-id prefixes of a package: its name without `omuzaki-`, then `-` (`omuzaki-char` gives `char-`,
/// `omuzaki-io-ops` gives `io-ops-`). `omuzaki-ui` has two: `ds-` and `ui-`.
pub fn prefixes(package: &str) -> Vec<String> {
    let name = package.strip_prefix("omuzaki-").unwrap_or(package);
    if package == "omuzaki-ui" {
        // Components keep `ds-`; everything else is `ui-` (design 3.2).
        return vec!["ds-".to_owned(), "ui-".to_owned()];
    }
    vec![format!("{name}-")]
}

/// One message against every per-message rule.
fn check_message(decl: &MessageDecl, prefixes: &[String], failures: &mut Vec<Failure>) {
    if decl.note_text().is_empty() {
        failures.push(Failure::new(CHECK, decl.id, "has no translator note: add a `///` doc comment"));
    }
    if let Some(reserved) = RESERVED.iter().find(|reserved| decl.id.starts_with(**reserved)) {
        failures.push(Failure::new(
            CHECK,
            decl.id,
            format!("starts with `{reserved}`, which is reserved for derived ids"),
        ));
    } else if !prefixes.iter().any(|prefix| decl.id.starts_with(prefix.as_str())) {
        failures.push(Failure::new(CHECK, decl.id, format!("must start with {}", quoted(prefixes))));
    }
    if let Some(problem) = placeable_problem(decl) {
        failures.push(Failure::new(CHECK, decl.id, problem));
    }
}

/// `` `a-` or `b-` ``.
fn quoted(prefixes: &[String]) -> String {
    prefixes.iter().map(|prefix| format!("`{prefix}`")).collect::<Vec<_>>().join(" or ")
}

/// Why the English doesn't parse as one message with a value, or doesn't use exactly the declared arguments.
fn placeable_problem(decl: &MessageDecl) -> Option<String> {
    let source = format!("{} = {}", decl.id, decl.english);
    let resource = match parser::parse(source.as_str()) {
        Ok(resource) => resource,
        Err((_, errors)) => {
            let first = errors.first().map_or_else(String::new, |error| format!(": {}", error.kind));
            return Some(format!("English does not parse{first}"));
        }
    };
    let mut messages = resource.body.iter().filter_map(|entry| match entry {
        Entry::Message(message) => Some(message),
        _ => None,
    });
    let (Some(message), None) = (messages.next(), messages.next()) else {
        return Some("English must be exactly one message".to_owned());
    };
    let Some(value) = &message.value else {
        return Some("English has no value".to_owned());
    };
    let mut used = variables(value);
    for attribute in &message.attributes {
        used.extend(variables(&attribute.value));
    }
    let declared: BTreeSet<&str> = decl.args.iter().map(|&(name, _)| name).collect();
    let missing: Vec<&str> = used.difference(&declared).copied().collect();
    let unused: Vec<&str> = declared.difference(&used).copied().collect();
    if missing.is_empty() && unused.is_empty() {
        return None;
    }
    let mut parts = Vec::new();
    if !missing.is_empty() {
        parts.push(format!("uses undeclared {}", dollars(&missing)));
    }
    if !unused.is_empty() {
        parts.push(format!("never uses declared {}", dollars(&unused)));
    }
    Some(parts.join(" and "))
}

/// `$a, $b`.
fn dollars(names: &[&str]) -> String {
    names.iter().map(|name| format!("${name}")).collect::<Vec<_>>().join(", ")
}
