//! [`LivePanel`]: the Live workspace's controls (#305, `shell.md` section 12): character, camera, calibrate,
//! smoothing, hotkeys, the output window and health, top to bottom in the order Tab visits them (SE-02).
//!
//! Every control is a design-system widget with an accessible name. A control whose op is not on main is drawn
//! disabled and says why on hover and on keyboard focus.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Event, Key, Modifiers, RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType, vec2};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;

use super::model::{Control, HOTKEY_ROWS, LiveModel, OP_CALIBRATE, OP_SET_RETARGET, Ops, SMOOTHING_RANGE};
use super::text::{Word, say, text};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{
    Button, Choice, Dropdown, EmptyState, FormRow, OpCall, ParamEvent, ParamSlider, Section, StatusChip, Tip,
    TipExt as _, Toggle,
};
use crate::output_window::OutputWindow;

/// Seconds between `perform.status` reads while the workspace is shown.
pub const REFRESH_S: f64 = 0.25;

/// The moods the hotkey table offers, in order: the first [`HOTKEY_ROWS`] always have a row, Add hotkey shows the rest.
/// Each is an expression of the starter set (`ExpressionSet::starter`), by its id.
const MOODS: [(&str, Word); 5] = [
    ("happy", Word::ExpressionHappy),
    ("surprised", Word::ExpressionSurprised),
    ("angry", Word::ExpressionAngry),
    ("sad", Word::ExpressionSad),
    ("relaxed", Word::ExpressionRelaxed),
];

/// Width of a hotkey's trigger cell: room for a key chord such as `Ctrl+Shift+F12`.
const TRIGGER_WIDTH: f32 = 140.0;
/// Width of a hotkey's action cell: room for the longest mood's name.
const ACTION_WIDTH: f32 = 120.0;

/// The one character V0 offers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Character {
    Kit,
}

/// The output window the toggle opens, shared with the app's frame loop, which shows it.
pub type SharedOutput = Arc<Mutex<OutputWindow>>;

/// The Live workspace.
#[derive(Debug)]
pub struct LivePanel {
    model: LiveModel,
    output: Option<SharedOutput>,
    /// The `ctx` time of the last status read.
    refreshed: Option<f64>,
    /// The expression whose trigger button waits for a key press.
    capturing: Option<String>,
    /// Mood rows shown: [`HOTKEY_ROWS`], and more after Add hotkey.
    moods: usize,
}

impl LivePanel {
    /// The workspace over `runner`'s ops, with the output window the app made (or `None` in a build without one).
    pub fn new(runner: &dyn Runner, output: Option<SharedOutput>) -> Self {
        Self::with_model(LiveModel::new(runner), output)
    }

    /// The workspace showing `model` (the gallery story and tests).
    pub fn with_model(model: LiveModel, output: Option<SharedOutput>) -> Self {
        Self { model, output, refreshed: None, capturing: None, moods: HOTKEY_ROWS }
    }

    /// The model.
    pub fn model(&self) -> &LiveModel {
        &self.model
    }

    /// The model, to change what it shows (the story's states).
    pub fn model_mut(&mut self) -> &mut LiveModel {
        &mut self.model
    }

    /// Draws the workspace; ops run through `runner` as `actor`. Reads the status every [`REFRESH_S`].
    pub fn ui(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let scroll = ScrollArea::vertical().id_salt("live.scroll").auto_shrink([false, false]).animated(false);
        scroll.show(ui, |ui| self.body(ui, runner, actor));
    }

    /// Draws the workspace at its natural height, without a scroll area of its own: for a host that scrolls it (the
    /// gallery). Reads the status every [`REFRESH_S`] as [`ui`](Self::ui) does.
    pub fn body(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let now = ui.input(|i| i.time);
        if self.refreshed.is_none_or(|at| now - at >= REFRESH_S) {
            self.model.refresh(runner, actor);
            self.refreshed = Some(now);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_S));
        ui.spacing_mut().item_spacing.y = space::S8;
        // Health and the output first: what a person checks before going live stays in view on a small screen.
        self.health(ui);
        self.failure(ui);
        self.output(ui);
        self.character(ui, runner, actor);
        camera(ui);
        self.calibrate(ui, runner, actor);
        self.smoothing(ui, runner, actor);
        self.hotkeys(ui, runner, actor);
    }

    fn character(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::CharacterSection);
        Section::new(&title).default_open(true).id_salt("live.character").show(ui, |ui| {
            let kit = text(&ctx, Word::CharacterKit);
            let choices = [Choice::new(Character::Kit, &kit)];
            let mut chosen = Character::Kit;
            let label = text(&ctx, Word::CharacterLabel);
            FormRow::new(&label).show(ui, |ui, name| {
                ui.add(Dropdown::new(name, &mut chosen, &choices).id_salt("live.character.pick").stretch())
            });
            // "Drop a VRM" exists only where the import op does: absent, not broken.
            if self.model.ops().import() == Control::Ready {
                let dropped = drop_zone(ui, &text(&ctx, Word::DropVrm));
                if let Some(path) = dropped {
                    self.model.import(runner, actor, &path);
                }
                if let Some(name) = self.model.imported() {
                    ui.add(StatusChip::new(Status::Ok, &say(&ctx, Word::Imported, &[("name", name)])));
                }
            }
        });
    }
}

/// The camera picker, its preview and the no-camera state, all waiting on the input ops.
fn camera(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let title = text(&ctx, Word::CameraSection);
    Section::new(&title).default_open(true).id_salt("live.camera").show(ui, |ui| {
        let reason = not_yet(&ctx, Ops::camera(), Word::NotYetCamera);
        let none = text(&ctx, Word::CameraNone);
        let choices = [Choice::new(0_u8, &none)];
        let mut chosen = 0_u8;
        let label = text(&ctx, Word::CameraLabel);
        FormRow::new(&label).show(ui, |ui, name| {
            let dropdown = Dropdown::new(name, &mut chosen, &choices).id_salt("live.camera.pick").stretch();
            ui.add(match reason.as_deref() {
                Some(reason) => dropdown.disabled(reason),
                None => dropdown,
            })
        });
        // No camera is bound until the input ops land: the empty state of shell.md section 13.
        EmptyState::new(icons::EYE_SLASH, &text(&ctx, Word::NoCameraHeadline), &text(&ctx, Word::NoCameraLine))
            .show(ui);
        let choose = text(&ctx, Word::ChooseCamera);
        let button = Button::new(&choose).primary();
        ui.add(match reason.as_deref() {
            Some(reason) => button.disabled(reason),
            None => button,
        });
        let preview = text(&ctx, Word::CameraPreview);
        let mut mirrored = true;
        let toggle = Toggle::checkbox(&preview, &mut mirrored);
        ui.add(match reason.as_deref() {
            Some(reason) => toggle.disabled(reason),
            None => toggle,
        });
    });
}

impl LivePanel {
    fn calibrate(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::CalibrateSection);
        Section::new(&title).default_open(true).id_salt("live.calibrate").show(ui, |ui| {
            ui.label(text(&ctx, Word::CalibratePrompt));
            let label = text(&ctx, Word::Calibrate);
            let missing = missing_reason(&ctx, self.model.ops().calibrate());
            let (status, line) = calibration_line(&ctx, self.model.calibration());
            // While a capture runs the button says why it waits: the capture's own progress line.
            let button = Button::new(&label).primary().op(OP_CALIBRATE).disabled_if(self.model.calibrating(), &line);
            let button = match missing.as_deref() {
                Some(reason) => button.disabled(reason),
                None => button,
            };
            if ui.add(button).clicked() {
                self.model.calibrate(runner, actor);
            }
            ui.add(StatusChip::new(status, &line));
        });
    }

    fn smoothing(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::SmoothingSection);
        Section::new(&title).default_open(true).id_salt("live.smoothing").show(ui, |ui| {
            let label = text(&ctx, Word::Smoothing);
            let missing = missing_reason(&ctx, self.model.ops().smoothing());
            let slider = ParamSlider::new(&label, self.model.smoothing_mut(), SMOOTHING_RANGE)
                .default_value(0.0)
                .step(0.05)
                .decimals(2)
                .op_call(OpCall { op: OP_SET_RETARGET, target: "" })
                .id_salt("live.smoothing.slider");
            let out = match missing.as_deref() {
                Some(reason) => slider.disabled(reason).show(ui),
                None => slider.show(ui),
            };
            if out.events.iter().any(|event| matches!(event, ParamEvent::Commit { .. })) {
                self.model.commit_smoothing(runner, actor);
            }
            ui.label(RichText::new(text(&ctx, Word::SmoothingCost)).font(ds(&ctx).text.small.clone()));
        });
    }

    fn output(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::OutputSection);
        Section::new(&title).default_open(true).id_salt("live.output").show(ui, |ui| {
            let label = text(&ctx, Word::OutputWindow);
            let Some(output) = &self.output else {
                let mut off = false;
                let reason = text(&ctx, Word::OutputUnavailable);
                ui.add(Toggle::switch(&label, &mut off).disabled(&reason));
                return;
            };
            let mut window = output.lock().unwrap_or_else(PoisonError::into_inner);
            let mut open = window.status().open;
            if ui.add(Toggle::switch(&label, &mut open)).changed() {
                window.set_open(open);
            }
            let status = window.status();
            let size = window.plan().size;
            let line = say(
                &ctx,
                Word::OutputSize,
                &[("width", &size.width.to_string()), ("height", &size.height.to_string())],
            );
            ui.label(line);
            if status.fallback {
                ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::OutputFallback)));
            }
            if status.open && status.minimised {
                ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::OutputMinimised)));
            }
        });
    }

    /// The health strip: tracking state, frame rate and latency on one line (nothing in it takes the keyboard).
    fn health(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = space::S16;
            ui.label(RichText::new(text(&ctx, Word::HealthSection)).strong());
            let (status, word) = match self.model.tracking() {
                None => (Status::Info, Word::HealthUnknown),
                Some(Tracking::NoFrames) => (Status::Info, Word::HealthNoFrames),
                Some(Tracking::Tracking) => (Status::Ok, Word::HealthTracking),
                Some(Tracking::Lost) => (Status::Warn, Word::HealthLost),
            };
            ui.add(StatusChip::new(status, &text(&ctx, word)));
            if let Some(status) = self.model.status() {
                let fps = format!("{:.0}", status.source_fps);
                ui.label(say(&ctx, Word::HealthFps, &[("fps", &fps)]));
            }
            let latency = text(&ctx, Word::HealthLatency);
            let reason = text(&ctx, Word::NotYetLatency);
            // The tip only adds detail to a plain label; nothing reads its response.
            let _ = ui.label(format!("{latency}: —")).tip(Tip::new(&latency).detail(&reason));
        });
    }

    fn failure(&self, ui: &mut Ui) {
        if let Some(failure) = self.model.failure() {
            let line = format!("{}: {}", failure.op, failure.message);
            ui.add(StatusChip::new(Status::Danger, &line));
        }
    }
}

impl LivePanel {
    /// The hotkey table: a row per mood (at least [`HOTKEY_ROWS`]) and per other bound expression, each with its key and
    /// a remove button. A trigger button waits for the next key press (with its modifiers) and binds it; Escape or Tab
    /// cancels.
    fn hotkeys(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::HotkeysSection);
        let missing = missing_reason(&ctx, self.model.ops().hotkeys());
        if missing.is_some() {
            self.capturing = None;
        }
        if let Some(key) = self.captured_key(ui)
            && let Some(expression) = self.capturing.take()
        {
            self.model.bind_hotkey(runner, actor, &key, &expression);
        }
        Section::new(&title).default_open(true).id_salt("live.hotkeys").show(ui, |ui| {
            let (trigger, action) = (text(&ctx, Word::HotkeyTrigger), text(&ctx, Word::HotkeyAction));
            // Rows of fixed-width cells, not a grid: a grid sizes its columns from the last frame, which would cut the
            // buttons' labels on the first.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S16;
                cell(ui, TRIGGER_WIDTH, |ui| ui.label(RichText::new(&*trigger).strong()));
                cell(ui, ACTION_WIDTH, |ui| ui.label(RichText::new(&*action).strong()));
            });
            for (expression, name) in self.rows(&ctx) {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S16;
                    self.hotkey_row(ui, runner, actor, &expression, &name, missing.as_deref());
                });
            }
            let (label, every) = (text(&ctx, Word::HotkeyAdd), text(&ctx, Word::HotkeyEveryMood));
            let add = Button::new(&label).icon(icons::PLUS);
            let add = match missing.as_deref() {
                Some(reason) => add.disabled(reason),
                None => add.disabled_if(self.moods >= MOODS.len(), &every),
            };
            if ui.add(add).clicked() {
                self.moods = (self.moods + 1).min(MOODS.len());
            }
        });
    }

    /// The rows: the moods shown, then every other bound expression (bound by an agent, say), by its id.
    fn rows(&self, ctx: &egui::Context) -> Vec<(String, String)> {
        let moods = MOODS.iter().take(self.moods).map(|(id, word)| ((*id).to_owned(), text(ctx, *word).into_owned()));
        let others = self
            .model
            .triggers()
            .iter()
            .map(|info| info.binding.expression.as_str())
            .filter(|id| !MOODS.iter().take(self.moods).any(|(mood, _)| mood == id))
            .map(|id| (id.to_owned(), id.to_owned()));
        moods.chain(others).collect()
    }

    fn hotkey_row(
        &mut self,
        ui: &mut Ui,
        runner: &dyn Runner,
        actor: &Actor,
        expression: &str,
        name: &str,
        missing: Option<&str>,
    ) {
        let ctx = ui.ctx().clone();
        let bound = self.model.trigger_for(expression).cloned();
        let capturing = self.capturing.as_deref() == Some(expression);
        let unbound = text(&ctx, Word::HotkeyUnbound);
        let key = bound.as_ref().map_or(&*unbound, |info| info.binding.key.as_str());
        let (shown, label) = if capturing {
            let press = say(&ctx, Word::HotkeyPress, &[("action", name)]);
            (text(&ctx, Word::HotkeyWaiting).into_owned(), press)
        } else {
            (key.to_owned(), say(&ctx, Word::HotkeyTriggerOf, &[("action", name), ("key", key)]))
        };
        let button = Button::new(&shown).ghost().min_width(TRIGGER_WIDTH).accessible_label(&label);
        let button = match missing {
            Some(reason) => button.disabled(reason),
            None => button,
        };
        let response = cell(ui, TRIGGER_WIDTH, |ui| ui.add(button));
        if response.clicked() {
            self.capturing = if capturing { None } else { Some(expression.to_owned()) };
        }
        if capturing && response.lost_focus() {
            // Focus moved elsewhere (Tab, a click): the wait ends without a bind.
            self.capturing = None;
        }
        cell(ui, ACTION_WIDTH, |ui| ui.label(name));
        let short = text(&ctx, Word::HotkeyRemoveShort);
        let remove = say(&ctx, Word::HotkeyRemove, &[("action", name)]);
        let nothing = text(&ctx, Word::HotkeyNothingBound);
        let button = Button::new(&short).icon(icons::TRASH).ghost().accessible_label(&remove);
        let button = match (missing, &bound) {
            (Some(reason), _) => button.disabled(reason),
            (None, None) => button.disabled(&nothing),
            (None, Some(_)) => button,
        };
        if ui.add(button).clicked()
            && let Some(info) = &bound
        {
            self.model.unbind_hotkey(runner, actor, &info.binding.key);
        }
        match &bound {
            Some(info) if info.missing => {
                ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::HotkeyMissing)));
            }
            Some(info) if info.active => {
                ui.add(StatusChip::new(Status::Ok, &text(&ctx, Word::HotkeyShowing)));
            }
            _ => {}
        }
    }

    /// While a trigger button waits: the key pressed this frame, as the chord to bind, taken from the input so nothing
    /// else acts on it. Escape ends the wait without a bind; Tab is left alone and moves focus, which ends it too.
    fn captured_key(&mut self, ui: &Ui) -> Option<String> {
        self.capturing.as_ref()?;
        let (key, modifiers) = ui.input(|i| {
            i.events.iter().find_map(|event| match event {
                Event::Key { key, pressed: true, repeat: false, modifiers, .. } if *key != Key::Tab => {
                    Some((*key, *modifiers))
                }
                _ => None,
            })
        })?;
        ui.ctx().input_mut(|i| i.consume_key(modifiers, key));
        if key == Key::Escape {
            self.capturing = None;
            return None;
        }
        Some(chord(modifiers, key))
    }
}

/// A cell `width` wide in a row, holding what `add` adds at its left.
fn cell<R>(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui) -> R) -> R {
    let size = vec2(width, ui.spacing().interact_size.y);
    let layout = egui::Layout::left_to_right(egui::Align::Center);
    ui.allocate_ui_with_layout(size, layout, |ui| {
        ui.set_min_width(width);
        add(ui)
    })
    .inner
}

/// The name a key press is bound under: its modifiers, then the key, as `Ctrl+Shift+F12`.
fn chord(modifiers: Modifiers, key: Key) -> String {
    let mut name = String::new();
    for (held, word) in
        [(modifiers.ctrl, "Ctrl"), (modifiers.alt, "Alt"), (modifiers.shift, "Shift"), (modifiers.mac_cmd, "Cmd")]
    {
        if held {
            name.push_str(word);
            name.push('+');
        }
    }
    name.push_str(key.name());
    name
}

/// A target that takes a dropped file; returns its path when one is dropped while the pointer is over it.
fn drop_zone(ui: &mut Ui, label: &str) -> Option<String> {
    let ds = ds(ui.ctx());
    let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 56.0), Sense::hover());
    let hovering = ui.ctx().input(|i| !i.raw.hovered_files.is_empty());
    let stroke = if hovering { ds.theme.accent } else { ds.theme.line };
    ui.painter().rect_stroke(rect, 6.0, egui::Stroke::new(1.0, stroke), egui::StrokeKind::Inside);
    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, label, ds.text.body.clone(), ds.theme.text2);
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, label), |_| {});
    let over = response.contains_pointer();
    ui.ctx().input(|i| i.raw.dropped_files.first().filter(|_| over).map(|file| file.path().display().to_string()))
}

/// Why a control whose op is missing from this build is disabled.
fn missing_reason(ctx: &egui::Context, control: Control) -> Option<String> {
    match control {
        Control::Ready => None,
        Control::Missing(op) => Some(say(ctx, Word::NoOp, &[("op", op)])),
        Control::NotYet => Some(text(ctx, Word::NotYet).into_owned()),
    }
}

/// The "not available yet" reason for a control waiting on ops that aren't on main.
fn not_yet(ctx: &egui::Context, control: Control, why: Word) -> Option<String> {
    match control {
        Control::NotYet => Some(text(ctx, why).into_owned()),
        other => missing_reason(ctx, other),
    }
}

/// The calibration chip: its status and line.
fn calibration_line(ctx: &egui::Context, state: Option<CalibrationState>) -> (Status, String) {
    match state {
        None | Some(CalibrationState::Idle) => (Status::Info, text(ctx, Word::CalibrateIdle).into_owned()),
        Some(CalibrationState::Capturing { frames, of }) => {
            (Status::Info, say(ctx, Word::Calibrating, &[("frames", &frames.to_string()), ("of", &of.to_string())]))
        }
        Some(CalibrationState::Done { frames }) => {
            (Status::Ok, say(ctx, Word::CalibrateDone, &[("frames", &frames.to_string())]))
        }
        Some(CalibrationState::Failed { got, need }) => {
            (Status::Warn, say(ctx, Word::CalibrateFailed, &[("got", &got.to_string()), ("need", &need.to_string())]))
        }
    }
}
