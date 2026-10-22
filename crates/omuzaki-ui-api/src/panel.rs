//! The [`Panel`] trait and the [`PanelCtx`] the shell hands it each frame.

use std::any::{Any, TypeId};
use std::borrow::Cow;
use std::collections::BTreeMap;

use egui::Ui;
use omuzaki_ops_api::{Actor, Event, Runner};

/// One panel instance, built by its registration's `build` function (see [`register_panel!`](crate::register_panel)).
///
/// The shell owns it and calls [`ui`](Panel::ui) each frame while it is shown (egui may call it more than once in a
/// frame, see below). Every method but `ui` has a
/// default, so a new method here never breaks an existing panel.
pub trait Panel {
    /// Draws the panel's body. Changes to the document go through [`PanelCtx::runner`], as [`PanelCtx::actor`].
    ///
    /// egui may run `ui` more than once in one frame when a pass is discarded (`Context::request_discard`; egui's
    /// default allows a second pass), and each run sees the same [`PanelCtx::events`]. So `ui` must not treat events
    /// as consumed: read them to update state that a second run can redo without harm, and put anything that must
    /// happen once (counting, running an op) behind your own guard.
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>);

    /// A title for the tab that replaces the registered one (an inspector naming its selection), or `None` to keep it.
    fn title(&self) -> Option<Cow<'static, str>> {
        None
    }

    /// What a screen reader announces for the panel, when it says more than the title; `None` uses the title.
    fn accessibility_label(&self) -> Option<Cow<'static, str>> {
        None
    }

    /// Whether the panel can work now. When it can't, the shell draws the registered [`EmptyState`] in its body
    /// instead of calling [`ui`](Panel::ui): panels never close themselves (`shell.md` section 4, rule 5).
    fn available(&self, _ctx: &PanelCtx<'_>) -> bool {
        true
    }

    /// The command-palette entries the panel contributes while it is open, in the order it lists them.
    fn palette_entries(&self) -> &[PaletteEntry] {
        &[]
    }

    /// Runs the palette entry `id`, one of [`palette_entries`](Panel::palette_entries). Returns `false` when the
    /// panel has no entry by that id, so the shell can report it instead of doing nothing.
    fn run_palette_entry(&mut self, _id: &str, _ctx: &PanelCtx<'_>) -> bool {
        false
    }
}

/// A command-palette entry a panel contributes (`shell.md` section 8).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct PaletteEntry {
    /// Stable id, passed back to [`Panel::run_palette_entry`].
    pub id: &'static str,
    /// The English title; the shell translates it through its message table.
    pub title: &'static str,
    /// Extra words the palette's search matches.
    pub keywords: &'static [&'static str],
}

impl PaletteEntry {
    /// An entry with no extra keywords.
    pub const fn new(id: &'static str, title: &'static str) -> Self {
        Self { id, title, keywords: &[] }
    }

    /// The entry with `keywords`.
    #[must_use]
    pub const fn with_keywords(mut self, keywords: &'static [&'static str]) -> Self {
        self.keywords = keywords;
        self
    }
}

/// What a panel shows when it can't work, with the way out (`shell.md` section 4, rule 5).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct EmptyState {
    /// Why the panel can't work ("No character is open").
    pub reason: &'static str,
    /// What to do about it ("Open or create a character").
    pub fix: &'static str,
}

impl EmptyState {
    /// The empty state with `reason` and `fix`, in English; the shell translates both.
    pub const fn new(reason: &'static str, fix: &'static str) -> Self {
        Self { reason, fix }
    }
}

/// Which palette the shell is drawing with.
///
/// Why only this: the design tokens live in `omuzaki-ui`'s design system, an App crate that a panel in another crate
/// can't depend on. So a panel gets the theme in two parts. The colours, type and spacing arrive through the egui
/// `Style` that the shell installs on the context before any panel draws, which every egui widget already reads
/// (`ui.visuals()`). This enum names the palette, for the few panels that pick assets by theme (a thumbnail
/// backdrop). The contract then carries no colour values, which change with every design pass.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum ThemeKind {
    /// The baseline: warm, quiet, dark.
    #[default]
    Dark,
    /// Warm paper tones for bright rooms.
    Light,
    /// Black and white, every status above 7:1.
    HighContrast,
}

/// Typed, read-only state the shell shares with panels: the selection, the open character's summary, and so on.
///
/// Each type is stored once and looked up by type, so a new view model is a new type, never a new field here.
#[derive(Default)]
pub struct ViewModels(BTreeMap<TypeId, Box<dyn Any>>);

impl ViewModels {
    /// An empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `value`; returns whether it replaced a value of the same type.
    pub fn insert<T: Any>(&mut self, value: T) -> bool {
        self.0.insert(TypeId::of::<T>(), Box::new(value)).is_some()
    }

    /// The stored value of type `T`, if any.
    pub fn get<T: Any>(&self) -> Option<&T> {
        self.0.get(&TypeId::of::<T>())?.downcast_ref::<T>()
    }

    /// The stored value of type `T` to change in place, if any (the shell's side; panels only read).
    pub fn get_mut<T: Any>(&mut self) -> Option<&mut T> {
        self.0.get_mut(&TypeId::of::<T>())?.downcast_mut::<T>()
    }
}

/// What a panel may read and run during a frame. Built by the shell (or a test host) with [`PanelCtx::new`] and the
/// `with_*` methods; panels only read it.
pub struct PanelCtx<'a> {
    runner: &'a dyn Runner,
    actor: &'a Actor,
    events: &'a [Event],
    views: &'a ViewModels,
    theme: ThemeKind,
}

impl<'a> PanelCtx<'a> {
    /// A context that runs ops through `runner` as `actor`, with no events, no view models and the dark theme.
    pub fn new(runner: &'a dyn Runner, actor: &'a Actor) -> Self {
        const NO_VIEWS: &ViewModels = &ViewModels(BTreeMap::new());
        Self { runner, actor, events: &[], views: NO_VIEWS, theme: ThemeKind::Dark }
    }

    /// The context with this frame's `events`.
    #[must_use]
    pub fn with_events(mut self, events: &'a [Event]) -> Self {
        self.events = events;
        self
    }

    /// The context with `views`.
    #[must_use]
    pub fn with_views(mut self, views: &'a ViewModels) -> Self {
        self.views = views;
        self
    }

    /// The context drawing with `theme`.
    #[must_use]
    pub fn with_theme(mut self, theme: ThemeKind) -> Self {
        self.theme = theme;
        self
    }

    /// The runner: every change to the document is an op run through it.
    pub fn runner(&self) -> &'a dyn Runner {
        self.runner
    }

    /// The person the panel acts for; ops it runs carry this actor.
    pub fn actor(&self) -> &'a Actor {
        self.actor
    }

    /// The events the runner sent since the previous frame, oldest first. Every panel sees the same slice, and a
    /// panel whose [`ui`](Panel::ui) egui runs twice in a frame (a discarded pass) sees it twice: events are not
    /// consumed by being read.
    pub fn events(&self) -> &'a [Event] {
        self.events
    }

    /// The view model of type `T`, if the shell provides one.
    pub fn view<T: Any>(&self) -> Option<&'a T> {
        self.views.get::<T>()
    }

    /// The theme the shell is drawing with.
    pub fn theme(&self) -> ThemeKind {
        self.theme
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn view_models_are_found_by_type_and_replaced_whole() {
        let mut views = ViewModels::new();
        assert!(!views.insert(3_u32));
        assert!(!views.insert("selection"));
        assert!(views.insert(4_u32));
        assert_eq!(views.get::<u32>(), Some(&4));
        assert_eq!(views.get::<&str>(), Some(&"selection"));
        assert_eq!(views.get::<i64>(), None);
        if let Some(n) = views.get_mut::<u32>() {
            *n += 1;
        }
        assert_eq!(views.get::<u32>(), Some(&5));
    }

    #[test]
    fn a_palette_entry_carries_its_keywords() {
        let entry = PaletteEntry::new("frame", "Frame selection").with_keywords(&["zoom", "focus"]);
        assert_eq!((entry.id, entry.title, entry.keywords), ("frame", "Frame selection", &["zoom", "focus"][..]));
    }
}
