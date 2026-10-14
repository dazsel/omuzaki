//! Controls: `ColourField`.

use egui::Ui;
use omuzaki_core::colour::Colour;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::tr;
use crate::ds::theme::ds;
use crate::ds::widgets::{ColourEvent, ColourField, PaletteSlot};
use crate::register_story;

struct State {
    free: Colour,
    linked: Colour,
    translucent: Colour,
    last: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            free: Colour::rgb(0x8A, 0x6F, 0xD1),
            linked: Colour::rgb(0xE6, 0xC3, 0x6A),
            translucent: Colour::rgba(0x5F, 0xD7, 0xB8, 0x99),
            last: "Open a field to change its colour.".to_owned(),
        }
    }
}

register_story! {
    id: "colour_field", title: "Colour field", group: Group::Controls, order: 60, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let palette = [
        PaletteSlot { name: "hair.root", colour: Colour::rgb(0x8A, 0x6F, 0xD1) },
        PaletteSlot { name: "hair.tip", colour: Colour::rgb(0xE6, 0xC3, 0x6A) },
        PaletteSlot { name: "skin.base", colour: Colour::rgb(0xF2, 0xC9, 0xB0) },
        PaletteSlot { name: "eye.iris", colour: Colour::rgb(0x5F, 0xD7, 0xB8) },
        PaletteSlot { name: "outfit.main", colour: Colour::rgb(0x34, 0x2F, 0x3D) },
    ];
    let recent = [Colour::rgb(0xF2, 0x5A, 0x72), Colour::rgb(0xF5, 0x8A, 0x1F), Colour::rgb(0xB9, 0xA2, 0xFF)];
    let neighbour = PaletteSlot { name: "skin.base", colour: palette[2].colour };
    let mut event = None;
    specimen(ui, ds, "Free", |ui| {
        let out = ColourField::new(tr!(ds, "Eye colour"), &mut s.free)
            .palette(&palette)
            .recent(&recent)
            .neighbour(neighbour)
            .show(ui);
        event = event.or(out.event);
    });
    specimen(ui, ds, "Linked", |ui| {
        let out =
            ColourField::new(tr!(ds, "Hair tip colour"), &mut s.linked).linked("hair.tip").palette(&palette).show(ui);
        event = event.or(out.event);
    });
    specimen(ui, ds, "Alpha", |ui| {
        let out = ColourField::new(tr!(ds, "Blush colour"), &mut s.translucent).alpha(true).show(ui);
        event = event.or(out.event);
    });
    specimen(ui, ds, "Disabled", |ui| {
        let mut c = Colour::rgb(0x66, 0x66, 0x70);
        ColourField::new(tr!(ds, "Locked colour"), &mut c).disabled(tr!(ds, "Managed by the theme")).show(ui);
    });
    if let Some(e) = event {
        s.last = match e {
            ColourEvent::Preview(_) => "Previewing live.".to_owned(),
            ColourEvent::Commit { .. } => "Committed one transaction.".to_owned(),
            ColourEvent::Cancel(_) => "Cancelled: restored the colour.".to_owned(),
            ColourEvent::Link(i) => format!("Linked to {}.", palette[i].name),
            ColourEvent::PickRequested => "The host would sample the screen now.".to_owned(),
        };
    }
    caption(ui, ds, &s.last);
}
