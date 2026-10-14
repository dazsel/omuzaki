//! Controls: `TextField`.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::TextField;
use crate::register_story;

/// The sample contents are the person's data, not the gallery's text, but they start as words, so they are written
/// into the fields in the gallery's language the first time the story is drawn.
#[derive(Default)]
struct State {
    name: Option<String>,
    search: String,
    bad: Option<String>,
}

register_story! {
    id: "text_field", title: "Text field", group: Group::Controls, order: 50, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    specimen(ui, ds, "Filled", |ui| {
        let name = s.name.get_or_insert_with(|| translate(ds, "Hair front").into_owned());
        ui.add(TextField::new(tr!(ds, "Part name"), name).width(220.0));
    });
    specimen(ui, ds, "Search", |ui| {
        ui.add(
            TextField::new(tr!(ds, "Search parts"), &mut s.search).search().hint(tr!(ds, "Search parts")).width(220.0),
        );
    });
    specimen(ui, ds, "Error", |ui| {
        ui.vertical(|ui| {
            let bad = s.bad.get_or_insert_with(|| translate(ds, "twintails??").into_owned());
            ui.add(TextField::new(tr!(ds, "Preset name"), bad).error(tr!(ds, "Use letters and numbers")).width(220.0));
        });
    });
    specimen(ui, ds, "Disabled", |ui| {
        let mut locked = translate(ds, "Locked by Designer").into_owned();
        ui.add(
            TextField::new(tr!(ds, "Locked name"), &mut locked)
                .disabled(tr!(ds, "The part is locked by Designer"))
                .width(220.0),
        );
    });
    caption(ui, ds, "Esc clears the field, then leaves it. The clear button shows when there is text.");
}
