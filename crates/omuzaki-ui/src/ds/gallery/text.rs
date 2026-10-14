//! The gallery's own words (story titles, captions, specimen labels, the sample names in the widgets).
//!
//! The components take every string from their caller, so the gallery is the caller: it writes its text in English in
//! the story code and passes it through [`translate`] (or the [`tr!`] shorthand). In English that is the text itself. In the pseudo-locale it becomes the
//! accented, 40% longer version, which is how the gallery proves two things at once: no component hard-codes a word
//! (a plain ASCII word on screen is one that skipped the table), and every layout survives longer text.

use std::borrow::Cow;

use crate::ds::strings::{PSEUDO_LOCALE, pseudo};
use crate::ds::theme::Ds;

/// `text`, as the gallery says it in the language the context speaks.
pub fn translate<'a>(ds: &Ds, text: &'a str) -> Cow<'a, str> {
    if ds.strings.locale() == PSEUDO_LOCALE { Cow::Owned(pseudo(text)) } else { Cow::Borrowed(text) }
}

/// `translate(ds, text)` as a `&str` that lives to the end of the statement, for use as an argument:
/// `ui.add(Button::new(tr!(ds, "Approve")))`.
macro_rules! tr {
    ($ds:expr, $text:expr) => {
        &*$crate::ds::gallery::text::translate($ds, $text)
    };
}
pub(crate) use tr;

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::ds::strings::{English, Pseudo};
    use crate::ds::theme::{Settings, apply_settings, ds, set_strings};

    #[test]
    fn english_is_the_text_itself_and_the_pseudo_locale_is_not() {
        let ctx = egui::Context::default();
        apply_settings(&ctx, Settings::default());
        assert!(matches!(translate(&ds(&ctx), "Approve"), Cow::Borrowed("Approve")));
        set_strings(&ctx, Arc::new(Pseudo(English)));
        let pseudo = translate(&ds(&ctx), "Approve").into_owned();
        assert!(pseudo.starts_with('[') && pseudo.contains('Å'), "{pseudo}");
    }
}
