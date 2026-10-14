//! Foundations: the type scale.

use egui::{RichText, Ui};

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::translate;
use crate::ds::theme::ds;
use crate::register_story;

#[derive(Default)]
struct State;

register_story! {
    id: "type", title: "Type", group: Group::Foundations, order: 20, state: State,
    show: |ui, _| show(ui),
}

fn show(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let t = &ds.theme;
    let rows = [
        ("Display 28 · Bricolage 600", "Start with a character", ds.text.display.clone()),
        ("Title 18 · Instrument Sans 600", "Dialog and workspace titles", ds.text.title.clone()),
        ("Section 14 · Instrument Sans 600", "Face profile", ds.text.section.clone()),
        ("Body 13 · Instrument Sans 400", "Controls, labels and tree rows read like this.", ds.text.body.clone()),
        ("Body strong 13 · Medium 500", "Selected rows and button labels", ds.text.strong.clone()),
        ("Small 12 · Instrument Sans 400", "Secondary text, tooltips and chips", ds.text.small.clone()),
        ("Micro 11 · Medium 500", "BADGES AND UNITS ONLY", ds.text.micro.clone()),
        ("Mono 12.5 · JetBrains Mono", "0.62  face.set_profile  #8A6FD1", ds.text.mono.clone()),
    ];
    for (meta, sample, font) in rows {
        ui.label(RichText::new(translate(&ds, sample)).font(font).color(t.text));
        ui.label(RichText::new(translate(&ds, meta)).font(egui::FontId::monospace(10.5)).color(t.text3));
        ui.add_space(6.0);
    }
    caption(ui, &ds, "Numbers are always mono and right-aligned. Weights are separate families, never synthetic bold.");
}
