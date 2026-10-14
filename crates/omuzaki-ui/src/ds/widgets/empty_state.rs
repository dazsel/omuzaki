//! `EmptyState` (`shell.md` section 13): an empty panel is a bug, so every one has a headline, one line of why, one
//! primary action and at most one secondary.

use egui::{Align, Layout, RichText, Sense, Ui, WidgetInfo, WidgetType, vec2};

use super::Button;
use crate::ds::a11y;
use crate::ds::paint;
use crate::ds::theme::ds;
use crate::ds::tokens::{radius, size, space};

/// What an empty state reports.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EmptyOutput {
    /// The primary action was pressed.
    pub primary: bool,
    /// The secondary action was pressed.
    pub secondary: bool,
}

/// A centred empty state.
#[must_use = "an empty state does nothing until it is shown"]
pub struct EmptyState<'a> {
    icon: &'a str,
    headline: &'a str,
    line: &'a str,
    primary: Option<&'a str>,
    secondary: Option<&'a str>,
}

impl<'a> EmptyState<'a> {
    /// An empty state with an icon glyph, a headline (what this is) and one line (why it is empty).
    pub fn new(icon: &'a str, headline: &'a str, line: &'a str) -> Self {
        a11y::require_label(headline);
        Self { icon, headline, line, primary: None, secondary: None }
    }

    /// The one primary action ("Open the sample character").
    pub const fn primary(mut self, label: &'a str) -> Self {
        self.primary = Some(label);
        self
    }

    /// An optional secondary action.
    pub const fn secondary(mut self, label: &'a str) -> Self {
        self.secondary = Some(label);
        self
    }

    /// Shows the state centred in the available width.
    pub fn show(self, ui: &mut Ui) -> EmptyOutput {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let mut out = EmptyOutput::default();
        ui.with_layout(Layout::top_down(Align::Center), |ui| {
            ui.spacing_mut().item_spacing.y = space::S8;
            ui.add_space(space::S16);
            let (rect, response) =
                ui.allocate_exact_size(vec2(size::ICON_LARGE * 2.0, size::ICON_LARGE * 2.0), Sense::hover());
            ui.painter().rect_filled(rect, radius::FULL, t.bg3);
            paint::icon(ui.painter(), &ds, rect.center(), self.icon, size::ICON_LARGE, t.text2, false);
            let headline = self.headline;
            a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, headline), |_| {});
            ui.add_space(space::S4);
            ui.label(RichText::new(self.headline).font(ds.text.title.clone()).color(t.text));
            ui.label(RichText::new(self.line).font(ds.text.body.clone()).color(t.text2));
            if self.primary.is_some() || self.secondary.is_some() {
                ui.add_space(space::S4);
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    let width = self.primary.map_or(0.0, |p| paint::text_width(ui.ctx(), p, &ds.text.strong) + 24.0)
                        + self
                            .secondary
                            .map_or(0.0, |s| paint::text_width(ui.ctx(), s, &ds.text.strong) + 24.0 + space::S8);
                    ui.add_space(((ui.available_width() - width) / 2.0).max(0.0));
                    if let Some(label) = self.primary {
                        out.primary = ui.add(Button::new(label).primary()).clicked();
                    }
                    if let Some(label) = self.secondary {
                        out.secondary = ui.add(Button::new(label).ghost()).clicked();
                    }
                });
            }
            ui.add_space(space::S16);
        });
        out
    }
}
