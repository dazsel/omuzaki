//! Small layout helpers the stories share: a labelled specimen row, a caption and an inspector-width frame.

use egui::{Align, Frame, Layout, Margin, RichText, Stroke, Ui, vec2};

use super::text::translate;
use crate::ds::theme::Ds;
use crate::ds::tokens::radius;

/// Width of the label column of a specimen row.
const LABEL_WIDTH: f32 = 92.0;

/// One specimen row: a quiet label on the left, the specimens on the right (wrapping when they do not fit).
pub fn specimen(ui: &mut Ui, ds: &Ds, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            vec2(LABEL_WIDTH, ds.metrics.control_row),
            Layout::left_to_right(Align::Center),
            |ui| {
                // Without a minimum the cell shrinks to its text and the specimens start at ragged x positions.
                ui.set_min_size(vec2(LABEL_WIDTH, ds.metrics.control_row));
                ui.label(RichText::new(translate(ds, label)).font(ds.text.small.clone()).color(ds.theme.text3));
            },
        );
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing = vec2(8.0, 8.0);
            add(ui);
        });
    });
}

/// A line of explanation under a specimen.
pub fn caption(ui: &mut Ui, ds: &Ds, text: &str) {
    ui.label(RichText::new(translate(ds, text)).font(ds.text.small.clone()).color(ds.theme.text3));
}

/// A panel-like frame (`bg1`, 8 pt padding) so a specimen is seen on the surface it is built for.
pub fn panel(ds: &Ds) -> Frame {
    Frame::new()
        .fill(ds.theme.bg1)
        .stroke(Stroke::new(1.0, ds.theme.line))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(ds.metrics.panel_padding as i8))
}

/// A raised frame (`bg2`): the surface cards, toasts and inputs sit on.
pub fn raised(ds: &Ds) -> Frame {
    panel(ds).fill(ds.theme.bg2)
}
