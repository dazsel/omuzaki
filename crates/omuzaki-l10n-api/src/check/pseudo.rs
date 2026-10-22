//! The `pseudo` check (design 10.1): every message of a package is rendered in the pseudo-locale with sample
//! arguments, and must format, reach the catalog's transform, and grow.

use std::collections::BTreeSet;

use super::Failure;
use crate::extract::PackageText;
use crate::{Arg, ArgKind, Catalog, MessageDecl, MissingReason, Text, Unit};

/// The check's name in a [`Failure`].
const CHECK: &str = "pseudo";

/// Fluent's isolation marks around a placeable's value.
const ISOLATION: [char; 2] = ['\u{2068}', '\u{2069}'];

/// The `Int` values that reach the zero, one, two and many branches of a plural selector.
const INT_SAMPLES: [i64; 3] = [1, 2, 5];

/// Design 10.1, "pseudo-locale run of every message of the crate". `pseudo` is a catalog for
/// [`Locale::PSEUDO`](crate::Locale::PSEUDO); `english` is one for [`Locale::EN`](crate::Locale::EN) over the same
/// resources.
///
/// Each message is rendered once per combination of its `Int` samples, and once otherwise. It fails when
/// - the pseudo catalog reports a formatting error for it (an unknown variable, a syntax error, a missing id);
/// - it renders no text, which the pseudo-locale's frame would otherwise hide;
/// - a plain ASCII letter survives outside the sample `Id` and `Verbatim` values, which is text that skipped the
///   catalog;
/// - the pseudo rendering has no more characters than the English one.
///
/// The last two do not apply to a message marked `verbatim`. Each rule gives one failure per message, from the first
/// combination that breaks it, and a message that failed to format is not judged on the others.
pub fn pseudo(text: &PackageText, pseudo: &Catalog, english: &Catalog) -> Vec<Failure> {
    let nested = text.messages.iter().find(|decl| decl.args.is_empty()).map(|decl| decl.id);
    let mut failures = Vec::new();
    for decl in &text.messages {
        failures.extend(check_message(decl, nested, pseudo, english));
    }
    failures
}

/// The sample values for one declared argument kind. `Int` gives 1, 2 and 5 to reach the plural branches; every
/// other kind gives one value.
pub fn samples(kind: ArgKind) -> Vec<Arg> {
    let one = match kind {
        ArgKind::Id => Arg::Id("sample-id".to_owned()),
        ArgKind::Int => return INT_SAMPLES.iter().map(|&n| Arg::Int(n)).collect(),
        ArgKind::Num => Arg::num_or_verbatim(1.5),
        ArgKind::Qty => Arg::qty_or_verbatim(12.5, Unit::Cm),
        ArgKind::List => Arg::List(vec![Arg::Id("sample-a".to_owned()), Arg::Id("sample-b".to_owned())]),
        ArgKind::Verbatim => Arg::Verbatim("Sample".to_owned()),
        // The caller swaps in a nested `Text` when the package has a message to nest.
        ArgKind::Text => Arg::Id("sample-text".to_owned()),
    };
    vec![one]
}

/// One message: render it for every combination, and collect at most one failure per rule.
fn check_message(decl: &MessageDecl, nested: Option<&str>, pseudo: &Catalog, english: &Catalog) -> Vec<Failure> {
    let per_arg: Vec<(&str, Vec<Arg>)> = decl
        .args
        .iter()
        .map(|&(name, kind)| {
            let values = match (kind, nested) {
                (ArgKind::Text, Some(id)) => vec![Arg::Text(Box::new(Text::new(id.to_owned())))],
                _ => samples(kind),
            };
            (name, values)
        })
        .collect();
    let renders: Vec<(String, String, Vec<String>)> = combinations(&per_arg)
        .into_iter()
        .map(|args| {
            let mut text = Text::new(decl.id.to_owned());
            for (&(name, _), arg) in per_arg.iter().zip(&args) {
                text = text.with(name.to_owned(), arg.clone());
            }
            let mut plain = Vec::new();
            for arg in &args {
                sample_strings(arg, &mut plain);
            }
            (pseudo.render(&text), english.render(&text), plain)
        })
        .collect();

    let broken =
        pseudo.missing().into_iter().filter(|missing| missing.id == decl.id).find_map(|missing| match missing.reason {
            MissingReason::Untranslated => None,
            MissingReason::UnknownId => Some("is not in the resources".to_owned()),
            MissingReason::Format(error) => Some(format!("does not format: {error}")),
        });
    if let Some(problem) = broken {
        return vec![Failure::new(CHECK, decl.id, problem)];
    }

    let mut failures = Vec::new();
    if renders.iter().any(|(_, english, _)| english.trim().is_empty()) {
        failures.push(Failure::new(CHECK, decl.id, "renders no text"));
        return failures;
    }
    if decl.verbatim {
        return failures;
    }
    let mut seen = BTreeSet::new();
    for (pseudo_text, english_text, plain) in &renders {
        if let Some(letter) = plain_ascii(pseudo_text, plain)
            && seen.insert("ascii")
        {
            failures.push(Failure::new(
                CHECK,
                decl.id,
                format!("renders plain ASCII text in the pseudo-locale (`{letter}`): it skipped the catalog"),
            ));
        }
        if pseudo_text.chars().count() <= english_text.chars().count() && seen.insert("growth") {
            failures.push(Failure::new(
                CHECK,
                decl.id,
                "does not grow in the pseudo-locale: the transform never reached it",
            ));
        }
    }
    failures
}

/// Every combination of one value per argument, varying the last argument fastest. An argument with one sample does
/// not multiply the renders; no arguments give one empty combination.
fn combinations(per_arg: &[(&str, Vec<Arg>)]) -> Vec<Vec<Arg>> {
    per_arg.iter().fold(vec![Vec::new()], |rows, (_, values)| {
        rows.iter()
            .flat_map(|row| {
                values.iter().map(move |value| {
                    let mut next = row.clone();
                    next.push(value.clone());
                    next
                })
            })
            .collect()
    })
}

/// The strings an argument shows as it is: `Id` and `Verbatim` values, and the unit symbol of a quantity.
fn sample_strings(arg: &Arg, out: &mut Vec<String>) {
    match arg {
        Arg::Id(s) | Arg::Verbatim(s) => out.push(s.clone()),
        Arg::Qty { unit, .. } => out.push(unit.symbol().to_owned()),
        Arg::List(items) => items.iter().for_each(|item| sample_strings(item, out)),
        Arg::Int(_) | Arg::Num(_) | Arg::Text(_) => {}
    }
}

/// The rendered text with the sample strings and Fluent's isolation marks removed, and the first ASCII letter left,
/// if any.
fn plain_ascii(rendered: &str, plain: &[String]) -> Option<char> {
    let mut rest = rendered.to_owned();
    // Longest first, so a sample inside another (`Sample` in `sample-text`) cannot split it.
    let mut plain: Vec<&String> = plain.iter().collect();
    plain.sort_by_key(|s| std::cmp::Reverse(s.len()));
    for sample in plain {
        rest = rest.replace(sample.as_str(), "");
    }
    rest.retain(|c| !ISOLATION.contains(&c));
    rest.chars().find(char::is_ascii_alphabetic)
}
