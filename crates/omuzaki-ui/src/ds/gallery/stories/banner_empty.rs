//! Feedback: `Banner` and `EmptyState`.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel};
use crate::ds::gallery::text::tr;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::Status;
use crate::ds::widgets::{Banner, EmptyState};
use crate::register_story;

#[derive(Default)]
struct State;

register_story! {
    id: "banner_empty", title: "Banner and empty state", group: Group::Feedback, order: 20, state: State,
    show: |ui, _| show(ui),
}

fn show(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        Banner::new(Status::Warn, tr!(ds, "Tracking lost")).action(tr!(ds, "Reconnect")).show(ui);
        Banner::new(Status::Danger, tr!(ds, "Licence gate: this asset can't be published"))
            .action(tr!(ds, "Replace asset"))
            .show(ui);
        Banner::new(Status::Info, tr!(ds, "Previewing the close-up variant")).show(ui);
    });
    ui.add_space(12.0);
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        EmptyState::new(
            icons::USER,
            tr!(ds, "Start with a character"),
            tr!(ds, "Open the sample character to see a living avatar in seconds."),
        )
        .primary(tr!(ds, "Open the sample character"))
        .secondary(tr!(ds, "Import a VRM"))
        .show(ui);
    });
    caption(ui, ds, "An empty panel is a bug: a headline, one line of why, one primary action.");
}
