//! Telling AccessKit what a custom widget is (`design-system.md` section 8.1).
//!
//! egui's own widgets register their role and name; a widget that only paints must say so itself. Every component in
//! this library goes through [`describe`], and its label parameter is a plain `&str` rather than an `Option`, so a
//! widget without an accessible name cannot be written. Tests walk the resulting tree (`gallery::tests`).

use egui::{Response, WidgetInfo, accesskit};

/// Registers `response` with AccessKit: `info` fills the standard fields (role family, label, toggled state, value) and
/// `refine` sets what `egui::WidgetType` cannot express (the exact role, a text value, min/max/step).
///
/// Both closures run only when a screen reader or a test has switched AccessKit on, so a normal frame pays nothing.
pub fn describe(response: &Response, info: impl Fn() -> WidgetInfo, refine: impl FnOnce(&mut accesskit::Node)) {
    response.widget_info(info);
    response.ctx.accesskit_node_builder(response.id, refine);
}

/// Debug check that a label is usable as an accessible name; the parameter types already forbid `None`.
pub fn require_label(label: &str) {
    debug_assert!(!label.trim().is_empty(), "every design-system widget needs a non-empty accessible label");
}

/// The AccessKit class a component puts on stops that are visited in the order of their data rather than of the screen,
/// such as the points of a curve (left to right by input value, though a rising curve climbs the screen). The
/// reading-order check of the gallery exempts consecutive stops that both carry it.
pub const ORDERED_BY_DATA: &str = "ordered-by-data";
