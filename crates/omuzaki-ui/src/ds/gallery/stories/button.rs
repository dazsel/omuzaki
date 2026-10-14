//! Controls: Button.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::tr;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::ds;
use crate::ds::tokens::radius;
use crate::ds::widgets::Button;
use crate::register_story;

#[derive(Default)]
struct State {
    clicks: u32,
}

register_story! {
    id: "button", title: "Button", group: Group::Controls, order: 10, state: State,
    show: show,
}

fn show(ui: &mut Ui, state: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    specimen(ui, ds, "Kinds", |ui| {
        for button in [
            Button::new(tr!(ds, "Approve")).primary(),
            Button::new(tr!(ds, "Cancel")),
            Button::new(tr!(ds, "Details")).ghost(),
            Button::new(tr!(ds, "Delete 3 parts")).danger(),
        ] {
            if ui.add(button).clicked() {
                state.clicks += 1;
            }
        }
    });
    specimen(ui, ds, "Icon", |ui| {
        ui.add(Button::new(tr!(ds, "Add part")).icon(icons::PLUS).primary());
        ui.add(Button::new(tr!(ds, "Reset")).icon(icons::ARROW_COUNTER_CLOCKWISE));
        ui.add(Button::new(tr!(ds, "Open")).icon(icons::FOLDER).ghost());
    });
    specimen(ui, ds, "Shortcut", |ui| {
        ui.add(Button::new(tr!(ds, "Approve")).primary().shortcut("Enter"));
        ui.add(Button::new(tr!(ds, "Approve for session")).shortcut("Ctrl Enter"));
        ui.add(Button::new(tr!(ds, "Reject…")).shortcut("Backspace"));
    });
    specimen(ui, ds, "Hovered", |ui| {
        ui.add(Button::new(tr!(ds, "Approve")).primary()).highlight();
        ui.add(Button::new(tr!(ds, "Cancel"))).highlight();
        ui.add(Button::new(tr!(ds, "Details")).ghost()).highlight();
        ui.add(Button::new(tr!(ds, "Delete")).danger()).highlight();
    });
    specimen(ui, ds, "Focused", |ui| {
        for button in [Button::new(tr!(ds, "Approve")).primary(), Button::new(tr!(ds, "Cancel"))] {
            let response = ui.add(button);
            paint::draw_ring(ui.painter(), &ds.theme, response.rect, radius::CONTROL, RingPlacement::Outside);
        }
        ui.add_space(6.0);
    });
    specimen(ui, ds, "Disabled", |ui| {
        ui.add(Button::new(tr!(ds, "Approve")).primary().disabled(tr!(ds, "Select a part first")));
        ui.add(Button::new(tr!(ds, "Undo")).disabled(tr!(ds, "Nothing to undo")));
        ui.add(Button::new(tr!(ds, "Delete")).danger().disabled(tr!(ds, "The part is locked by Designer")));
    });
    caption(ui, ds, "A disabled button keeps its place and says why on hover and keyboard focus.");
}
