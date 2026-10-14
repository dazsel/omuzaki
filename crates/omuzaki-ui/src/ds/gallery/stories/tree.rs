//! Display: `TreeRow` in an outliner.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{DropLine, Expand, RowHit, RowIcon, TreeRow};
use crate::register_story;

struct State {
    selected: usize,
    hair_open: bool,
    head_open: bool,
}

impl Default for State {
    fn default() -> Self {
        Self { selected: 2, hair_open: false, head_open: true }
    }
}

register_story! {
    id: "tree", title: "Tree row", group: Group::Display, order: 20, state: State,
    show: show,
}

#[allow(clippy::too_many_lines, reason = "a story lists its specimens one after another")]
fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let designer = translate(ds, "Designer");
    let likeness = translate(ds, "likeness 0.81 < 0.85");
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 0.0;
        let row = |ui: &mut Ui, index: usize, row: TreeRow<'_>, state: &mut State| {
            let out = row.selected(state.selected == index, true).show(ui);
            if let Some(hit) = out.hit {
                match (hit, index) {
                    (RowHit::Chevron, 1) => state.head_open = !state.head_open,
                    (RowHit::Chevron, 6) => state.hair_open = !state.hair_open,
                    _ => state.selected = index,
                }
            }
        };
        row(ui, 0, TreeRow::new("character", tr!(ds, "Character"), icons::USER).expand(Expand::Open), s);
        let head = if s.head_open { Expand::Open } else { Expand::Closed };
        row(
            ui,
            1,
            TreeRow::new("head", tr!(ds, "Head"), icons::SMILEY).level(1).expand(head).icons(&[RowIcon::Visible(true)]),
            s,
        );
        if s.head_open {
            row(
                ui,
                2,
                TreeRow::new("face", tr!(ds, "Face profile"), icons::CUBE)
                    .level(2)
                    .icons(&[RowIcon::GateWarning { text: &likeness }, RowIcon::Visible(true)]),
                s,
            );
            row(
                ui,
                3,
                TreeRow::new("eyes", tr!(ds, "Eyes"), icons::CUBE)
                    .level(2)
                    .icons(&[RowIcon::AgentTouched { by: &designer }, RowIcon::Visible(true)]),
                s,
            );
            row(
                ui,
                4,
                TreeRow::new("brows", tr!(ds, "Brows"), icons::CUBE).level(2).icons(&[RowIcon::Visible(false)]),
                s,
            );
            row(
                ui,
                5,
                TreeRow::new("mouth", tr!(ds, "Mouth"), icons::CUBE)
                    .level(2)
                    .icons(&[RowIcon::Locked { owner: &designer }, RowIcon::Visible(true)]),
                s,
            );
        }
        let hair = if s.hair_open { Expand::Open } else { Expand::Closed };
        row(
            ui,
            6,
            TreeRow::new("hair", tr!(ds, "Hair"), icons::HAIR_DRYER)
                .level(1)
                .expand(hair)
                .icons(&[RowIcon::Visible(true)]),
            s,
        );
        if s.hair_open {
            row(
                ui,
                7,
                TreeRow::new("bangs", tr!(ds, "Bangs"), icons::CUBE)
                    .level(2)
                    .matched((0, 3))
                    .icons(&[RowIcon::Visible(true)]),
                s,
            );
            row(
                ui,
                8,
                TreeRow::new("ponytail", tr!(ds, "Ponytail"), icons::CUBE)
                    .level(2)
                    .drop_line(DropLine::Below)
                    .icons(&[RowIcon::Visible(true)]),
                s,
            );
        }
        row(
            ui,
            9,
            TreeRow::new("outfit", tr!(ds, "Outfit"), icons::T_SHIRT)
                .level(1)
                .expand(Expand::Closed)
                .icons(&[RowIcon::Visible(true)]),
            s,
        );
        let out = TreeRow::new("unfocused", tr!(ds, "Selected, panel unfocused"), icons::CUBE)
            .level(1)
            .selected(true, false)
            .show(ui);
        let _ = out;
    });
    caption(
        ui,
        ds,
        "26 pt rows. Selection is a fill and a bar (hollow when the panel is unfocused), never colour alone.",
    );
}
