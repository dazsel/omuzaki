//! Rendering: the fallback chain of design 9.1, and arguments turned into Fluent values.

use std::borrow::Cow;
use std::sync::Arc;

use fluent_bundle::types::FluentNumber;
use fluent_bundle::{FluentArgs, FluentValue};
use fluent_syntax::ast::Pattern;

use super::Inner;
use super::builtin::LIST_ID;
use super::layer::Layer;
use super::numbers::ExactInt;
use super::report::MissingReason;
use crate::{Arg, Text, Unit};

/// Integers beyond this lose digits in the `f64` Fluent numbers are made of, so they travel as an [`ExactInt`].
const EXACT_INT: u64 = 1 << 53;

impl Inner {
    /// The chain for one text. `wrap` is false for a nested text, which the outer text wraps once for the pseudo-locale.
    pub(super) fn render(&self, text: &Text, stored: Option<&str>, wrap: bool) -> String {
        let Some((layer, pattern)) = self.resolve(&text.id, None) else {
            self.report.record(&text.id, MissingReason::UnknownId);
            return stored.map_or_else(|| self.last_resort(text), str::to_owned);
        };
        let mut args = FluentArgs::with_capacity(text.args.len());
        for (name, arg) in &text.args {
            args.set(name.as_ref(), self.value(arg));
        }
        let out = self.format(layer, &text.id, pattern, Some(&args));
        if wrap { self.finish(&text.id, out) } else { out }
    }

    /// Step 4: `⟦id: name=value, …⟧`, with each argument rendered by this catalog (a nested `Text` falls back on its own,
    /// and is reported).
    fn last_resort(&self, text: &Text) -> String {
        let mut out = format!("⟦{}", text.id);
        for (i, (name, arg)) in text.args.iter().enumerate() {
            out.push_str(if i == 0 { ": " } else { ", " });
            out.push_str(name);
            out.push('=');
            out.push_str(&self.text_of(arg));
        }
        out.push('⟧');
        out
    }

    /// Text without arguments, formatted for the cache. `None` when no language has it; that is not an error, since
    /// descriptions are optional.
    pub(super) fn static_text(&self, id: &str, attr: Option<&str>) -> Option<Arc<str>> {
        let (layer, pattern) = self.resolve(id, attr)?;
        let key = attr.map_or_else(|| id.to_owned(), |a| format!("{id}.{a}"));
        let out = self.format(layer, &key, pattern, None);
        Some(self.finish(id, out).into())
    }

    /// Steps 1 and 2 of the chain: the first layer holding a pattern, recording when that is English although the
    /// locale has translations. A regional override's absent entry is found in its parent first, so it is not missing.
    fn resolve(&self, id: &str, attr: Option<&str>) -> Option<(&Layer, &Pattern<&str>)> {
        let key = || attr.map_or_else(|| id.to_owned(), |a| format!("{id}.{a}"));
        if let Some(found) = self.translations.iter().find_map(|l| l.pattern(id, attr).map(|p| (l, p))) {
            return Some(found);
        }
        // The pseudo-locale leaves `#[verbatim]` messages (key legends) as written.
        let english = match &self.plain_english {
            Some(plain) if self.verbatim.contains(id) => plain,
            _ => &self.english,
        };
        let found = english.pattern(id, attr).map(|p| (english, p));
        if found.is_some() && !self.translations.is_empty() {
            self.report.record(&key(), MissingReason::Untranslated);
        }
        found
    }

    fn format(&self, layer: &Layer, key: &str, pattern: &Pattern<&str>, args: Option<&FluentArgs<'_>>) -> String {
        let mut errors = Vec::new();
        let out = layer.bundle().format_pattern(pattern, args, &mut errors).into_owned();
        if let Some(error) = errors.first() {
            // Fluent keeps what it could (`{$name}` stays visible); the report says why.
            self.report.record(key, MissingReason::Format(error.to_string()));
        }
        out
    }

    /// The pseudo-locale's frame: brackets, and a tail of dots that makes the text about 40% longer. Verbatim
    /// messages are not framed.
    fn finish(&self, id: &str, text: String) -> String {
        if self.plain_english.is_none() || self.verbatim.contains(id) {
            return text;
        }
        let letters = text.chars().filter(|c| c.is_alphabetic()).count();
        format!("[{text}{}]", "·".repeat((letters * 2).div_ceil(5)))
    }

    /// An argument as the Fluent value a message sees. Numbers stay numbers, so plural selectors work on them.
    fn value<'a>(&self, arg: &'a Arg) -> FluentValue<'a> {
        match arg {
            Arg::Id(s) | Arg::Verbatim(s) => FluentValue::String(Cow::Borrowed(s)),
            Arg::Int(n) if n.unsigned_abs() <= EXACT_INT => FluentValue::Number(FluentNumber::from(*n)),
            Arg::Int(n) => FluentValue::Custom(Box::new(ExactInt::new(*n, self.marks))),
            Arg::Num(n) => FluentValue::Number(FluentNumber::from(*n)),
            _ => FluentValue::String(Cow::Owned(self.text_of(arg))),
        }
    }

    /// An argument as text, for nesting inside another string (lists) and for the kinds Fluent has no value for.
    fn text_of(&self, arg: &Arg) -> String {
        match arg {
            Arg::Id(s) | Arg::Verbatim(s) => s.clone(),
            Arg::Int(n) => self.marks.localise(&n.to_string(), true),
            Arg::Num(n) => self.marks.localise(&n.to_string(), true),
            // Until the unit formatter lands: the localised number and the unit's symbol.
            Arg::Qty { value, unit } => {
                let number = self.marks.localise(&value.to_string(), true);
                match unit {
                    Unit::Deg => format!("{number}°"),
                    Unit::Percent => format!("{number}%"),
                    other => format!("{number} {}", other.symbol()),
                }
            }
            Arg::Text(text) => self.render(text, None, false),
            Arg::List(items) => self.list(items),
        }
    }

    /// Joins items with the locale's list message: `two` for a pair, else `start`, `middle`s and `end`, folded left
    /// (so "a, b and c" is `end(start(a, b), c)`).
    fn list(&self, items: &[Arg]) -> String {
        let join = |attr: &str, a: &str, b: &str| -> String {
            let Some((layer, pattern)) = self.resolve(LIST_ID, Some(attr)) else {
                return format!("{a}, {b}");
            };
            let mut args = FluentArgs::with_capacity(2);
            args.set("a", a);
            args.set("b", b);
            self.format(layer, LIST_ID, pattern, Some(&args))
        };
        match items {
            [] => String::new(),
            [only] => self.text_of(only),
            [a, b] => join("two", &self.text_of(a), &self.text_of(b)),
            [first, second, middle @ .., last] => {
                let mut joined = join("start", &self.text_of(first), &self.text_of(second));
                for item in middle {
                    joined = join("middle", &joined, &self.text_of(item));
                }
                join("end", &joined, &self.text_of(last))
            }
        }
    }
}
