//! The words the Settings window says (`docs/design/settings.md`, spec SE-03): messages in a table, never literals in
//! the drawing code, as `shell/text.rs` does for the shell. A setting's own words (its summary, the names of its
//! options) are its registration's English and go through [`title`] like a panel's title.

use egui::Context;

use crate::ds::strings::fill;
pub use crate::shell::text::title;

macro_rules! words {
    ($($(#[$meta:meta])* $name:ident = $id:literal => $english:literal),+ $(,)?) => {
        /// A message the Settings window says. The doc line of each variant is its English text.
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub enum Word {
            $($(#[$meta])* #[doc = concat!("`", $english, "`")] $name),+
        }

        impl Word {
            /// Every message, for tests and tools that export the table.
            pub const ALL: &'static [Self] = &[$(Self::$name),+];

            /// The Fluent message id.
            pub const fn id(self) -> &'static str {
                match self { $(Self::$name => $id),+ }
            }

            /// The English pattern, with `{name}` placeholders.
            pub const fn english(self) -> &'static str {
                match self { $(Self::$name => $english),+ }
            }
        }
    };
}

words! {
    Title = "settings-title" => "Settings",
    Search = "settings-search" => "Search settings",
    NoMatch = "settings-no-match" => "No setting matches “{query}”",
    NoMatchLine = "settings-no-match-line" => "Try another word, or clear the search to see every setting.",
    Unavailable = "settings-unavailable" => "The settings can't be read",
    UnavailableLine = "settings-unavailable-line" => "{reason}",
    NotSaved = "settings-not-saved" => "Not saved: {reason}",
    NextStart = "settings-next-start" => "Applies after restart",
    Machine = "settings-machine" => "This computer",
    ChangedBy = "settings-changed-by" => "Last changed by",
    AreaUi = "settings-area-ui" => "Appearance",
    AreaInput = "settings-area-input" => "Input",
    AreaPerform = "settings-area-perform" => "Performing",
    AreaOutput = "settings-area-output" => "Output",
    AreaObs = "settings-area-obs" => "OBS",
    AreaPrivacy = "settings-area-privacy" => "Privacy",
}

/// The text of `word` with `{name}` placeholders filled from `args`, in the language the design system speaks.
pub fn say(ctx: &Context, word: Word, args: &[(&str, &str)]) -> String {
    fill(&title(ctx, word.english()), args)
}

/// The section title of a settings area (`settings-area-<area>`), or the area's name with a capital when this table has
/// none.
pub fn area_title(ctx: &Context, area: &str) -> String {
    let id = format!("settings-area-{area}");
    match Word::ALL.iter().find(|word| word.id() == id) {
        Some(word) => say(ctx, *word, &[]),
        None => title(ctx, &humanise(area)).into_owned(),
    }
}

/// A key segment or an option's value as words: `reduce_motion` and `high-contrast` read "Reduce motion" and
/// "High contrast".
pub fn humanise(id: &str) -> String {
    let spaced: String = id.chars().map(|c| if c == '_' || c == '-' { ' ' } else { c }).collect();
    let mut chars = spaced.chars();
    chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_every_english_text_is_set() {
        let mut seen = std::collections::HashSet::new();
        for word in Word::ALL {
            assert!(seen.insert(word.id()), "{} is used twice", word.id());
            assert!(word.id().starts_with("settings-"), "{}", word.id());
            assert!(!word.english().trim().is_empty(), "{} has no English", word.id());
        }
    }

    #[test]
    fn ids_read_as_words() {
        assert_eq!(humanise("reduce_motion"), "Reduce motion");
        assert_eq!(humanise("high-contrast"), "High contrast");
        assert_eq!(humanise(""), "");
    }
}
