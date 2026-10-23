//! The Input panel: capture and what it needs here, the bindings with their keys, and the protected programs.
//!
//! - **Capture** (#752's switch): the indicator's line with Start or Stop; a backend waiting for an OS permission
//!   says how to grant it, with Check again; a class this machine can't read says so.
//! - **Bindings** (UI-2): each binding from `input.binding.list` with what it does, its keys and a conflict warning.
//!   This panel is the only place keys are learnt or shown: Set keys asks the hub's [`ChordStore`] to read the next
//!   chord (off the frame, up to the hub's own wait), and the keys are drawn straight from
//!   [`ChordLabel::render`](omuzaki_input_api::binding::ChordLabel::render) each frame and never kept, logged or sent
//!   through an op. Learning reads the keyboard, so it needs capture on; with capture off the panel says so, under the
//!   Capture section's Start. A conflict is shown, never overwritten. Remove and the presets go through the ops.
//! - **Protected programs**: the person's own kill-switch list, read and written through the hub's [`ProtectList`]
//!   (never an op, so no agent can read it).
//!
//! [`ChordStore`]: omuzaki_input_api::feed::ChordStore
//! [`ProtectList`]: omuzaki_input_api::feed::ProtectList

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{RichText, Ui};
use omuzaki_input_api::binding::{BindingAction, BindingId, BindingInfo, Intent};
use omuzaki_input_api::capture::{BackendState, CaptureReport, CaptureState, PermissionHow, Support};
use omuzaki_input_api::feed::ChordStore;
use omuzaki_ops_api::{Actor, Runner};
use serde_json::json;

use super::indicator::{self, line};
use super::text as t;
use super::{InputSurface, run};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Button, EmptyState, Section, StatusChip, TextField};
use crate::panels::text::say;

/// How often the panel reads `input.capture.state` and `input.binding.list` while shown.
const READ_EVERY: f64 = 1.0;

/// A Set keys in flight: the binding, its name, and where the hub's answer arrives.
struct Learning {
    id: BindingId,
    name: String,
    answer: Receiver<Result<(), String>>,
}

/// The Input panel's state between frames. It holds no chord and no label: those are read from the hub as drawn.
#[derive(Default)]
pub struct InputPanel {
    report: Option<CaptureReport>,
    bindings: Vec<BindingInfo>,
    presets: Vec<String>,
    read_at: Option<f64>,
    learning: Option<Learning>,
    error: Option<String>,
    program: String,
}

impl std::fmt::Debug for InputPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InputPanel")
            .field("bindings", &self.bindings.len())
            .field("learning", &self.learning.as_ref().map(|l| l.id))
            .finish_non_exhaustive()
    }
}

impl InputPanel {
    /// Whether the panel is waiting for keys.
    pub fn is_learning(&self) -> bool {
        self.learning.is_some()
    }

    /// Draws the panel over `surface` (the hub's handles), running ops through `runner` as `actor`.
    pub fn ui(&mut self, ui: &mut Ui, surface: Option<&InputSurface>, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let Some(surface) = surface else {
            let (headline, line) = (say(&ds, t::NO_HUB, &[]), say(&ds, t::NO_HUB_HOW, &[]));
            EmptyState::new(icons::KEYBOARD, &headline, &line).show(ui);
            return;
        };
        let now = ui.input(|i| i.time);
        if self.read_at.is_none_or(|at| now - at >= READ_EVERY) {
            self.read(runner, actor);
            self.read_at = Some(now);
        }
        self.poll_learning(&ctx, runner, actor);
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(READ_EVERY));
        ui.spacing_mut().item_spacing.y = space::S8;
        self.capture(ui, &ds, surface, runner, actor);
        self.bindings(ui, &ds, surface, runner, actor);
        self.protected(ui, &ds, surface);
    }

    fn read(&mut self, runner: &dyn Runner, actor: &Actor) {
        match run(runner, actor, "input.capture.state", json!({})) {
            Ok(value) => self.report = serde_json::from_value(value).ok(),
            Err(error) => self.error = Some(error),
        }
        match run(runner, actor, "input.binding.list", json!({})) {
            Ok(value) => {
                self.bindings = serde_json::from_value(value["bindings"].clone()).unwrap_or_default();
                self.presets = serde_json::from_value(value["presets"].clone()).unwrap_or_default();
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn poll_learning(&mut self, ctx: &egui::Context, runner: &dyn Runner, actor: &Actor) {
        let Some(learning) = &self.learning else { return };
        match learning.answer.try_recv() {
            Ok(Ok(())) => {
                self.learning = None;
                self.error = None;
                self.read(runner, actor);
            }
            Ok(Err(reason)) => {
                self.learning = None;
                self.error = Some(say(&ds(ctx), t::LEARN_FAILED, &[("reason", &reason)]).into_owned());
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => self.learning = None,
        }
    }

    fn capture(&mut self, ui: &mut Ui, ds: &Ds, surface: &InputSurface, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = say(ds, t::CAPTURE, &[]);
        Section::new(&title).default_open(true).id_salt("input.capture").show(ui, |ui| {
            let view = surface.indicator().view();
            let (status, words) = line(ds, &view);
            ui.add(StatusChip::new(status, &words));
            let on = !matches!(view.state, CaptureState::Off);
            if on {
                if ui.add(Button::new(&say(ds, t::STOP, &[]))).clicked() {
                    indicator::stop(&ctx, runner, actor);
                }
            } else if ui.add(Button::new(&say(ds, t::START, &[])).primary()).clicked() {
                indicator::start(&ctx, runner, actor);
                self.read(runner, actor);
            }
            if let Some(refused) = indicator::refused(&ctx) {
                ui.add(StatusChip::new(Status::Danger, &refused));
            }
            let Some(report) = self.report.clone() else { return };
            let mut needs = false;
            for backend in &report.backends {
                let note = match &backend.state {
                    BackendState::NeedsPermission { how: PermissionHow::InputMonitoring, .. } => {
                        needs = true;
                        say(ds, t::NEEDS_MONITORING, &[]).into_owned()
                    }
                    BackendState::NeedsPermission { .. } => {
                        needs = true;
                        say(ds, t::NEEDS_GROUP, &[]).into_owned()
                    }
                    BackendState::Failed { reason, .. } => {
                        say(ds, t::BACKEND_FAILED, &[("backend", &backend.name), ("reason", reason)]).into_owned()
                    }
                    BackendState::Running => continue,
                };
                ui.label(RichText::new(note).font(ds.text.small.clone()));
            }
            if needs && ui.add(Button::new(&say(ds, t::CHECK_AGAIN, &[])).ghost()).clicked() {
                self.read(runner, actor);
            }
            // What this machine can't read, said once per class; what works needs no words.
            let caps = report.caps;
            let rows =
                [(t::KEYBOARD, caps.typing), (t::MOUSE, caps.pointer), (t::PEN, caps.pen), (t::GAMEPAD, caps.gamepad)];
            for (class, support) in rows {
                let word = match support {
                    Support::Available => continue,
                    Support::NeedsOptIn => t::SUPPORT_OPT_IN,
                    Support::NeedsPermission => t::SUPPORT_PERMISSION,
                    Support::Portal => t::SUPPORT_PORTAL,
                    Support::Unavailable => t::SUPPORT_NO,
                };
                let line = say(ds, t::SUPPORT, &[("class", &say(ds, class, &[])), ("support", &say(ds, word, &[]))]);
                ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
            }
        });
    }

    fn bindings(&mut self, ui: &mut Ui, ds: &Ds, surface: &InputSurface, runner: &dyn Runner, actor: &Actor) {
        let title = say(ds, t::BINDINGS, &[]);
        let capturing = surface.indicator().view().state.is_on();
        Section::new(&title).default_open(true).id_salt("input.bindings").show(ui, |ui| {
            if !capturing && !self.bindings.is_empty() {
                ui.label(RichText::new(say(ds, t::NOT_CAPTURING, &[])).font(ds.text.small.clone()));
            }
            if self.bindings.is_empty() {
                ui.label(RichText::new(say(ds, t::NO_BINDINGS, &[])).strong());
                ui.label(RichText::new(say(ds, t::NO_BINDINGS_HOW, &[])).font(ds.text.small.clone()));
            }
            let mut asks = Vec::new();
            for binding in &self.bindings {
                ui.separator();
                self.row(ui, ds, surface.chords(), binding, capturing, &mut asks);
            }
            if let Some(error) = &self.error {
                ui.add(StatusChip::new(Status::Danger, error));
            }
            for preset in &self.presets {
                let label = say(ds, t::ADD_PRESET, &[("preset", preset)]);
                if ui.add(Button::new(&label).ghost().icon(icons::PLUS)).clicked() {
                    asks.push(Ask::Preset(preset.clone()));
                }
            }
            for ask in asks {
                self.answer(ask, surface.chords(), runner, actor);
            }
        });
    }

    /// One binding: its name and action, its keys as the hub renders them, a conflict warning, and its buttons.
    fn row(
        &self,
        ui: &mut Ui,
        ds: &Ds,
        chords: &Arc<dyn ChordStore>,
        binding: &BindingInfo,
        capturing: bool,
        asks: &mut Vec<Ask>,
    ) {
        let name = binding.name.as_str();
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new(name).strong());
            ui.label(RichText::new(action_text(ds, &binding.action)).font(ds.text.small.clone()).color(ds.theme.text2));
        });
        let learning = self.learning.as_ref().is_some_and(|l| l.id == binding.id);
        ui.horizontal_wrapped(|ui| {
            if learning {
                ui.add(StatusChip::new(Status::Warn, &say(ds, t::PRESS_KEYS, &[("name", name)])));
            } else {
                keys(ui, ds, chords.as_ref(), binding.id, name);
            }
            let set = say(ds, t::SET_KEYS, &[]);
            let set_for = say(ds, t::SET_KEYS_FOR, &[("name", name)]);
            let not_capturing = say(ds, t::NOT_CAPTURING, &[]);
            let button = Button::new(&set).accessible_label(&set_for).disabled_if(!capturing, &not_capturing);
            let busy = self.learning.as_ref().map(|l| say(ds, t::PRESS_KEYS, &[("name", &l.name)])).unwrap_or_default();
            let button = button.disabled_if(self.learning.is_some() && capturing, &busy);
            if ui.add(button).clicked() {
                asks.push(Ask::Learn(binding.id, binding.name.clone()));
            }
            let clear = say(ds, t::CLEAR_KEYS, &[("name", name)]);
            if ui.add(Button::new(&say(ds, t::CLEAR_SHORT, &[])).ghost().accessible_label(&clear)).clicked() {
                asks.push(Ask::Clear(binding.id));
            }
            let remove = say(ds, t::REMOVE, &[("name", name)]);
            let word = say(ds, t::REMOVE_SHORT, &[]);
            if ui.add(Button::new(&word).ghost().icon(icons::TRASH).accessible_label(&remove)).clicked() {
                asks.push(Ask::Remove(binding.id));
            }
        });
        if binding.conflict {
            ui.add(StatusChip::new(Status::Warn, &say(ds, t::CONFLICT, &[])));
        }
    }

    fn answer(&mut self, ask: Ask, chords: &Arc<dyn ChordStore>, runner: &dyn Runner, actor: &Actor) {
        let result = match ask {
            Ask::Learn(id, name) => {
                let (send, answer) = channel();
                let store = Arc::clone(chords);
                // The hub waits for the chord; the frame never does. The label it returns is dropped unread here:
                // the row draws it from the store.
                let spawned = std::thread::Builder::new().name("input-learn".to_owned()).spawn(move || {
                    let _ = send.send(store.learn(id).map(drop).map_err(|error| error.to_string()));
                });
                match spawned {
                    Ok(_) => {
                        self.learning = Some(Learning { id, name, answer });
                        Ok(())
                    }
                    Err(error) => Err(error.to_string()),
                }
            }
            Ask::Clear(id) => {
                chords.clear(id);
                Ok(())
            }
            Ask::Remove(id) => run(runner, actor, "input.binding.remove", json!({ "id": id })).map(drop),
            Ask::Preset(preset) => {
                run(runner, actor, "input.binding.apply_preset", json!({ "preset": preset })).map(drop)
            }
        };
        match result {
            Ok(()) => {
                self.error = None;
                self.read(runner, actor);
            }
            Err(error) => self.error = Some(error),
        }
    }

    fn protected(&mut self, ui: &mut Ui, ds: &Ds, surface: &InputSurface) {
        let title = say(ds, t::PROTECTED, &[]);
        Section::new(&title).default_open(true).id_salt("input.protected").show(ui, |ui| {
            ui.label(RichText::new(say(ds, t::PROTECTED_HOW, &[])).font(ds.text.small.clone()));
            let mut programs = surface.protect().get();
            if programs.is_empty() {
                ui.label(RichText::new(say(ds, t::NO_PROGRAMS, &[])).color(ds.theme.text2));
            }
            let mut removed = None;
            for (i, program) in programs.iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.label(program.as_str());
                    let remove = say(ds, t::REMOVE_PROGRAM, &[("program", program)]);
                    let word = say(ds, t::REMOVE_SHORT, &[]);
                    if ui.add(Button::new(&word).ghost().accessible_label(&remove)).clicked() {
                        removed = Some(i);
                    }
                });
            }
            let label = say(ds, t::PROGRAM, &[]);
            let hint = say(ds, t::PROGRAM_HINT, &[]);
            let field = ui.add(TextField::new(&label, &mut self.program).hint(&hint).id_salt("input.program"));
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            let add = ui.add(Button::new(&say(ds, t::ADD, &[])).disabled_if(self.program.trim().is_empty(), &hint));
            let name = self.program.trim().to_owned();
            if (add.clicked() || entered) && !name.is_empty() && !programs.contains(&name) {
                programs.push(name);
                surface.protect().set(&programs);
                self.program.clear();
            }
            if let Some(i) = removed {
                programs.remove(i);
                surface.protect().set(&programs);
            }
        });
    }
}

/// What a row asked for.
enum Ask {
    Learn(BindingId, String),
    Clear(BindingId),
    Remove(BindingId),
    Preset(String),
}

/// A binding's keys, drawn straight from the hub's label: nothing of the text is kept.
fn keys(ui: &mut Ui, ds: &Ds, chords: &dyn ChordStore, id: BindingId, name: &str) {
    let spoken = say(ds, t::KEYS_OF, &[("name", name)]);
    match chords.label(id) {
        Some(label) => label.render(&mut |text| {
            let response = ui.add(egui::Label::new(RichText::new(text).monospace().strong()));
            // The keys are the value of a labelled row, read out with its name.
            crate::ds::a11y::describe(
                &response,
                || egui::WidgetInfo::labeled(egui::WidgetType::Label, true, text),
                |node| node.set_label(spoken.clone()),
            );
        }),
        None => {
            ui.label(RichText::new(say(ds, t::NO_KEYS, &[])).color(ds.theme.text3));
        }
    }
}

/// What a binding does, in words.
fn action_text(ds: &Ds, action: &BindingAction) -> String {
    let words = match action {
        BindingAction::Op { op, .. } => say(ds, t::ACTION_OP, &[("op", op)]),
        BindingAction::Hold { op, .. } => say(ds, t::ACTION_HOLD, &[("op", op)]),
        BindingAction::Trigger(n) => say(ds, t::ACTION_TRIGGER, &[("n", &(u32::from(*n) + 1).to_string())]),
        BindingAction::Intent(intent) => match intent {
            Intent::Move { .. } => say(ds, t::ACTION_MOVE, &[]),
            Intent::Run => say(ds, t::ACTION_RUN, &[]),
            Intent::Jump => say(ds, t::ACTION_JUMP, &[]),
            Intent::Sit => say(ds, t::ACTION_SIT, &[]),
            Intent::Emote(n) => say(ds, t::ACTION_EMOTE, &[("n", &(u32::from(*n) + 1).to_string())]),
        },
        _ => say(ds, t::ACTION_OTHER, &[]),
    };
    words.into_owned()
}
