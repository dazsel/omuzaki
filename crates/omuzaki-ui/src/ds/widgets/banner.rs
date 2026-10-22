//! Banner (`design-system.md` section 7.0): an inline note at the top of a panel for a persistent condition, with one
//! action ("Tracking lost: Reconnect"). It is tinted by status and always has an icon and words.

use egui::{Align, Frame, Layout, Margin, RichText, Stroke, Ui, WidgetInfo, WidgetType, accesskit};

use super::{Button, ButtonKind};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, mix, radius, space};

/// What a banner reports.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct BannerOutput {
    /// The action button was pressed.
    pub action: bool,
}

/// An inline status banner.
#[must_use = "a banner does nothing until it is shown"]
pub struct Banner<'a> {
    status: Status,
    text: &'a str,
    action: Option<&'a str>,
}

impl<'a> Banner<'a> {
    /// A banner saying `text` with `status`'s icon and colour.
    pub fn new(status: Status, text: &'a str) -> Self {
        a11y::require_label(text);
        Self { status, text, action: None }
    }

    /// The one action ("Reconnect").
    pub const fn action(mut self, label: &'a str) -> Self {
        self.action = Some(label);
        self
    }

    /// Shows the banner at full width.
    pub fn show(self, ui: &mut Ui) -> BannerOutput {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let colour = t.status(self.status);
        let mut out = BannerOutput::default();
        let response = Frame::new()
            .fill(t.status_tint(self.status, t.bg1))
            .stroke(Stroke::new(1.0, mix(colour, t.bg1, 45)))
            .corner_radius(radius::CARD)
            .inner_margin(Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    ui.label(RichText::new(icons::status(self.status)).font(ds.text.icon(16.0, true)).color(colour));
                    // A long note wraps in the room left beside the icon, never widening the panel.
                    ui.add(egui::Label::new(RichText::new(self.text).font(ds.text.body.clone()).color(t.text)).wrap());
                    if let Some(action) = self.action {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            out.action = ui.add(Button::new(action).kind(ButtonKind::Secondary)).clicked();
                        });
                    }
                });
            })
            .response;
        let (text, word) = (self.text, ds.status_word(self.status).into_owned());
        let response = ui.interact(response.rect, response.id.with("banner"), egui::Sense::hover());
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::Label, true, text),
            |node| {
                node.set_role(accesskit::Role::Status);
                node.set_description(word);
            },
        );
        out
    }
}
