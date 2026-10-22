//! Controls: Segmented.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::widgets::{Segment, Segmented};
use omuzaki_ui_api::register_story;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Side {
    Left,
    Centre,
    Right,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Quality {
    Live,
    High,
    Offline,
}

struct State {
    side: Side,
    quality: Quality,
    view: u8,
}

impl Default for State {
    fn default() -> Self {
        Self { side: Side::Centre, quality: Quality::Live, view: 1 }
    }
}

register_story! {
    id: "segmented", title: "Segmented control", group: Group::Controls, order: 30, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    specimen(ui, ds, "Text", |ui| {
        let (left, centre, right) = (translate(ds, "Left"), translate(ds, "Centre"), translate(ds, "Right"));
        ui.add(Segmented::new(
            tr!(ds, "Fringe side"),
            &mut s.side,
            &[Segment::new(Side::Left, &left), Segment::new(Side::Centre, &centre), Segment::new(Side::Right, &right)],
        ));
    });
    specimen(ui, ds, "Quality", |ui| {
        let (live, high, offline) = (translate(ds, "Live"), translate(ds, "High"), translate(ds, "Offline"));
        ui.add(Segmented::new(
            tr!(ds, "Render quality"),
            &mut s.quality,
            &[
                Segment::new(Quality::Live, &live),
                Segment::new(Quality::High, &high),
                Segment::new(Quality::Offline, &offline),
            ],
        ));
    });
    specimen(ui, ds, "Icons", |ui| {
        let (front, three_quarter, side) =
            (translate(ds, "Front"), translate(ds, "Three-quarter"), translate(ds, "Side"));
        ui.add(Segmented::new(
            tr!(ds, "Camera view"),
            &mut s.view,
            &[
                Segment::new(0u8, &front).icon(icons::USER).icon_only(),
                Segment::new(1u8, &three_quarter).icon(icons::CUBE).icon_only(),
                Segment::new(2u8, &side).icon(icons::EYE).icon_only(),
            ],
        ));
    });
    specimen(ui, ds, "Disabled", |ui| {
        let mut v = 0u8;
        let (one, two) = (translate(ds, "One"), translate(ds, "Two"));
        ui.add(
            Segmented::new(tr!(ds, "Locked choice"), &mut v, &[Segment::new(0u8, &one), Segment::new(1u8, &two)])
                .disabled(tr!(ds, "Managed by the project")),
        );
    });
    caption(ui, ds, "One tab stop; Left and Right move and select. More than five options belong in a dropdown.");
}
