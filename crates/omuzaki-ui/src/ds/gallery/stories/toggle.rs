//! Controls: Toggle (switch and checkbox).

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel, specimen};
use crate::ds::gallery::text::tr;
use crate::ds::theme::ds;
use crate::ds::widgets::{Toggle, TriState};
use omuzaki_ui_api::register_story;

struct State {
    /// Live preview, outlines, reduce motion, shadows.
    switches: [bool; 4],
    parent: TriState,
    children: [bool; 3],
}

impl Default for State {
    fn default() -> Self {
        Self { switches: [true, true, false, false], parent: TriState::Mixed, children: [true, false, true] }
    }
}

register_story! {
    id: "toggle", title: "Toggle", group: Group::Controls, order: 20, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        for (on, label) in s.switches.iter_mut().zip(["Live preview", "Outlines", "Reduce motion", "Shadows"]) {
            ui.add(Toggle::switch(tr!(ds, label), on));
        }
    });
    ui.add_space(8.0);
    specimen(ui, ds, "Checkbox", |ui| {
        if ui.add(Toggle::tri(tr!(ds, "Outfit"), &mut s.parent)).clicked() {
            s.children = [s.parent == TriState::On; 3];
        }
    });
    specimen(ui, ds, "Children", |ui| {
        ui.vertical(|ui| {
            for (i, name) in ["Jacket", "Skirt", "Boots"].into_iter().enumerate() {
                if ui.add(Toggle::checkbox(tr!(ds, name), &mut s.children[i])).clicked() {
                    let on = s.children.iter().filter(|c| **c).count();
                    s.parent = match on {
                        0 => TriState::Off,
                        3 => TriState::On,
                        _ => TriState::Mixed,
                    };
                }
            }
        });
    });
    specimen(ui, ds, "Disabled", |ui| {
        let (mut on, mut off) = (true, false);
        ui.add(Toggle::switch(tr!(ds, "On"), &mut on).disabled(tr!(ds, "Managed by the project")));
        ui.add(Toggle::checkbox(tr!(ds, "Off"), &mut off).disabled(tr!(ds, "Managed by the project")));
    });
    caption(ui, ds, "State is the knob's position and a check mark, not only the colour.");
}
