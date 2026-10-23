//! The capture indicator in the status bar (ADR 0060 task UI-1; `docs/design/input-motion.md` §4): visible in every
//! capture state, `Off` included. Off, it says so and offers Start, which only the person at the studio can use; on, it
//! names the devices read, the ones that aren't, and how much of a key the motion sees, with a one-click Stop; paused,
//! it gives the full reason, naming the protected program, since this indicator is the person's own.
//!
//! The indicator is the precondition for capture, not a courtesy: each time it is drawn in a window that isn't
//! minimised it writes the handle's heartbeat, and the hub pauses capture when the heartbeat is older than 1.5 s. It
//! never waits on the hub: it reads the view the hub last wrote and asks for a frame twice a second, so an idle studio
//! keeps beating.

use std::time::Duration;

use egui::{Id, RichText, Ui, WidgetInfo, WidgetType};
use omuzaki_input_api::capture::{CaptureState, Detail, IndicatorView, PauseWhy};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use omuzaki_ops_api::{Actor, Runner};

use super::text as t;
use super::{InputSurface, run};
use crate::ds::a11y;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::Status;
use crate::ds::widgets::StatusChip;
use crate::panels::text::say;

/// How often an idle studio draws the indicator: well inside the hub's 1.5 s.
pub const BEAT_EVERY: Duration = Duration::from_millis(500);

/// What the person can do from the indicator in a state.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Start,
    Resume,
    Stop,
}

/// Draws the badge for `surface` in the status bar and beats its heartbeat; Start, Resume and Stop run their ops
/// through `runner` as `actor`.
pub fn badge(ui: &mut Ui, surface: &InputSurface, runner: &dyn Runner, actor: &Actor) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let view = surface.indicator().view();
    let (status, line) = line(&ds, &view);
    ui.add(StatusChip::new(status, &line));
    for &action in actions(&view.state) {
        if small_button(ui, &ds, action).clicked() {
            act(&ctx, runner, actor, action);
        }
    }
    if let Some(refused) = ctx.data(|d| d.get_temp::<String>(refused_id())) {
        ui.add(StatusChip::new(Status::Danger, &refused));
    }
    // Drawn in a window the person can see: the heartbeat. A minimised studio shows nothing, so it beats nothing.
    if ctx.input(|i| i.viewport().minimized != Some(true)) {
        surface.indicator().beat();
    }
    ctx.request_repaint_after(BEAT_EVERY);
}

/// The chip's status and words for `view`.
pub fn line(ds: &Ds, view: &IndicatorView) -> (Status, String) {
    match &view.state {
        CaptureState::Off => (Status::Info, say(ds, t::OFF, &[]).into_owned()),
        CaptureState::On { classes, degraded, .. } => {
            let detail = match view.detail {
                Detail::HandOnly => t::DETAIL_HANDS,
                Detail::Zones => t::DETAIL_ZONES,
            };
            let mut line =
                say(ds, t::ON, &[("classes", &classes_text(ds, *classes)), ("detail", &say(ds, detail, &[]))])
                    .into_owned();
            if *degraded != DeviceMask::NONE {
                let missing = say(ds, t::DEGRADED, &[("classes", &classes_text(ds, *degraded))]);
                line = format!("{line}; {missing}");
            }
            (Status::Warn, line)
        }
        CaptureState::Paused { why, .. } => {
            let line = match (why, view.program.as_deref()) {
                (Some(PauseWhy::IndicatorLost), _) => say(ds, t::PAUSED_HIDDEN, &[]),
                (Some(PauseWhy::ProtectedApp), Some(program)) => say(ds, t::PAUSED_PROGRAM, &[("program", program)]),
                (Some(PauseWhy::ProtectedApp), None) => say(ds, t::PAUSED_PROTECTED, &[]),
                _ => say(ds, t::PAUSED, &[]),
            };
            (Status::Info, line.into_owned())
        }
        CaptureState::Failed { reason, .. } => (Status::Danger, say(ds, t::FAILED, &[("reason", reason)]).into_owned()),
    }
}

fn actions(state: &CaptureState) -> &'static [Action] {
    match state {
        CaptureState::Off => &[Action::Start],
        CaptureState::Paused { .. } => &[Action::Resume, Action::Stop],
        CaptureState::On { .. } | CaptureState::Failed { .. } => &[Action::Stop],
    }
}

/// "keyboard, mouse and gamepad", or "every device".
pub fn classes_text(ds: &Ds, classes: DeviceMask) -> String {
    if classes == DeviceMask::ALL {
        return say(ds, t::EVERY_DEVICE, &[]).into_owned();
    }
    let names: Vec<String> = DeviceClass::ALL
        .iter()
        .filter(|class| classes.contains(**class))
        .map(|class| say(ds, class_word(*class), &[]).into_owned())
        .collect();
    match names.as_slice() {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => say(ds, t::AND, &[("first", &rest.join(", ")), ("last", last)]).into_owned(),
    }
}

/// A device class's word.
pub fn class_word(class: DeviceClass) -> &'static str {
    match class {
        DeviceClass::Keyboard => t::KEYBOARD,
        DeviceClass::Mouse => t::MOUSE,
        DeviceClass::Pen => t::PEN,
        DeviceClass::Gamepad => t::GAMEPAD,
        DeviceClass::Midi => t::MIDI,
        DeviceClass::Osc => t::OSC,
    }
}

fn small_button(ui: &mut Ui, ds: &Ds, action: Action) -> egui::Response {
    let word = match action {
        Action::Start => t::START,
        Action::Resume => t::RESUME,
        Action::Stop => t::STOP,
    };
    let label = say(ds, word, &[]).into_owned();
    let text = RichText::new(&label).font(ds.text.small.clone()).color(ds.theme.accent_text);
    let response = ui.add(egui::Button::new(text).small().frame(false));
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Button, true, &label), |_| {});
    if action == Action::Start { response.on_hover_text(say(ds, t::START_TIP, &[])) } else { response }
}

/// Runs `action`'s op as the person; a refusal is shown beside the badge until the next action.
pub(super) fn act(ctx: &egui::Context, runner: &dyn Runner, actor: &Actor, action: Action) {
    let (op, word) = match action {
        Action::Start | Action::Resume => {
            ("input.capture.start", if action == Action::Start { t::START } else { t::RESUME })
        }
        Action::Stop => ("input.capture.stop", t::STOP),
    };
    let ds = ds(ctx);
    match run(runner, actor, op, serde_json::json!({})) {
        Ok(_) => ctx.data_mut(|d| d.remove::<String>(refused_id())),
        Err(reason) => {
            let line = say(&ds, t::REFUSED, &[("action", &say(&ds, word, &[])), ("reason", &reason)]).into_owned();
            ctx.data_mut(|d| d.insert_temp(refused_id(), line));
        }
    }
}

/// Starts capture as the person, from the Input panel.
pub(super) fn start(ctx: &egui::Context, runner: &dyn Runner, actor: &Actor) {
    act(ctx, runner, actor, Action::Start);
}

/// Stops capture, from the Input panel.
pub(super) fn stop(ctx: &egui::Context, runner: &dyn Runner, actor: &Actor) {
    act(ctx, runner, actor, Action::Stop);
}

fn refused_id() -> Id {
    Id::new("omuzaki-input-refused")
}

/// The refusal last shown, if any.
pub(super) fn refused(ctx: &egui::Context) -> Option<String> {
    ctx.data(|d| d.get_temp::<String>(refused_id()))
}
