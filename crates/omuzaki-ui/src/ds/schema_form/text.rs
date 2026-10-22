//! The words `schema_form` adds in its second part, beside the ones it takes from the [`Strings`](crate::ds::Strings)
//! table (`ds-form-*`, `ds-param-reset*`).
//!
//! They are written in English here and passed through [`say`], which gives the text itself in English and the
//! accented, longer version in the pseudo-locale, as the gallery's words are (`ds::gallery::text`). They move into the
//! table with the next change that holds `ds/strings.rs`.

use std::borrow::Cow;

use crate::ds::strings::{PSEUDO_LOCALE, fill, pseudo};
use crate::ds::theme::Ds;

/// An integer field's text is not a whole number.
pub(super) const WHOLE: &str = "Enter a whole number";
/// An integer field's text is a whole number below its lower bound.
pub(super) const AT_LEAST: &str = "Enter a whole number of at least {min}";
/// An integer field's text is a whole number above its upper bound.
pub(super) const AT_MOST: &str = "Enter a whole number of at most {max}";

/// A curve field's tangent can't be edited: the schema's array carries points only, so a tangent would be lost.
pub(super) const NO_TANGENTS: &str = "Tangents are not saved in this field";

/// `text` in the language the context speaks, with its `{name}` placeholders filled from `args`.
pub(super) fn say<'a>(ds: &Ds, text: &'a str, args: &[(&str, &str)]) -> Cow<'a, str> {
    let text = if ds.strings.locale() == PSEUDO_LOCALE { Cow::Owned(pseudo(text)) } else { Cow::Borrowed(text) };
    if args.is_empty() { text } else { Cow::Owned(fill(&text, args)) }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ds::strings::{English, Pseudo};
    use crate::ds::theme::{Settings, apply_settings, ds, set_strings};

    #[test]
    fn english_is_the_text_itself_filled_and_the_pseudo_locale_is_not() {
        let ctx = egui::Context::default();
        apply_settings(&ctx, Settings::default());
        assert!(matches!(say(&ds(&ctx), WHOLE, &[]), Cow::Borrowed(WHOLE)));
        assert_eq!(say(&ds(&ctx), AT_LEAST, &[("min", "3")]), "Enter a whole number of at least 3");
        set_strings(&ctx, Arc::new(Pseudo(English)));
        let pseudo = say(&ds(&ctx), AT_MOST, &[("max", "9")]).into_owned();
        assert!(pseudo.contains('9') && !pseudo.contains("{max}") && !pseudo.is_ascii(), "{pseudo}");
    }
}
