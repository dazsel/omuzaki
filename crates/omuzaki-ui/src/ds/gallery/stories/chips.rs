//! Display: the chip family.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::tr;
use crate::ds::theme::ds;
use crate::ds::tokens::Status;
use crate::ds::widgets::{ActorChip, ActorKind, CostChip, GateChip, ScopeBadge, StatusChip};
use omuzaki_ui_api::register_story;

#[derive(Default)]
struct State;

register_story! {
    id: "chips", title: "Chips", group: Group::Display, order: 10, state: State,
    show: |ui, _| show(ui),
}

fn show(ui: &mut Ui) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    specimen(ui, ds, "Actors", |ui| {
        ui.add(ActorChip::new(tr!(ds, "Designer"), ActorKind::Agent));
        ui.add(ActorChip::new(tr!(ds, "Producer"), ActorKind::Person));
        ui.add(ActorChip::new(tr!(ds, "VMC bridge"), ActorKind::Plugin));
        ui.add(ActorChip::new(tr!(ds, "Autosave"), ActorKind::System));
    });
    specimen(ui, ds, "Scopes", |ui| {
        ui.add(ScopeBadge::new("edit:character"));
        ui.add(ScopeBadge::new("read"));
        ui.add(ScopeBadge::new("render"));
        ui.add(ScopeBadge::new("broadcast"));
        ui.add(ScopeBadge::new("spend"));
    });
    specimen(ui, ds, "Cost", |ui| {
        ui.add(CostChip::new(&["~40 s", "1.2 GB", "$0.12"]));
        ui.add(CostChip::new(&["~2 s"]));
    });
    specimen(ui, ds, "Status", |ui| {
        ui.add(StatusChip::new(Status::Ok, tr!(ds, "Tracking connected")));
        ui.add(StatusChip::new(Status::Warn, tr!(ds, "Degraded")));
        ui.add(StatusChip::new(Status::Danger, tr!(ds, "LIVE")));
        ui.add(StatusChip::new(Status::Info, tr!(ds, "Preview")));
    });
    specimen(ui, ds, "Gates", |ui| {
        ui.add(GateChip::passed(tr!(ds, "likeness"), "0.88"));
        ui.add(GateChip::failed(tr!(ds, "likeness"), "0.81", "0.85"));
        ui.add(GateChip::failed(tr!(ds, "readability"), "0.62", "0.70")).highlight();
    });
    caption(ui, ds, "Every status is an icon, a word and a colour. Agents are the only violet.");
}
