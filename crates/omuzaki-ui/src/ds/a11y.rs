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
///
/// A widget the keyboard has just reached is scrolled into view: egui moves the focus without scrolling, so Tab could
/// otherwise land on a control below the fold of a panel (WCAG 2.4.11).
pub fn describe(response: &Response, info: impl Fn() -> WidgetInfo, refine: impl FnOnce(&mut accesskit::Node)) {
    response.widget_info(info);
    response.ctx.accesskit_node_builder(response.id, refine);
    if response.gained_focus() && super::focus::visible(&response.ctx) {
        response.scroll_to_me_animation(None, egui::style::ScrollAnimation::none());
    }
}

/// Debug check that a label is usable as an accessible name; the parameter types already forbid `None`.
pub fn require_label(label: &str) {
    debug_assert!(!label.trim().is_empty(), "every design-system widget needs a non-empty accessible label");
}

/// The AccessKit class a component puts on stops that are visited in the order of their data rather than of the screen,
/// such as the points of a curve (left to right by input value, though a rising curve climbs the screen). The
/// reading-order check of the gallery exempts consecutive stops that both carry it.
pub const ORDERED_BY_DATA: &str = "ordered-by-data";

/// Lays out `add_contents` left to right at the trailing end of the row, so the keyboard and a screen reader meet the
/// controls in the order the eye reads them. A right-to-left layout puts them in the same places but registers them
/// backwards, so Tab would walk the row from its right end (WCAG 2.4.3).
///
/// The contents are measured in an invisible pass, at the row's whole width, on the first frame and whenever `key`
/// (what the contents show: their labels, their state), the row's width or the fonts change; that frame is drawn
/// again at once, so nothing is seen out of place. Between measurements the width is kept under `id_salt`. Measuring
/// only at the whole width matters: a width read back from the placed contents would be one a squeezed control was
/// cut to, and would stay cut.
pub fn trailing<R>(
    ui: &mut egui::Ui,
    id_salt: &str,
    key: impl std::hash::Hash,
    add_contents: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    use egui::{Layout, Rect, UiBuilder, pos2};
    let id = ui.id().with(("trailing", id_salt));
    let row = ui.available_rect_before_wrap();
    let ds = crate::ds::theme::ds(ui.ctx());
    // What the contents' natural width depends on besides themselves: the room, the scale and the fonts in use.
    #[allow(clippy::cast_possible_truncation)] // a row's width in whole points
    let room = row.width().round() as i32;
    let fingerprint = {
        use std::hash::{Hash as _, Hasher as _};
        let mut hasher = std::hash::DefaultHasher::new();
        (key, room, ds.text.strong.size.to_bits(), ds.text.fonts_active()).hash(&mut hasher);
        hasher.finish()
    };
    let known: Option<(u64, f32)> = ui.ctx().data(|d| d.get_temp(id));
    let width = known.filter(|(seen, _)| *seen == fingerprint).map(|(_, width)| width);
    let left = width.map_or(row.left(), |width| (row.right() - width).max(row.left()));
    let place = Rect::from_min_max(pos2(left, row.top()), row.right_bottom());
    let mut builder = UiBuilder::new().max_rect(place).layout(Layout::left_to_right(ui.layout().cross_align));
    if width.is_none() {
        builder = builder.sizing_pass().invisible();
    }
    let inner = ui.scope_builder(builder, add_contents);
    if width.is_none() {
        let measured = inner.response.rect.width();
        ui.ctx().data_mut(|d| d.insert_temp(id, (fingerprint, measured)));
        ui.ctx().request_discard("a trailing row was measured");
    }
    inner.inner
}
