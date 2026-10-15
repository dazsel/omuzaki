//! The translation-file check (design 10.1): every translation of a package parses, defines only ids the English file
//! defines, uses no placeable the English lacks, and keeps every backticked code span of the English verbatim.
//! Comments are not checked here: staleness is the drafting tool's business (design 9.3).

use std::collections::{BTreeMap, BTreeSet};

use fluent_syntax::ast::{Entry, Message, Pattern, PatternElement};
use fluent_syntax::parser::parse;

use super::{Failure, variables};
use crate::extract::{PackageText, english_of};
use crate::registry::Resource;

/// The check's name in a [`Failure`].
const CHECK: &str = "translations";

/// Design 10.1, "every translation file": each non-English resource of the package, checked against the English file
/// generated from `text`. An English resource is not a translation and is ignored.
pub fn translations(text: &PackageText, resources: &[Resource]) -> Vec<Failure> {
    let english_source = english_of(text);
    // The English file's own problems are the messages and english checks' to report, so a partial parse is used.
    let english = match parse(english_source.as_str()) {
        Ok(resource) | Err((resource, _)) => resource,
    };
    let english_messages: BTreeMap<&str, &Message<&str>> = english
        .body
        .iter()
        .filter_map(|entry| match entry {
            Entry::Message(message) => Some((message.id.name, message)),
            _ => None,
        })
        .collect();

    let mut failures = Vec::new();
    for resource in resources {
        if resource.package == text.package && resource.locale != "en" {
            check_resource(resource, &english_messages, &mut failures);
        }
    }
    failures
}

/// One translation file: it must parse, and each message in it is checked against the English message of its id.
fn check_resource(resource: &Resource, english: &BTreeMap<&str, &Message<&str>>, failures: &mut Vec<Failure>) {
    let file = format!("locales/{}/{}.ftl", resource.locale, resource.package);
    let translation = match parse(resource.source) {
        Ok(parsed) => parsed,
        Err((_, errors)) => {
            // Only the first error: a broken entry usually causes several, and one fault should give one failure.
            if let Some(error) = errors.first() {
                let line = line_of(resource.source, error.pos.start);
                failures.push(Failure::new(
                    CHECK,
                    file,
                    format!("({}) does not parse at line {line}: {error}", resource.locale),
                ));
            }
            return;
        }
    };
    for entry in &translation.body {
        let Entry::Message(message) = entry else {
            continue;
        };
        let id = message.id.name;
        match english.get(id) {
            Some(english_message) => check_message(message, english_message, resource.locale, failures),
            None => failures.push(Failure::new(
                CHECK,
                id,
                format!("({}) is not defined in the English file: an orphan id", resource.locale),
            )),
        }
    }
}

/// A translated message keeps the English's placeables and code spans. Its placeables may be a subset of the English's,
/// and every backticked code span of the English must appear in the translation.
fn check_message(message: &Message<&str>, english: &Message<&str>, locale: &str, failures: &mut Vec<Failure>) {
    let id = message.id.name;
    let mut extra: BTreeSet<&str> = BTreeSet::new();
    if let Some(value) = &message.value {
        let allowed = english.value.as_ref().map(variables).unwrap_or_default();
        extra.extend(variables(value).difference(&allowed));
    }
    for attribute in &message.attributes {
        let allowed = english
            .attributes
            .iter()
            .find(|candidate| candidate.id.name == attribute.id.name)
            .map(|candidate| variables(&candidate.value))
            .unwrap_or_default();
        extra.extend(variables(&attribute.value).difference(&allowed));
    }
    if !extra.is_empty() {
        let names: Vec<String> = extra.iter().map(|name| format!("`{name}`")).collect();
        failures.push(Failure::new(
            CHECK,
            id,
            format!("({locale}) has placeables the English lacks: {}", names.join(", ")),
        ));
    }

    let english_text = message_text(english);
    let translated = message_text(message);
    let missing: Vec<&str> = code_spans(&english_text).into_iter().filter(|span| !translated.contains(span)).collect();
    if !missing.is_empty() {
        let names: Vec<String> = missing.iter().map(|span| format!("`{span}`")).collect();
        failures.push(Failure::new(
            CHECK,
            id,
            format!("({locale}) does not keep the code span {} of the English verbatim", names.join(", ")),
        ));
    }
}

/// The literal text of a message: its value and its attributes, with placeables left out.
fn message_text(message: &Message<&str>) -> String {
    let mut text = String::new();
    if let Some(value) = &message.value {
        text.push_str(&pattern_text(value));
    }
    for attribute in &message.attributes {
        text.push('\n');
        text.push_str(&pattern_text(&attribute.value));
    }
    text
}

/// The literal text of a pattern, placeables left out.
fn pattern_text(pattern: &Pattern<&str>) -> String {
    pattern
        .elements
        .iter()
        .filter_map(|element| match element {
            PatternElement::TextElement { value } => Some(*value),
            PatternElement::Placeable { .. } => None,
        })
        .collect()
}

/// The backticked code spans of a text: the text between each pair of backticks, empty spans left out.
fn code_spans(text: &str) -> Vec<&str> {
    text.split('`').skip(1).step_by(2).filter(|span| !span.is_empty()).collect()
}

/// The 1-based line of a byte offset in `source`.
fn line_of(source: &str, offset: usize) -> usize {
    source.get(..offset).map_or(1, |before| before.matches('\n').count() + 1)
}
