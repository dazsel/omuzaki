//! Key caps follow the platform's key labels and the interface language.

use egui::{Key, Modifiers};

use super::super::keycap::{Face, Platform, Shortcut, caps_for};
use crate::ds::icons;
use crate::ds::strings::PSEUDO_LOCALE;

/// The caps as text, with pictures named by their glyph.
fn rendered(shortcut: impl Into<Shortcut>, platform: Platform, locale: &str) -> Vec<String> {
    caps_for(shortcut.into(), platform, locale)
        .iter()
        .map(|face| match face {
            Face::Icon(glyph) => format!("icon:{glyph}"),
            Face::Text(label) => label.to_string(),
            Face::Char(c) => c.to_string(),
        })
        .collect()
}

const ENTER: &str = icons::ENTER;
const BACKSPACE: &str = icons::BACKSPACE;

#[test]
fn macos_uses_apples_symbols_in_apples_order() {
    let all = Shortcut::new(Modifiers { ctrl: true, alt: true, shift: true, mac_cmd: true, command: true }, Key::K);
    assert_eq!(rendered(all, Platform::Mac, "en"), ["⌃", "⌥", "⇧", "⌘", "K"]);
    assert_eq!(rendered(Shortcut::command(Key::Enter), Platform::Mac, "en"), ["⌘".to_owned(), format!("icon:{ENTER}")]);
    assert_eq!(
        rendered(Shortcut::command(Key::Backspace), Platform::Mac, "en"),
        ["⌘".to_owned(), format!("icon:{BACKSPACE}")]
    );
    assert_eq!(rendered(Key::ArrowLeft, Platform::Mac, "en"), ["←"]);
}

#[test]
fn windows_names_the_modifiers_in_microsofts_order() {
    let all = Shortcut::new(Modifiers { ctrl: true, alt: true, shift: true, ..Modifiers::NONE }, Key::K);
    assert_eq!(rendered(all, Platform::Windows, "en"), ["Ctrl", "Alt", "Shift", "K"]);
    // The platform's main modifier is Ctrl here.
    assert_eq!(
        rendered(Shortcut::command(Key::Enter), Platform::Windows, "en"),
        ["Ctrl".to_owned(), format!("icon:{ENTER}")]
    );
    assert_eq!(rendered(Key::Escape, Platform::Windows, "en"), ["Esc"]);
}

#[test]
fn linux_matches_windows_names() {
    for shortcut in [Shortcut::command(Key::Z), Shortcut::new(Modifiers::SHIFT, Key::Tab), Key::F5.into()] {
        assert_eq!(rendered(shortcut, Platform::Linux, "en"), rendered(shortcut, Platform::Windows, "en"));
    }
    assert_eq!(rendered(Shortcut::new(Modifiers::SHIFT, Key::Tab), Platform::Linux, "en"), ["Shift", "Tab"]);
    assert_eq!(rendered(Key::F5, Platform::Linux, "en"), ["F5"]);
}

#[test]
fn the_interface_language_renames_keys_where_keyboards_do() {
    assert_eq!(rendered(Shortcut::command(Key::K), Platform::Windows, "de-DE"), ["Strg", "K"]);
    assert_eq!(rendered(Key::Delete, Platform::Linux, "de"), ["Entf"]);
    assert_eq!(rendered(Key::Enter, Platform::Windows, "es"), ["Intro"]);
    // macOS symbols are the same in every language.
    assert_eq!(rendered(Shortcut::command(Key::K), Platform::Mac, "de"), ["⌘", "K"]);
}

#[test]
fn the_pseudo_locale_accents_every_name() {
    let caps = rendered(Shortcut::new(Modifiers::SHIFT, Key::Tab), Platform::Windows, PSEUDO_LOCALE);
    assert!(caps.iter().all(|c| !c.is_ascii()), "{caps:?}");
}

#[test]
fn the_old_text_form_still_reads() {
    assert_eq!(Shortcut::from("Ctrl Enter"), Shortcut::command(Key::Enter));
    assert_eq!(Shortcut::from("Shift Tab"), Shortcut::new(Modifiers::SHIFT, Key::Tab));
    assert_eq!(Shortcut::from("F"), Shortcut::key(Key::F));
}
