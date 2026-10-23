//! The Input panel, registered with `omuzaki-ui-api`: a tab beside Channels and Health in the Perform workspace.

use omuzaki_ui_api::panel::{Panel, PanelCtx};
use omuzaki_ui_api::registry::Region;

use super::InputSurface;
use super::panel::InputPanel;
use crate::ds::icons;

/// The Input panel's id.
pub const INPUT_PANEL: &str = "input";

/// [`InputPanel`] as a registered panel, over the hub's handles when the shell has them.
pub struct InputUiPanel(InputPanel);

impl Panel for InputUiPanel {
    fn ui(&mut self, ui: &mut egui::Ui, ctx: &PanelCtx<'_>) {
        egui::ScrollArea::vertical().id_salt("input.scroll").auto_shrink([false, false]).show(ui, |ui| {
            egui::Frame::new().inner_margin(egui::Margin::symmetric(8, 6)).show(ui, |ui| {
                self.0.ui(ui, ctx.view::<InputSurface>(), ctx.runner(), ctx.actor());
            });
        });
    }
}

omuzaki_ui_api::register_panel! {
    id: "input", title: "Input", icon: icons::KEYBOARD, region: Region::Right, min_size: [280.0, 240.0], order: 40,
    build: |_| Box::new(InputUiPanel(InputPanel::default())),
}
