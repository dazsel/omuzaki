//! Display: `ChannelMeter` and `MeterGrid`.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{Channel, MeterGrid, Owner, Toggle};
use omuzaki_ui_api::register_story;

#[derive(Default)]
struct State {
    live: bool,
}

register_story! {
    id: "channel_meter", title: "Channel meters", group: Group::Display, order: 30, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let time = if s.live { ui.input(|i| i.time) as f32 } else { 0.0 };
    let wave = |phase: f32, base: f32, amp: f32| (base + amp * (time * 1.7 + phase).sin()).clamp(0.0, 1.0);
    let names = [
        "Jaw open",
        "Eye blink L",
        "Eye blink R",
        "Brow inner up",
        "Mouth funnel",
        "Mouth smile L",
        "Cheek puff",
        "Eye look up",
        "Tongue out",
        "Nose sneer",
    ]
    .map(|name| translate(ds, name));
    let mut channels = [
        Channel { calibrated: Some((0.1, 0.85)), dead_zone: Some((0.0, 0.08)), ..Channel::new(&names[0], 0.52) },
        Channel { raw: 0.45, ..Channel::new(&names[1], 0.88) },
        Channel { raw: 0.9, ..Channel::new(&names[2], 0.7) },
        Channel { confidence: 0.3, ..Channel::new(&names[3], 0.4) },
        Channel { owner: Owner::Audio, ..Channel::new(&names[4], 0.34) },
        Channel { clipping: true, raw: 1.0, ..Channel::new(&names[5], 1.0) },
        Channel { owner: Owner::Ai, ..Channel::new(&names[6], 0.22) },
        Channel { owner: Owner::LifeLayer, ..Channel::new(&names[7], 0.15) },
        Channel { owner: Owner::Hotkey, ..Channel::new(&names[8], 0.0) },
        Channel::new(&names[9], 0.08),
    ];
    if s.live {
        for (i, c) in channels.iter_mut().enumerate() {
            let v = wave(i as f32 * 0.9, 0.5, 0.4);
            c.processed = v;
            c.raw = (v + 0.08 * (time * 3.1 + i as f32).sin()).clamp(0.0, 1.0);
        }
    }
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        MeterGrid::new(tr!(ds, "Face channels"), &channels).columns(1).values(true).show(ui);
    });
    ui.add_space(8.0);
    ui.add(Toggle::switch(tr!(ds, "Animate"), &mut s.live));
    caption(
        ui,
        ds,
        "Fill: processed. Tick: raw. Brackets: calibrated range. Hatching: low confidence. Triangle: clipping. Violet icon: an AI owns the channel.",
    );
}
