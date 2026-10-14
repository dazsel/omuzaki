//! Controls: `FormRow`, a label in the 38% column and any control after it.

use egui::Ui;
use omuzaki_core::colour::Colour;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{Choice, ColourField, Dropdown, FormRow, ParamSlider, Segment, Segmented, TextField, Toggle};
use crate::register_story;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Centre,
    Right,
}

struct State {
    name: Option<String>,
    other: String,
    side: Side,
    shape: usize,
    skin: Colour,
    smooth: bool,
    width: f64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            name: None,
            other: String::new(),
            side: Side::Centre,
            shape: 1,
            skin: Colour::rgb(0xF2, 0xC9, 0xB0),
            smooth: true,
            width: 0.62,
        }
    }
}

register_story! {
    id: "form_row", title: "Form row", group: Group::Controls, order: 80, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        let name = s.name.get_or_insert_with(|| translate(ds, "Hair front").into_owned());
        FormRow::new(tr!(ds, "Part name")).show(ui, |ui, label| ui.add(TextField::new(label, name)));
        let (left, centre, right) = (translate(ds, "Left"), translate(ds, "Centre"), translate(ds, "Right"));
        FormRow::new(tr!(ds, "Fringe side")).modified(s.side != Side::Centre).show(ui, |ui, label| {
            ui.add(
                Segmented::new(
                    label,
                    &mut s.side,
                    &[
                        Segment::new(Side::Left, &left),
                        Segment::new(Side::Centre, &centre),
                        Segment::new(Side::Right, &right),
                    ],
                )
                .stretch(),
            )
        });
        let (round, almond, sharp) = (translate(ds, "Round"), translate(ds, "Almond"), translate(ds, "Sharp"));
        FormRow::new(tr!(ds, "Eye shape")).show(ui, |ui, label| {
            ui.add(
                Dropdown::new(
                    label,
                    &mut s.shape,
                    &[Choice::new(0, &round), Choice::new(1, &almond), Choice::new(2, &sharp)],
                )
                .stretch(),
            )
        });
        FormRow::new(tr!(ds, "Skin tone")).show(ui, |ui, label| ColourField::new(label, &mut s.skin).show(ui));
        FormRow::new(tr!(ds, "Smooth shading"))
            .show(ui, |ui, label| ui.add(Toggle::switch(label, &mut s.smooth).label_hidden()));
        FormRow::new(tr!(ds, "A label long enough that it cannot fit in its column"))
            .help(tr!(ds, "The label is cut with an ellipsis; hovering shows it whole."))
            .show(ui, |ui, label| ui.add(TextField::new(label, &mut s.other)));
        ui.add(
            ParamSlider::new(tr!(ds, "Width (a slider row, for comparison)"), &mut s.width, 0.0..=1.0)
                .default_value(0.5),
        );
    });
    caption(
        ui,
        ds,
        "The label column is 38% of the row (96 to 160 pt), the same as a slider row, so mixed rows line up.",
    );
}
