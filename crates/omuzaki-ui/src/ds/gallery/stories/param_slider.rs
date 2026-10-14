//! Controls: `ParamSlider`, in every state the spec lists (7.1).

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, panel, specimen};
use crate::ds::gallery::text::tr;
use crate::ds::theme::ds;
use crate::ds::widgets::{AgentTouch, OpCall, ParamEvent, ParamSlider};
use crate::register_story;

struct State {
    width: f64,
    height: f64,
    jaw: f64,
    angle: f64,
    brow: f64,
    mixed: f64,
    opacity: f64,
    locked: f64,
    driven: f64,
    disabled: f64,
    agent: f64,
    agent_old: f64,
    variant: f64,
    history: bool,
}

impl Default for State {
    fn default() -> Self {
        Self {
            width: 0.62,
            height: 0.5,
            jaw: 0.0,
            angle: 15.0,
            brow: -0.3,
            mixed: 0.4,
            opacity: 100.0,
            locked: 0.35,
            driven: 0.58,
            disabled: 0.2,
            agent: 0.78,
            agent_old: 0.64,
            variant: 0.7,
            history: false,
        }
    }
}

register_story! {
    id: "param_slider", title: "Param slider", group: Group::Controls, order: 40, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        // The first row knows which op sets it: its menu gains Copy op call, and Show in History reaches the host.
        let width = ParamSlider::new(tr!(ds, "Width"), &mut s.width, 0.0..=1.0)
            .default_value(0.5)
            .op_call(OpCall { op: "part.set_param", target: "\"part\":\"hair\",\"param\":\"width\"" })
            .show(ui);
        s.history |= width.events.iter().any(|e| *e == ParamEvent::ShowInHistory);
        ui.add(ParamSlider::new(tr!(ds, "Height"), &mut s.height, 0.0..=1.0).default_value(0.5));
        ui.add(ParamSlider::new(tr!(ds, "Jaw"), &mut s.jaw, 0.0..=1.0).default_value(0.0));
        ui.add(ParamSlider::new(tr!(ds, "Tilt"), &mut s.angle, -45.0..=45.0).default_value(0.0).unit("°").decimals(1));
        ui.add(ParamSlider::new(tr!(ds, "Brow"), &mut s.brow, -1.0..=1.0).default_value(0.0));
        ui.add(
            ParamSlider::new(tr!(ds, "Opacity"), &mut s.opacity, 0.0..=100.0)
                .default_value(100.0)
                .unit("%")
                .decimals(0),
        );
    });
    ui.add_space(8.0);
    specimen(ui, ds, "States", |_| {});
    panel(ds).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.spacing_mut().item_spacing.y = 2.0;
        ui.add(
            ParamSlider::new(tr!(ds, "Variant override"), &mut s.variant, 0.0..=1.0)
                .default_value(0.5)
                .variant_override(0.5),
        );
        // Frozen ages keep screenshots deterministic: 0.3 s is mid-glow, 5 s is the settled violet tick.
        ui.add(ParamSlider::new(tr!(ds, "Agent, just now"), &mut s.agent, 0.0..=1.0).default_value(0.5).agent_set(
            AgentTouch {
                by: tr!(ds, "Designer"),
                age_secs: 0.3,
                cause: Some(tr!(ds, "make the face more oval")),
                stamp: 1,
            },
        ));
        ui.add(ParamSlider::new(tr!(ds, "Agent, earlier"), &mut s.agent_old, 0.0..=1.0).default_value(0.5).agent_set(
            AgentTouch {
                by: tr!(ds, "Designer"),
                age_secs: 5.0,
                cause: Some(tr!(ds, "make the face more oval")),
                stamp: 2,
            },
        ));
        ui.add(ParamSlider::new(tr!(ds, "Mixed"), &mut s.mixed, 0.0..=1.0).default_value(0.5).mixed(true));
        ui.add(
            ParamSlider::new(tr!(ds, "Locked"), &mut s.locked, 0.0..=1.0)
                .default_value(0.5)
                .locked_by(tr!(ds, "Designer")),
        );
        ui.add(ParamSlider::new(tr!(ds, "Driven"), &mut s.driven, 0.0..=1.0).default_value(0.5).driven(0.35));
        ui.add(
            ParamSlider::new(tr!(ds, "Disabled"), &mut s.disabled, 0.0..=1.0).disabled(tr!(ds, "Pick a part first")),
        );
    });
    caption(
        ui,
        ds,
        "Drag, click the track to jump, type a number, or use the keyboard. Shift is fine control; Esc undoes a drag.",
    );
    caption(
        ui,
        ds,
        if s.history {
            "Show in History was chosen: the host reveals this parameter's records."
        } else {
            "Right-click: Reset, Copy value, Paste value, Copy op call, Show in History. Ctrl+C, Ctrl+V and Ctrl+Shift+C work on the focused track."
        },
    );
}
