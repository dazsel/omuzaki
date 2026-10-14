//! Composites: an inspector panel assembled from the components, to judge how they sit together (`shell.md` 7).

use egui::{Id, RichText, Ui};
use omuzaki_core::colour::Colour;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{
    ActorChip, ActorKind, AgentTouch, ColourField, FormRow, ParamSlider, Section, Segment, Segmented, TextField, Toggle,
};
use crate::register_story;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Profile {
    Round,
    Oval,
    Heart,
    Square,
}

struct State {
    search: String,
    width: f64,
    height: f64,
    jaw: f64,
    chin: f64,
    cheek: f64,
    profile: Profile,
    skin: Colour,
    smooth: bool,
    tilt: f64,
    eye_gap: f64,
}

impl Default for State {
    fn default() -> Self {
        Self {
            search: String::new(),
            width: 0.62,
            height: 0.5,
            jaw: 0.34,
            chin: 0.5,
            cheek: 0.7,
            profile: Profile::Oval,
            skin: Colour::rgb(0xF2, 0xC9, 0xB0),
            smooth: true,
            tilt: 0.0,
            eye_gap: 0.5,
        }
    }
}

register_story! {
    id: "inspector", title: "Inspector", group: Group::Composites, order: 10, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let t = &ds.theme;
    let group = Id::new("inspector_sections");
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 4.0;
        // Header: what is selected, and who last edited it.
        ui.horizontal(|ui| {
            ui.label(RichText::new(translate(ds, "Face profile")).font(ds.text.section.clone()).color(t.text));
            ui.label(RichText::new(translate(ds, "Head › Face")).font(ds.text.small.clone()).color(t.text3));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(translate(ds, "2 m ago")).font(ds.text.small.clone()).color(t.text3));
                ui.add(ActorChip::new(tr!(ds, "Designer"), ActorKind::Agent));
            });
        });
        ui.add(TextField::new(tr!(ds, "Search settings"), &mut s.search).search().hint(tr!(ds, "Search settings")));
        ui.add_space(4.0);

        Section::new(tr!(ds, "Essentials")).default_open(true).group(group).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.add(ParamSlider::new(tr!(ds, "Width"), &mut s.width, 0.0..=1.0).default_value(0.5));
            ui.add(ParamSlider::new(tr!(ds, "Height"), &mut s.height, 0.0..=1.0).default_value(0.5));
            ui.add(ParamSlider::new(tr!(ds, "Jaw"), &mut s.jaw, 0.0..=1.0).default_value(0.5).agent_set(AgentTouch {
                by: tr!(ds, "Designer"),
                age_secs: 6.0,
                cause: Some(tr!(ds, "make the face more oval")),
                stamp: 7,
            }));
            ui.add(ParamSlider::new(tr!(ds, "Chin"), &mut s.chin, 0.0..=1.0).default_value(0.5));
            FormRow::new(tr!(ds, "Profile")).show(ui, |ui, label| {
                let (round, oval, heart, square) =
                    (translate(ds, "Round"), translate(ds, "Oval"), translate(ds, "Heart"), translate(ds, "Square"));
                ui.add(
                    Segmented::new(
                        label,
                        &mut s.profile,
                        &[
                            Segment::new(Profile::Round, &round),
                            Segment::new(Profile::Oval, &oval),
                            Segment::new(Profile::Heart, &heart),
                            Segment::new(Profile::Square, &square),
                        ],
                    )
                    .stretch(),
                );
            });
            FormRow::new(tr!(ds, "Skin tone")).show(ui, |ui, label| {
                ColourField::new(label, &mut s.skin).show(ui);
            });
            FormRow::new(tr!(ds, "Smooth shading")).show(ui, |ui, label| {
                ui.add(Toggle::switch(label, &mut s.smooth).label_hidden());
            });
        });
        Section::new(tr!(ds, "Advanced")).badge(tr!(ds, "6 more")).group(group).show(ui, |ui| {
            ui.add(ParamSlider::new(tr!(ds, "Cheek fullness"), &mut s.cheek, 0.0..=1.0).default_value(0.5));
            ui.add(
                ParamSlider::new(tr!(ds, "Tilt"), &mut s.tilt, -45.0..=45.0).default_value(0.0).unit("°").decimals(1),
            );
            ui.add(
                ParamSlider::new(tr!(ds, "Eye gap"), &mut s.eye_gap, 0.0..=1.0)
                    .default_value(0.5)
                    .variant_override(0.42),
            );
        });
        Section::new(tr!(ds, "Variants")).group(group).show(ui, |ui| {
            ui.label(
                RichText::new(translate(ds, "game: base values · close-up: 3 overrides"))
                    .font(ds.text.small.clone())
                    .color(t.text3),
            );
        });
    });
    caption(ui, ds, "Click a section to toggle it; Ctrl+click opens it and collapses the others.");
}
