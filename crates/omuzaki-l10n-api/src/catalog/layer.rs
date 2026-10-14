//! One Fluent bundle: the parsed text of a single language, from every crate that registered it.

use std::borrow::Cow;

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentError, FluentResource};
use fluent_syntax::ast::Pattern;
use unic_langid::LanguageIdentifier;

use super::numbers::Marks;
use super::report::{MissingReason, Report};
use crate::{Locale, Resource, accent};

/// A language's messages. The bundle is the concurrent kind: it formats from many threads.
pub(super) struct Layer {
    bundle: FluentBundle<FluentResource>,
}

/// What a layer is for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// Text as written.
    Plain,
    /// Text through the pseudo-locale's accent transform.
    Pseudo,
}

impl Layer {
    /// Parses every resource of `tag` (plus the catalog's own list message) into one bundle. Parse errors and
    /// duplicate ids go to the report; the parts that did parse still work.
    pub(super) fn build(
        tag: &Locale,
        role: Role,
        marks: Marks,
        resources: &[&Resource],
        builtin: Option<&str>,
        report: &Report,
    ) -> Self {
        // Pseudo text is English, so its plural rules are English.
        let langid: LanguageIdentifier =
            if role == Role::Pseudo { "en" } else { tag.as_str() }.parse().unwrap_or_default();
        let mut bundle = FluentBundle::new_concurrent(vec![langid]);
        // All six languages read left to right; agents and tests must see clean text (design 12).
        bundle.set_use_isolating(false);
        // Fluent starts with an empty function table; messages may call `NUMBER()` with options. (Our own `NUMBER`, since
        // Fluent's rejects the exact-integer value that integers beyond `f64`'s range arrive as.)
        register_number(&mut bundle, report);
        bundle.set_formatter(Some(marks.formatter()));
        if role == Role::Pseudo {
            bundle.set_transform(Some(accent_text));
        }
        let sources = resources.iter().filter(|r| r.locale == tag.as_str()).map(|r| r.source).chain(builtin);
        for source in sources {
            let resource = FluentResource::try_new(source.to_owned()).unwrap_or_else(|(partial, errors)| {
                for error in errors {
                    report.record("(resource)", MissingReason::Format(error.to_string()));
                }
                partial
            });
            if let Err(errors) = bundle.add_resource(resource) {
                for error in errors {
                    let id = match &error {
                        FluentError::Overriding { id, .. } => id.as_str(),
                        _ => "(resource)",
                    };
                    report.record(id, MissingReason::Format(error.to_string()));
                }
            }
        }
        Self { bundle }
    }

    /// The pattern of `id` (its value, or its attribute `attr`), if this language has it.
    pub(super) fn pattern(&self, id: &str, attr: Option<&str>) -> Option<&Pattern<&str>> {
        let message = self.bundle.get_message(id)?;
        match attr {
            None => message.value(),
            Some(name) => message.get_attribute(name).map(|a| a.value()),
        }
    }

    pub(super) fn bundle(&self) -> &FluentBundle<FluentResource> {
        &self.bundle
    }
}

/// The transform of the pseudo-locale's bundle: accents on the literal text of messages. Placeable values (ids,
/// numbers) are not text elements, so they pass through untouched.
fn accent_text(text: &str) -> Cow<'_, str> {
    if text.chars().any(|c| accent(c) != c) { text.chars().map(accent).collect::<String>().into() } else { text.into() }
}

/// Registers `NUMBER()`; failing here would mean a duplicate registration, which the report shows.
fn register_number(bundle: &mut FluentBundle<FluentResource>, report: &Report) {
    if let Err(error) = bundle.add_function("NUMBER", super::numbers::number_function) {
        report.record("(NUMBER)", MissingReason::Format(error.to_string()));
    }
}
