//! Key caps: `Ctrl K`, `Enter`, `Esc` drawn as small keys (the prototypes' key-cap hints, `design-system.md` intro).
//!
//! A shortcut is a [`Shortcut`] (modifiers plus a key), not a string; [`KeyCaps`] draws it with the labels of the
//! platform and the language: `⌘ K` on macOS, `Ctrl K` on Windows and Linux, `Strg K` on a German interface.
//! Prior art: egui's `ModifierNames` (MIT OR Apache-2.0) and VS Code's per-platform keybinding labels (MIT).

use std::borrow::Cow;

use egui::os::OperatingSystem;
use egui::{Align2, Color32, Context, FontId, Key, Modifiers, Painter, Pos2, Rect, Stroke, StrokeKind, vec2};

use crate::ds::icons;
use crate::ds::paint;
use crate::ds::strings::PSEUDO_LOCALE;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::space;

const HEIGHT: f32 = 18.0;
const MIN_WIDTH: f32 = 18.0;
const GAP: f32 = 3.0;
const RADIUS: u8 = 3;

/// The colours of a set of key caps, chosen by the surface they sit on.
#[derive(Clone, Copy, Debug)]
pub struct CapStyle {
    /// Cap face.
    pub fill: Color32,
    /// Cap outline.
    pub stroke: Color32,
    /// Label colour.
    pub text: Color32,
}

/// A keyboard shortcut: modifiers and a key. `command` in the modifiers is the platform's main modifier (⌘ on macOS,
/// Ctrl elsewhere), as in egui; `ctrl` is the physical Ctrl key.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortcut {
    /// The modifiers held with the key.
    pub modifiers: Modifiers,
    /// The key.
    pub key: Key,
}

impl Shortcut {
    /// A shortcut of `modifiers` and `key`.
    pub const fn new(modifiers: Modifiers, key: Key) -> Self {
        Self { modifiers, key }
    }

    /// `key` alone.
    pub const fn key(key: Key) -> Self {
        Self::new(Modifiers::NONE, key)
    }

    /// The platform's main modifier with `key`.
    pub const fn command(key: Key) -> Self {
        Self::new(Modifiers::COMMAND, key)
    }
}

impl From<Key> for Shortcut {
    fn from(key: Key) -> Self {
        Self::key(key)
    }
}

impl From<egui::KeyboardShortcut> for Shortcut {
    fn from(shortcut: egui::KeyboardShortcut) -> Self {
        Self::new(shortcut.modifiers, shortcut.logical_key)
    }
}

/// Reads the old text form, space-separated names (`"Ctrl Enter"`, `"Shift Tab"`, `"F"`). `Ctrl` is the platform's main
/// modifier, as a caller of the text form meant it. A name that is no key or modifier is left out (and fails a debug
/// build), so a typo shows as a missing cap and not a wrong one.
impl From<&str> for Shortcut {
    fn from(text: &str) -> Self {
        let (mut modifiers, mut key) = (Modifiers::NONE, None);
        for word in text.split(' ').filter(|w| !w.is_empty()) {
            match word {
                "Ctrl" | "Cmd" => modifiers.command = true, // key name
                "Alt" => modifiers.alt = true,              // key name
                "Shift" => modifiers.shift = true,          // key name
                _ => key = Key::from_name(word),
            }
        }
        debug_assert!(key.is_some(), "no key in the shortcut {text:?}");
        Self::new(modifiers, key.unwrap_or(Key::Enter))
    }
}

/// The operating system families whose key labels differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Platform {
    /// macOS and iOS: symbols for the modifiers.
    Mac,
    /// Windows.
    Windows,
    /// Linux, other Unix, and anything unknown.
    Linux,
}

impl Platform {
    /// The platform the app is running on, as egui detected it.
    pub fn of(ctx: &Context) -> Self {
        match ctx.os() {
            OperatingSystem::Mac | OperatingSystem::IOS => Self::Mac,
            OperatingSystem::Windows => Self::Windows,
            _ => Self::Linux,
        }
    }
}

/// What a cap shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Face {
    Icon(&'static str),
    Text(Cow<'static, str>),
    Char(char),
}

/// Up to this many caps: every modifier and a key.
const MAX_CAPS: usize = 5;

/// The caps of one shortcut, in order, without allocating for the usual English table.
pub(super) struct Caps {
    faces: [Option<Face>; MAX_CAPS],
}

impl Caps {
    pub(super) fn iter(&self) -> impl Iterator<Item = &Face> {
        self.faces.iter().flatten()
    }
}

/// Key names in a language where the platform's keyboards print something else than the English name.
/// `(language, key, label)`. Windows and Linux keyboards print these; macOS uses symbols for modifiers and the same
/// names for the rest.
const LOCALE_LABELS: &[(&str, &str, &str)] = &[
    ("de", "Ctrl", "Strg"),    // key name
    ("de", "Delete", "Entf"),  // key name
    ("de", "Escape", "Esc"),   // key name
    ("es", "Delete", "Supr"),  // key name
    ("es", "Enter", "Intro"),  // key name
    ("fr", "Delete", "Suppr"), // key name
    ("fr", "Escape", "Échap"), // key name
    ("it", "Delete", "Canc"),  // key name
    ("pt", "Delete", "Del"),   // key name
];

fn locale_label(locale: &str, key: &'static str, fallback: &'static str) -> &'static str {
    let language = locale.split(['-', '_']).next().unwrap_or(locale);
    LOCALE_LABELS.iter().find(|(l, k, _)| *l == language && *k == key).map_or(fallback, |(_, _, label)| label)
}

/// The cap labels for `shortcut` on `platform` in `locale` (a BCP 47 tag; [`PSEUDO_LOCALE`] accents everything).
pub(super) fn caps_for(shortcut: Shortcut, platform: Platform, locale: &str) -> Caps {
    let mut faces: [Option<Face>; MAX_CAPS] = Default::default();
    let mut n = 0;
    let mut push = |face: Face| {
        if let Some(slot) = faces.get_mut(n) {
            *slot = Some(face);
            n += 1;
        }
    };
    let m = shortcut.modifiers;
    let text = |label: &'static str| Face::Text(Cow::Borrowed(label));
    let name = |key: &'static str, fallback: &'static str| text(locale_label(locale, key, fallback));
    match platform {
        // Apple's order: Control, Option, Shift, Command.
        Platform::Mac => {
            if m.ctrl {
                push(text("⌃"));
            }
            if m.alt {
                push(text("⌥"));
            }
            if m.shift {
                push(text("⇧"));
            }
            if m.command || m.mac_cmd {
                push(text("⌘"));
            }
        }
        // Microsoft's order: Ctrl, Alt, Shift.
        Platform::Windows | Platform::Linux => {
            if m.ctrl || m.command {
                push(name("Ctrl", "Ctrl"));
            }
            if m.alt {
                push(text("Alt")); // key name
            }
            if m.shift {
                push(text("Shift")); // key name
            }
        }
    }
    push(key_face(shortcut.key, locale));
    // The language only renames keys; the pseudo-locale accents the lot so a hard-coded cap shows.
    if locale == PSEUDO_LOCALE {
        for face in faces.iter_mut().flatten() {
            if let Face::Text(label) = face {
                *label = Cow::Owned(label.chars().map(crate::ds::strings::accent).collect());
            }
        }
    }
    Caps { faces }
}

/// The face of the key itself: a picture, an arrow, the character, or its name.
fn key_face(key: Key, locale: &str) -> Face {
    let name =
        |key: &'static str, fallback: &'static str| Face::Text(Cow::Borrowed(locale_label(locale, key, fallback)));
    match key {
        Key::Enter => {
            let named = locale_label(locale, "Enter", ""); // key name
            if named.is_empty() { Face::Icon(icons::ENTER) } else { Face::Text(Cow::Borrowed(named)) }
        }
        Key::Backspace => Face::Icon(icons::BACKSPACE),
        Key::ArrowUp => Face::Text(Cow::Borrowed("↑")),
        Key::ArrowDown => Face::Text(Cow::Borrowed("↓")),
        Key::ArrowLeft => Face::Text(Cow::Borrowed("←")),
        Key::ArrowRight => Face::Text(Cow::Borrowed("→")),
        Key::Escape => name("Escape", "Esc"),             // key name
        Key::Delete => name("Delete", "Del"),             // key name
        Key::Space => Face::Text(Cow::Borrowed("Space")), // key name
        other => {
            let label = other.name();
            let mut chars = label.chars();
            match (chars.next(), chars.next()) {
                (Some(c), None) => Face::Char(c.to_ascii_uppercase()),
                _ => Face::Text(Cow::Borrowed(label)),
            }
        }
    }
}

/// A shortcut as caps.
#[derive(Clone, Copy, Debug)]
pub struct KeyCaps {
    /// The shortcut.
    pub shortcut: Shortcut,
}

impl KeyCaps {
    /// Caps for a shortcut. A `&str` such as `"Ctrl Enter"` is still accepted, through [`Shortcut`].
    pub fn new(shortcut: impl Into<Shortcut>) -> Self {
        Self { shortcut: shortcut.into() }
    }

    fn font() -> FontId {
        FontId::monospace(11.0)
    }

    fn caps(self, ctx: &Context, ds: &Ds) -> Caps {
        caps_for(self.shortcut, Platform::of(ctx), ds.strings.locale())
    }

    fn cap_width(ctx: &Context, face: &Face) -> f32 {
        let text_cap =
            |label: &str| (paint::text_width(ctx, label, &Self::font()) + 2.0 * space::S4 + 2.0).max(MIN_WIDTH);
        match face {
            Face::Icon(_) => MIN_WIDTH,
            Face::Text(label) => text_cap(label),
            Face::Char(c) => text_cap(c.encode_utf8(&mut [0; 4])),
        }
    }

    /// Total width of all caps with their gaps.
    pub fn width(&self, ctx: &Context) -> f32 {
        let caps = self.caps(ctx, &ds(ctx));
        let (mut width, mut count) = (0.0, 0.0_f32);
        for face in caps.iter() {
            width += Self::cap_width(ctx, face);
            count += 1.0;
        }
        width + GAP * (count - 1.0).max(0.0)
    }

    /// Draws the caps with their left edge at `left_centre` (vertically centred on it); returns the rectangle covered.
    pub fn paint(&self, painter: &Painter, ds: &Ds, left_centre: Pos2, style: CapStyle) -> Rect {
        let start = left_centre.x;
        let mut x = start;
        for face in self.caps(painter.ctx(), ds).iter() {
            let width = Self::cap_width(painter.ctx(), face);
            let cap = Rect::from_min_size(Pos2::new(x, left_centre.y - HEIGHT / 2.0), vec2(width, HEIGHT));
            painter.rect(cap, RADIUS, style.fill, Stroke::new(1.0, style.stroke), StrokeKind::Inside);
            let text = |label: &str| {
                paint::text(painter, cap.center(), Align2::CENTER_CENTER, label, &Self::font(), style.text);
            };
            match face {
                Face::Icon(glyph) => {
                    paint::icon(painter, ds, cap.center(), glyph, 12.0, style.text, false);
                }
                Face::Text(label) => text(label),
                Face::Char(c) => text(c.encode_utf8(&mut [0; 4])),
            }
            x += width + GAP;
        }
        Rect::from_min_max(
            Pos2::new(start, left_centre.y - HEIGHT / 2.0),
            Pos2::new((x - GAP).max(start), left_centre.y + HEIGHT / 2.0),
        )
    }
}

#[cfg(test)]
#[path = "tests/keycap.rs"]
mod tests;
