//! Locale numbers: digit grouping and the decimal mark, which `fluent-bundle` leaves to the host (its own `NUMBER`
//! prints the bare `f64`). All six shipped languages write Western digits; they differ in the two marks.

use std::borrow::Cow;

use fluent_bundle::types::{FluentNumber, FluentNumberOptions, FluentType};
use fluent_bundle::{FluentArgs, FluentValue};

use crate::Locale;

/// The decimal and grouping marks of a locale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Marks {
    pub(super) decimal: char,
    pub(super) group: char,
}

impl Marks {
    const DOT: Self = Self { decimal: '.', group: ',' };
    const COMMA: Self = Self { decimal: ',', group: '.' };

    /// `pt` uses the comma; `en`, `ja`, `ko`, `zh` and `es-419` (CLDR) use the dot.
    pub(super) fn of(locale: &Locale) -> Self {
        match locale.as_str().split('-').next() {
            Some("pt") => Self::COMMA,
            _ => Self::DOT,
        }
    }

    /// The formatter a bundle installs so every `Number` it prints follows these marks. A function pointer per mark
    /// set, because the bundle's hook cannot capture.
    pub(super) fn formatter<M>(self) -> fn(&FluentValue<'_>, &M) -> Option<String> {
        if self == Self::COMMA { number::<M, ','> } else { number::<M, '.'> }
    }

    /// `raw` (a plain `f64` or `i64` rendering: `-1234.5`) with grouping and this locale's marks.
    pub(super) fn localise(self, raw: &str, grouping: bool) -> String {
        let (sign, digits) = raw.strip_prefix('-').map_or(("", raw), |rest| ("-", rest));
        let (int, frac) = digits.split_once('.').map_or((digits, None), |(i, f)| (i, Some(f)));
        let mut out = String::with_capacity(raw.len() + raw.len() / 3 + 1);
        out.push_str(sign);
        for (i, c) in int.chars().enumerate() {
            if grouping && i > 0 && (int.len() - i).is_multiple_of(3) {
                out.push(self.group);
            }
            out.push(c);
        }
        if let Some(frac) = frac {
            out.push(self.decimal);
            out.push_str(frac);
        }
        out
    }
}

/// The hook body: numbers only, everything else is left to Fluent.
fn number<M, const DECIMAL: char>(value: &FluentValue<'_>, _: &M) -> Option<String> {
    let FluentValue::Number(n) = value else { return None };
    let marks = if DECIMAL == ',' { Marks::COMMA } else { Marks::DOT };
    Some(marks.localise(&plain(n), n.options.use_grouping))
}

/// An integer that does not fit `f64` exactly (beyond ±2^53), carried as itself so `NUMBER()` options still apply.
/// Fluent's own number is an `f64`, so these travel as a custom value and are formatted from the decimal digits.
/// (Plural selectors do not match it: no count that large needs one, and they take the default variant.)
#[derive(Debug, Clone, PartialEq)]
pub(super) struct ExactInt {
    value: i64,
    marks: Marks,
    options: FluentNumberOptions,
}

impl ExactInt {
    pub(super) fn new(value: i64, marks: Marks) -> Self {
        Self { value, marks, options: FluentNumberOptions::default() }
    }

    /// Digits of the integer with the options that apply to one (`maximumFractionDigits` cannot change an integer).
    fn format(&self) -> String {
        let digits = self.value.unsigned_abs().to_string();
        let width = self.options.minimum_integer_digits.unwrap_or(0);
        let mut raw = String::with_capacity(digits.len().max(width) + 2);
        if self.value < 0 {
            raw.push('-');
        }
        raw.extend(std::iter::repeat_n('0', width.saturating_sub(digits.len())));
        raw.push_str(&digits);
        if let Some(fraction) = self.options.minimum_fraction_digits.filter(|&n| n > 0) {
            raw.push('.');
            raw.extend(std::iter::repeat_n('0', fraction));
        }
        self.marks.localise(&raw, self.options.use_grouping)
    }
}

impl FluentType for ExactInt {
    fn duplicate(&self) -> Box<dyn FluentType + Send> {
        Box::new(self.clone())
    }

    fn as_string(&self, _: &intl_memoizer::IntlLangMemoizer) -> Cow<'static, str> {
        self.format().into()
    }

    fn as_string_threadsafe(&self, _: &intl_memoizer::concurrent::IntlLangMemoizer) -> Cow<'static, str> {
        self.format().into()
    }
}

/// The `NUMBER()` function: Fluent's own (options merged into the number), also for an [`ExactInt`].
pub(super) fn number_function<'a>(positional: &[FluentValue<'a>], named: &FluentArgs<'_>) -> FluentValue<'a> {
    match positional.first() {
        Some(FluentValue::Number(n)) => {
            let mut n = n.clone();
            n.options.merge(named);
            FluentValue::Number(n)
        }
        Some(FluentValue::Custom(custom)) => match (**custom).as_any().downcast_ref::<ExactInt>() {
            Some(exact) => {
                let mut exact = exact.clone();
                exact.options.merge(named);
                FluentValue::Custom(Box::new(exact))
            }
            None => FluentValue::Error,
        },
        _ => FluentValue::Error,
    }
}

/// The digits of `n` before grouping, honouring the options Fluent 0.16 leaves out of `as_string`: rounding to
/// `maximumFractionDigits` (exact ties go to the even digit, as Rust rounds), padding to `minimumFractionDigits` and
/// `minimumIntegerDigits`. (Significant-digit
/// options are not supported; no message uses them.)
fn plain(n: &FluentNumber) -> String {
    let options = &n.options;
    let min_fraction = options.minimum_fraction_digits.unwrap_or(0);
    let mut text = match options.maximum_fraction_digits {
        Some(max) => format!("{:.*}", max.max(min_fraction), n.value),
        None => n.value.to_string(),
    };
    let (mut int, mut frac) = match text.split_once('.') {
        Some((i, f)) => (i.to_owned(), f.to_owned()),
        None => (std::mem::take(&mut text), String::new()),
    };
    // Rounding left zeros the caller did not ask for: `1.50` with a maximum of 2 and no minimum is `1.5`.
    if options.maximum_fraction_digits.is_some() {
        while frac.len() > min_fraction && frac.ends_with('0') {
            frac.pop();
        }
    }
    while frac.len() < min_fraction {
        frac.push('0');
    }
    let negative = int.starts_with('-') && (int.trim_start_matches('-') != "0" || frac.bytes().any(|b| b != b'0'));
    int = int.trim_start_matches('-').to_owned();
    if let Some(width) = options.minimum_integer_digits {
        while int.len() < width {
            int.insert(0, '0');
        }
    }
    let sign = if negative { "-" } else { "" };
    if frac.is_empty() { format!("{sign}{int}") } else { format!("{sign}{int}.{frac}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grouping_and_marks_follow_the_locale() {
        assert_eq!(Marks::DOT.localise("1234567.25", true), "1,234,567.25");
        assert_eq!(Marks::COMMA.localise("1234567.25", true), "1.234.567,25");
        assert_eq!(Marks::DOT.localise("-999", true), "-999");
        assert_eq!(Marks::DOT.localise("-1000", true), "-1,000");
        assert_eq!(Marks::DOT.localise("1234", false), "1234");
        assert_eq!(Marks::of(&Locale::PT_BR), Marks::COMMA);
        assert_eq!(Marks::of(&Locale::ES_419), Marks::DOT);
        assert_eq!(Marks::of(&Locale::JA), Marks::DOT);
    }

    #[test]
    fn number_options_round_and_pad() {
        let with = |value: f64, edit: fn(&mut FluentNumberOptions)| {
            let mut options = FluentNumberOptions::default();
            edit(&mut options);
            plain(&FluentNumber::new(value, options))
        };
        assert_eq!(with(1234.567, |o| o.maximum_fraction_digits = Some(2)), "1234.57");
        assert_eq!(with(1.5, |o| o.maximum_fraction_digits = Some(2)), "1.5");
        assert_eq!(with(1234.0, |o| o.minimum_fraction_digits = Some(0)), "1234");
        assert_eq!(with(1234.0, |o| o.minimum_fraction_digits = Some(2)), "1234.00");
        assert_eq!(with(2.5, |o| o.maximum_fraction_digits = Some(0)), "2");
        assert_eq!(with(-0.001, |o| o.maximum_fraction_digits = Some(2)), "0");
        assert_eq!(with(-1.5, |o| o.maximum_fraction_digits = Some(0)), "-2");
        assert_eq!(with(7.0, |o| o.minimum_integer_digits = Some(3)), "007");
        assert_eq!(with(0.1 + 0.2, |o| o.maximum_fraction_digits = Some(3)), "0.3");
        assert_eq!(with(12.5, |_| {}), "12.5");
    }
}
