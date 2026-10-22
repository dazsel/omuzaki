//! The `translations` check (design 10.1 and 11): every translation file parses, defines no orphan id, adds no
//! placeable the English lacks, and keeps the English backticked code spans verbatim.

use std::collections::{BTreeMap, BTreeSet};

use fluent_syntax::ast::{Entry, Pattern, PatternElement};
use fluent_syntax::parser;

use super::{Failure, variables};
use crate::Resource;
use crate::extract::{PackageText, english_of};

/// The check's name in a [`Failure`].
const CHECK: &str = "translations";

/// Stands in for a placeable when a pattern is flattened to text, so a code span never crosses one.
const PLACEABLE: char = '\u{FFFC}';

/// One English slot (a message's value or one attribute): the variables it may use and the code spans it holds.
struct Slot<'s> {
    variables: BTreeSet<&'s str>,
    spans: Vec<String>,
}

/// One English message: its value, if any, and its attributes by name.
struct EnglishEntry<'s> {
    value: Option<Slot<'s>>,
    attributes: BTreeMap<&'s str, Slot<'s>>,
}

/// Design 10.1, "every translation file", over the `resources` of `text.package` whose locale isn't `en`.
pub fn translations(text: &PackageText, resources: &[Resource]) -> Vec<Failure> {
    let english_source = english_of(text);
    // A broken English file is the `messages` check's finding; the parts that did parse still serve as the reference.
    let (Ok(english) | Err((english, _))) = parser::parse(english_source.as_str());
    let reference = reference_of(&english.body);
    let mut failures = Vec::new();
    for resource in resources.iter().filter(|r| r.package == text.package && r.locale != "en") {
        check_resource(resource, &reference, &mut failures);
    }
    failures
}

/// The English entries by id.
fn reference_of<'s>(body: &[Entry<&'s str>]) -> BTreeMap<&'s str, EnglishEntry<'s>> {
    let mut reference = BTreeMap::new();
    for entry in body {
        if let Entry::Message(message) = entry {
            let attributes = message.attributes.iter().map(|a| (a.id.name, slot(&a.value))).collect();
            reference.insert(message.id.name, EnglishEntry { value: message.value.as_ref().map(slot), attributes });
        }
    }
    reference
}

fn slot<'s>(pattern: &Pattern<&'s str>) -> Slot<'s> {
    Slot { variables: variables(pattern), spans: code_spans(&flatten(pattern)) }
}

/// A pattern's text with each placeable replaced by [`PLACEABLE`].
fn flatten(pattern: &Pattern<&str>) -> String {
    pattern
        .elements
        .iter()
        .map(|element| match element {
            PatternElement::TextElement { value } => (*value).to_owned(),
            PatternElement::Placeable { .. } => PLACEABLE.to_string(),
        })
        .collect()
}

/// Every non-empty `` `...` `` span of `text` that holds no placeable, backticks excluded.
fn code_spans(text: &str) -> Vec<String> {
    let pieces: Vec<&str> = text.split('`').collect();
    pieces
        .iter()
        .enumerate()
        // Odd pieces sit between two backticks, except a last one that was never closed.
        .filter(|&(index, _)| index % 2 == 1 && index + 1 < pieces.len())
        .map(|(_, span)| *span)
        .filter(|span| !span.is_empty() && !span.contains(PLACEABLE))
        .map(str::to_owned)
        .collect()
}

/// One translation resource against the English.
fn check_resource(resource: &Resource, reference: &BTreeMap<&str, EnglishEntry<'_>>, failures: &mut Vec<Failure>) {
    let locale = resource.locale;
    let parsed = match parser::parse(resource.source) {
        Ok(parsed) => parsed,
        Err((parsed, errors)) => {
            for error in errors {
                let line = 1 + resource.source.bytes().take(error.pos.start).filter(|&byte| byte == b'\n').count();
                failures.push(Failure::new(CHECK, locale, format!("`{locale}` line {line}: {}", error.kind)));
            }
            parsed
        }
    };
    for entry in &parsed.body {
        let Entry::Message(message) = entry else {
            continue;
        };
        let id = message.id.name;
        let Some(english) = reference.get(id) else {
            failures.push(Failure::new(
                CHECK,
                id,
                format!("`{locale}` defines `{id}`, which the English file does not"),
            ));
            continue;
        };
        if let (Some(value), Some(source)) = (&message.value, &english.value) {
            check_slot(locale, id, value, source, failures);
        }
        for attribute in &message.attributes {
            let name = attribute.id.name;
            match english.attributes.get(name) {
                Some(source) => check_slot(locale, &format!("{id}.{name}"), &attribute.value, source, failures),
                None => failures.push(Failure::new(
                    CHECK,
                    id,
                    format!("`{locale}` defines `.{name}` on `{id}`, which the English file does not"),
                )),
            }
        }
    }
}

/// A translated value or attribute against the English one: no extra placeable, every code span kept.
fn check_slot(locale: &str, id: &str, translated: &Pattern<&str>, source: &Slot<'_>, failures: &mut Vec<Failure>) {
    let extra: Vec<&str> = variables(translated).difference(&source.variables).copied().collect();
    if !extra.is_empty() {
        let names = extra.iter().map(|name| format!("${name}")).collect::<Vec<_>>().join(", ");
        failures.push(Failure::new(CHECK, id, format!("`{locale}` uses {names}, which the English does not")));
    }
    let text = flatten(translated);
    for span in source.spans.iter().filter(|span| !text.contains(span.as_str())) {
        failures.push(Failure::new(
            CHECK,
            id,
            format!("`{locale}` must keep the code span `{span}` as the English has it"),
        ));
    }
}
