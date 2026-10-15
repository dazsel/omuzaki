//! The edit panels' own words.
//!
//! They are written in English here and passed through [`say`], which gives the text itself in English and the
//! accented, longer version in the pseudo-locale, as the gallery's words are (`ds::gallery::text`): a plain ASCII word
//! on screen in the pseudo-locale is one that skipped this table. They move into the `Strings` table with the change
//! that holds `ds/strings.rs`.

use std::borrow::Cow;

use crate::ds::strings::{PSEUDO_LOCALE, fill, pseudo};
use crate::ds::theme::Ds;

/// The outliner's name, for its tree and its screen-reader label.
pub const OUTLINER: &str = "Outliner";
/// The outliner's filter field.
pub const FILTER: &str = "Filter parts";
/// The outliner with no character open.
pub const NO_CHARACTER: &str = "No character is open";
/// What to do about it.
pub const OPEN_CHARACTER: &str = "Open or create a character to see its parts.";
/// A character with no parts.
pub const NO_PARTS: &str = "This character has no parts yet";
/// What to do about it.
pub const ADD_PART: &str = "Add one from the library or ask the assistant.";
/// A filter that matches nothing.
pub const NO_MATCH: &str = "No part matches \u{201c}{filter}\u{201d}";
/// The context menu's reorder entries (every drag has one, WCAG 2.5.7).
pub const MOVE_UP: &str = "Move up";
/// See [`MOVE_UP`].
pub const MOVE_DOWN: &str = "Move down";
/// Copies a part.
pub const DUPLICATE: &str = "Duplicate";
/// Removes a part (undoable).
pub const DELETE: &str = "Delete";
/// Turns a disabled part back on.
pub const ENABLE: &str = "Enable";
/// Turns a part off without removing it.
pub const DISABLE: &str = "Disable";
/// Frames the part in the viewport.
pub const FRAME: &str = "Frame";
/// An op the panel ran was refused: the runner's message follows.
pub const REFUSED: &str = "That didn't work: {reason}";
/// A part whose stored parameters fail its kind's check.
pub const PART_INVALID: &str = "Its parameters don't pass the kind's check";

/// The inspector's name.
pub const INSPECTOR: &str = "Inspector";
/// The inspector with nothing selected.
pub const SELECT_A_PART: &str = "Select a part to edit it";
/// What to do about it.
pub const SELECT_HOW: &str = "Click a part in the outliner or the viewport.";
/// The inspector's search field.
pub const SEARCH_SETTINGS: &str = "Search settings\u{2026}";
/// More than one part is selected.
pub const MANY_SELECTED: &str = "{count} parts selected";
/// What to do about it.
pub const MANY_HOW: &str = "Select one part to edit its settings.";
/// A search that matches no setting.
pub const NO_SETTING: &str = "No setting matches \u{201c}{query}\u{201d}";
/// The header's kind line.
pub const KIND: &str = "Kind: {kind}";
/// `part.describe` failed: the runner's message follows.
pub const UNDESCRIBED: &str = "This part can't be described: {reason}";

/// The viewport's name.
pub const VIEWPORT: &str = "Viewport";
/// The camera presets (keys 1 to 4).
pub const FRONT: &str = "Front";
/// See [`FRONT`].
pub const THREE_QUARTER: &str = "\u{be}";
/// See [`FRONT`].
pub const SIDE: &str = "Side";
/// See [`FRONT`].
pub const BACK: &str = "Back";
/// Frames the selection (F).
pub const FRAME_SELECTION: &str = "Frame selection";
/// Frames the whole character (Home).
pub const FRAME_CHARACTER: &str = "Frame character";
/// The viewport without a GPU to draw on.
pub const NO_GPU: &str = "The viewport needs a GPU";
/// What to do about it.
pub const NO_GPU_HOW: &str = "Start the app with a Vulkan, Metal or DirectX 12 adapter.";
/// The character doesn't build.
pub const UNBUILT: &str = "The character doesn't build: {reason}";
/// What the viewport's frame readout says.
pub const READOUT: &str = "{ms} ms GPU";

/// `text` in the language the context speaks, with its `{name}` placeholders filled from `args`.
pub fn say<'a>(ds: &Ds, text: &'a str, args: &[(&str, &str)]) -> Cow<'a, str> {
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
    fn english_is_the_text_itself_and_the_pseudo_locale_is_not() {
        let ctx = egui::Context::default();
        apply_settings(&ctx, Settings::default());
        assert!(matches!(say(&ds(&ctx), OUTLINER, &[]), Cow::Borrowed(OUTLINER)));
        assert_eq!(say(&ds(&ctx), MANY_SELECTED, &[("count", "3")]), "3 parts selected");
        set_strings(&ctx, Arc::new(Pseudo(English)));
        let pseudo = say(&ds(&ctx), REFUSED, &[("reason", "no")]).into_owned();
        assert!(!pseudo.is_ascii() && pseudo.contains("no") && !pseudo.contains('{'), "{pseudo}");
    }
}
