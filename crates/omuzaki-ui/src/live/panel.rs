//! [`LivePanel`]: the Live workspace's controls (#305, `shell.md` section 12): character, camera, calibrate,
//! smoothing, hotkeys, the output window and health, top to bottom in the order Tab visits them (SE-02).
//!
//! Every control is a design-system widget with an accessible name. A control whose op is not on main is drawn
//! disabled and says why on hover and on keyboard focus.

use std::sync::{Arc, Mutex, PoisonError};

use egui::{Id, RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType, vec2};
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

/// The expressions the hotkey table lists, in order.
const EXPRESSIONS: [Word; HOTKEY_ROWS] =
    [Word::ExpressionHappy, Word::ExpressionSurprised, Word::ExpressionAngry, Word::ExpressionSad];

/// Width of a hotkey's trigger cell: room for a key chord such as `Ctrl+Shift+F12`.
const TRIGGER_WIDTH: f32 = 140.0;

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
}

impl LivePanel {
    /// The workspace over `runner`'s ops, with the output window the app made (or `None` in a build without one).
    pub fn new(runner: &dyn Runner, output: Option<SharedOutput>) -> Self {
        Self::with_model(LiveModel::new(runner), output)
    }

    /// The workspace showing `model` (the gallery story and tests).
    pub fn with_model(model: LiveModel, output: Option<SharedOutput>) -> Self {
        Self { model, output, refreshed: None }
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
        hotkeys(ui);
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

/// The hotkey table: [`HOTKEY_ROWS`] rows of trigger and action, and add and remove, all waiting on the trigger ops.
fn hotkeys(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    let title = text(&ctx, Word::HotkeysSection);
    let reason = not_yet(&ctx, Ops::hotkeys(), Word::NotYetHotkeys).unwrap_or_default();
    Section::new(&title).default_open(true).id_salt("live.hotkeys").show(ui, |ui| {
        let unbound = text(&ctx, Word::HotkeyUnbound);
        let (trigger, action) = (text(&ctx, Word::HotkeyTrigger), text(&ctx, Word::HotkeyAction));
        egui::Grid::new(Id::new("live.hotkeys.grid")).num_columns(3).spacing(vec2(space::S16, space::S8)).show(
            ui,
            |ui| {
                ui.label(RichText::new(&*trigger).strong());
                ui.label(RichText::new(&*action).strong());
                ui.end_row();
                for expression in EXPRESSIONS {
                    let name = text(&ctx, expression);
                    let key = say(&ctx, Word::HotkeyTriggerOf, &[("action", &name), ("key", &unbound)]);
                    let remove = say(&ctx, Word::HotkeyRemove, &[("action", &name)]);
                    ui.add(
                        Button::new(&unbound).ghost().min_width(TRIGGER_WIDTH).accessible_label(&key).disabled(&reason),
                    );
                    ui.label(&*name);
                    ui.add(Button::new(&remove).icon(icons::TRASH).ghost().disabled(&reason));
                    ui.end_row();
                }
            },
        );
        ui.add(Button::new(&text(&ctx, Word::HotkeyAdd)).icon(icons::PLUS).disabled(&reason));
    });
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
