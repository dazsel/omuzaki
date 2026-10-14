//! Controls: `Dropdown`, and the `Listbox` it opens, drawn open so a still image can show it.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::theme::ds;
use crate::ds::tokens::Theme;
use crate::ds::widgets::{Choice, Dropdown, ListItem, Listbox};
use crate::register_story;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Look {
    Dark,
    Light,
    HighContrast,
}

/// A part to choose among: enough of them that typing is faster than scrolling.
const PARTS: [&str; 14] = [
    "Back hair",
    "Bangs",
    "Body",
    "Brows",
    "Eyelashes",
    "Eyes",
    "Face",
    "Front hair",
    "Hands",
    "Mouth",
    "Neck",
    "Outfit",
    "Shoes",
    "Twin tails",
];

struct State {
    look: Look,
    part: usize,
    icon: usize,
    unset: Option<usize>,
    long: usize,
    locked: usize,
}

impl Default for State {
    fn default() -> Self {
        Self { look: Look::Dark, part: 7, icon: 0, unset: None, long: 0, locked: 0 }
    }
}

register_story! {
    id: "dropdown", title: "Dropdown and listbox", group: Group::Controls, order: 35, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    specimen(ui, ds, "Few", |ui| {
        let (dark, light, high) = (translate(ds, "Dark"), translate(ds, "Light"), translate(ds, "High contrast"));
        ui.add(Dropdown::new(
            tr!(ds, "Appearance"),
            &mut s.look,
            &[Choice::new(Look::Dark, &dark), Choice::new(Look::Light, &light), Choice::new(Look::HighContrast, &high)],
        ));
    });
    specimen(ui, ds, "Many", |ui| {
        let names: Vec<_> = PARTS.iter().map(|p| translate(ds, p)).collect();
        let choices: Vec<_> = names.iter().enumerate().map(|(i, n)| Choice::new(i, n)).collect();
        ui.add(Dropdown::new(tr!(ds, "Part"), &mut s.part, &choices));
    });
    specimen(ui, ds, "Icons", |ui| {
        let (front, three, side) = (translate(ds, "Front"), translate(ds, "Three-quarter"), translate(ds, "Side"));
        let (k1, k2, k3) = ("1", "2", "3");
        ui.add(Dropdown::new(
            tr!(ds, "Camera"),
            &mut s.icon,
            &[
                Choice::new(0, &front).icon(icons::USER).detail(k1),
                Choice::new(1, &three).icon(icons::CUBE).detail(k2),
                Choice::new(2, &side).icon(icons::EYE).detail(k3),
            ],
        ));
    });
    specimen(ui, ds, "Nothing chosen", |ui| {
        let (a, b) = (translate(ds, "Almond"), translate(ds, "Round"));
        ui.add(
            Dropdown::new(tr!(ds, "Eye shape"), &mut s.unset, &[Choice::new(Some(0), &a), Choice::new(Some(1), &b)])
                .placeholder(tr!(ds, "Pick a shape")),
        );
    });
    specimen(ui, ds, "Long label", |ui| {
        let (a, b) = (
            translate(ds, "Soft studio light with a warm rim from behind and a cool fill"),
            translate(ds, "Hard noon sun"),
        );
        ui.add(Dropdown::new(tr!(ds, "Light rig"), &mut s.long, &[Choice::new(0, &a), Choice::new(1, &b)]));
    });
    specimen(ui, ds, "Disabled", |ui| {
        let a = translate(ds, "Base");
        ui.add(
            Dropdown::new(tr!(ds, "Variant"), &mut s.locked, &[Choice::new(0, &a)])
                .disabled(tr!(ds, "Only one variant exists")),
        );
    });
    specimen(ui, ds, "Open", open_list);
    caption(
        ui,
        ds,
        "Enter, Space or the arrows open it; type to jump to a row; Enter chooses, Esc closes. More than five exclusive options belong here, fewer in a segmented control.",
    );
}

/// The list as the dropdown opens it, with the second row chosen and the fourth active and one row unavailable.
fn open_list(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let t: &Theme = &ds.theme;
    let names = ["Front hair", "Back hair", "Bangs", "Twin tails", "Wig"].map(|n| translate(ds, n));
    let why = translate(ds, "No wig in this character");
    let rows = [
        ListItem::new(&names[0]).icon(icons::HAIR_DRYER),
        ListItem::new(&names[1]).icon(icons::HAIR_DRYER).detail("hair.back"),
        ListItem::new(&names[2]).icon(icons::HAIR_DRYER),
        ListItem::new(&names[3]).icon(icons::HAIR_DRYER),
        ListItem::new(&names[4]).icon(icons::HAIR_DRYER).disabled(&why),
    ];
    paint::overlay_frame(ds).fill(t.bg2).inner_margin(4).show(ui, |ui| {
        ui.set_width(220.0);
        let id = ui.id().with("open_list");
        Listbox::new(id, tr!(ds, "Hair parts"), &rows).selected(Some(1)).active(Some(3)).show(ui);
    });
}
