//! Feedback: `Dialog`, drawn in place so a still image can show it, and opened over the gallery on demand.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{Button, Dialog, DialogAction, FirstFocus, TextField};
use omuzaki_ui_api::register_story;

#[derive(Default)]
struct State {
    naming: bool,
    deleting: bool,
    opener: Option<egui::Id>,
    name: String,
    inline_name: Option<String>,
    last: Option<DialogAction>,
}

register_story! {
    id: "dialog", title: "Dialog", group: Group::Feedback, order: 40, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    // Whole-card width, not a specimen row: a dialog is a block, and its width follows the room it has.
    caption(ui, ds, "Data entry");
    let name = s.inline_name.get_or_insert_with(|| translate(ds, "Before the oval face").into_owned());
    Dialog::new("preview_name", tr!(ds, "Save a named version"), tr!(ds, "Save version"))
        .first_focus(FirstFocus::Body)
        .show_inline(ui, |ui| {
            ui.label(translate(ds, "Named versions survive history trimming."));
            ui.add(TextField::new(tr!(ds, "Version name"), name));
        });
    ui.add_space(8.0);
    caption(ui, ds, "Dangerous");
    Dialog::new("preview_delete", tr!(ds, "Delete 3 parts?"), tr!(ds, "Delete 3 parts"))
        .dangerous(tr!(ds, "Confirm: delete 3 parts"))
        .show_inline(ui, |ui| {
            ui.label(translate(ds, "Hair front, hair back and bangs go, with their weights. You can't undo this."));
        });
    ui.add_space(8.0);
    specimen(ui, ds, "Over the page", |ui| {
        let name = ui.add(Button::new(tr!(ds, "Name a version…")));
        if name.clicked() {
            s.naming = true;
            s.opener = Some(name.id);
        }
        let delete = ui.add(Button::new(tr!(ds, "Delete parts…")).danger());
        if delete.clicked() {
            s.deleting = true;
            s.opener = Some(delete.id);
        }
    });
    let ctx = ui.ctx().clone();
    if s.naming {
        let name = &mut s.name;
        let (title, verb) = (translate(ds, "Save a named version"), translate(ds, "Save version"));
        let mut dialog = Dialog::new("naming", &title, &verb);
        if let Some(opener) = s.opener {
            dialog = dialog.opened_from(opener);
        }
        let action = dialog
            .primary_disabled_if(name.trim().is_empty(), tr!(ds, "Enter a name first"))
            .first_focus(FirstFocus::Body)
            .show(&ctx, |ui| {
                let field = ui.add(TextField::new(tr!(ds, "Version name"), name));
                if !field.has_focus() && name.is_empty() {
                    field.request_focus();
                }
            });
        if action.is_some() {
            s.naming = false;
            s.last = action;
        }
    }
    if s.deleting {
        let (title, verb) = (translate(ds, "Delete 3 parts?"), translate(ds, "Delete 3 parts"));
        let mut dialog = Dialog::new("deleting", &title, &verb);
        if let Some(opener) = s.opener {
            dialog = dialog.opened_from(opener);
        }
        let action = dialog.dangerous(tr!(ds, "Confirm: delete 3 parts")).show(&ctx, |ui| {
            ui.label(translate(ds, "Hair front, hair back and bangs go, with their weights. You can't undo this."));
        });
        if action.is_some() {
            s.deleting = false;
            s.last = action;
        }
    }
    let text = match s.last {
        None => {
            "Esc closes a dialog and focus goes back to the button that opened it. A dangerous one needs two presses."
        }
        Some(DialogAction::Primary) => "Chosen: the primary action.",
        Some(DialogAction::Cancel) => "Chosen: cancel.",
    };
    caption(ui, ds, text);
}
