//! [`LivePanel`]: the Live workspace's controls (#305, `shell.md` section 12): character, camera, calibrate,
//! smoothing, hotkeys, the output window and health, top to bottom in the order Tab visits them (SE-02).
//!
//! Every control is a design-system widget with an accessible name. A control whose op is not on main is drawn
//! disabled and says why on hover and on keyboard focus.

use std::sync::Arc;

use egui::{Event, Key, RichText, ScrollArea, Sense, Ui, WidgetInfo, WidgetType, vec2};
use omuzaki_metrics_api::MetricsReader;
use omuzaki_metrics_api::sections::TrackingState;
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;
use omuzaki_stream_api::window::{Backdrop as Shown, DEFAULT_TITLE, WindowState};

use super::hotkeys::chord;
use super::latency::LatencyMeter;
use super::model::{
    Backdrop, Clicks, Control, HOTKEY_ROWS, LiveModel, OP_CALIBRATE, OP_SET_RETARGET, OP_WINDOW_SET, OPACITY_RANGE,
    Place, SMOOTHING_RANGE, WindowChoice,
};
use super::text::{Word, say, text};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{
    Button, Choice, Dropdown, EmptyState, OpCall, ParamEvent, ParamSlider, Section, Segment, Segmented, StatusChip,
    Tip, TipExt as _, Toggle,
};
use omuzaki_stage::perform::input::{InputState, Permission};

/// Seconds between `perform.status` reads while the workspace is shown.
pub const REFRESH_S: f64 = 0.25;

/// The moods the hotkey table offers, in order: the first [`HOTKEY_ROWS`] always have a row, Add hotkey shows the rest.
/// Each is an expression of the starter set (`ExpressionSet::starter`), by its id.
pub(super) const MOODS: [(&str, Word); 5] = [
    ("happy", Word::ExpressionHappy),
    ("surprised", Word::ExpressionSurprised),
    ("angry", Word::ExpressionAngry),
    ("sad", Word::ExpressionSad),
    ("relaxed", Word::ExpressionRelaxed),
];

/// The widest a hotkey's trigger cell gets: room for a key chord such as `Ctrl+Shift+F12`. A longer chord, or one
/// wider than [`TRIGGER_SHARE`] of a narrow dock, ends in an ellipsis (the tooltip has it whole).
const TRIGGER_WIDTH: f32 = 140.0;
/// The narrowest a hotkey's trigger cell gets: a function key and its padding.
const TRIGGER_MIN: f32 = 72.0;
/// The most of the row the trigger cell takes.
const TRIGGER_SHARE: f32 = 0.45;
/// The narrowest a hotkey's action cell gets; a longer name ends in an ellipsis.
const ACTION_MIN: f32 = 56.0;
/// The status icon before a hotkey's action, in points.
const ICON_CELL: f32 = 14.0;

/// The one character V0 offers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Character {
    Kit,
}

/// The Live workspace.
#[derive(Debug)]
pub struct LivePanel {
    model: LiveModel,
    /// The `ctx` time of the last status read.
    refreshed: Option<f64>,
    /// The expression whose trigger button waits for a key press.
    capturing: Option<String>,
    /// Mood rows shown: [`HOTKEY_ROWS`], and more after Add hotkey.
    moods: usize,
    /// The camera-to-output latency the render side measures, read when there is no live-metrics snapshot.
    latency: Arc<LatencyMeter>,
    /// The app's live-metrics snapshot: the health strip's source once the shell has one (#1348).
    metrics: Option<MetricsReader>,
    /// The desktop opacity slider's value, in percent, while it is dragged; sent when it settles.
    opacity: f64,
}

impl LivePanel {
    /// The workspace over `runner`'s ops.
    pub fn new(runner: &dyn Runner) -> Self {
        Self::with_model(LiveModel::new(runner))
    }

    /// The workspace reading its latency from `meter` instead of the app's shared one (tests).
    #[must_use]
    pub fn with_latency(mut self, meter: Arc<LatencyMeter>) -> Self {
        self.latency = meter;
        self
    }

    /// The workspace showing `model` (the gallery story and tests).
    pub fn with_model(model: LiveModel) -> Self {
        let opacity = f64::from(model.window_choice().opacity);
        let latency = LatencyMeter::shared();
        Self { model, refreshed: None, capturing: None, moods: HOTKEY_ROWS, latency, metrics: None, opacity }
    }

    /// Reads the health strip from `reader` from now on: tracking, the camera's rate and the latency.
    pub fn set_metrics(&mut self, reader: MetricsReader) {
        self.metrics = Some(reader);
    }

    /// The snapshot the health strip reads, once one is set.
    pub fn metrics(&self) -> Option<&MetricsReader> {
        self.metrics.as_ref()
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
        self.output(ui, runner, actor);
        self.character(ui, runner, actor);
        self.camera(ui, runner, actor);
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
            // The section's title is the label a person reads; the dropdown carries it as its accessible name and takes
            // the dock's whole width, which a narrow dock needs for a camera's or a character's name.
            let label = text(&ctx, Word::CharacterLabel);
            ui.add(Dropdown::new(&label, &mut chosen, &choices).id_salt("live.character.pick").stretch());
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

impl LivePanel {
    /// The camera picker, the tracking switch and what the input says (no camera, unplugged, no permission), all
    /// through the `input.*` ops. Without them the section is drawn disabled, "not available yet".
    fn camera(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::CameraSection);
        let reason = missing_reason(&ctx, self.model.ops().camera());
        Section::new(&title).default_open(true).id_salt("live.camera").show(ui, |ui| {
            let view = self.model.input().cloned();
            let cameras = view.as_ref().map_or(&[][..], |view| &view.cameras[..]);
            // A bound camera that is unplugged is no longer listed; it keeps a row of its own, so the picker still
            // says which camera the input waits for.
            let gone = view
                .as_ref()
                .and_then(|view| view.camera.as_ref().map(|id| id.0.as_str()))
                .filter(|id| !cameras.iter().any(|camera| camera.id.0 == *id))
                .map(|id| say(&ctx, Word::CameraGone, &[("camera", id)]));
            let none = text(&ctx, Word::CameraNone);
            let mut choices = vec![Choice::new(None, &none)];
            choices.extend(cameras.iter().enumerate().map(|(i, camera)| Choice::new(Some(i), &camera.name)));
            choices.extend(gone.as_deref().map(|label| Choice::new(Some(cameras.len()), label)));
            let bound = cameras.iter().position(|camera| camera.bound).or(gone.as_ref().map(|_| cameras.len()));
            let mut chosen = bound;
            let label = text(&ctx, Word::CameraLabel);
            let dropdown = Dropdown::new(&label, &mut chosen, &choices).id_salt("live.camera.pick").stretch();
            ui.add(disable(dropdown, reason.as_deref(), Dropdown::disabled));
            match chosen {
                _ if chosen == bound => {}
                None => self.model.bind_camera(runner, actor, None),
                Some(i) => {
                    if let Some(camera) = cameras.get(i) {
                        self.model.bind_camera(runner, actor, Some(&camera.id.0));
                    }
                }
            }
            if cameras.is_empty() {
                // The empty state of shell.md section 13: the life layer has the face until a camera tracks it.
                EmptyState::new(icons::EYE_SLASH, &text(&ctx, Word::NoCameraHeadline), &text(&ctx, Word::NoCameraLine))
                    .show(ui);
            }
            let capturing = view.as_ref().is_some_and(|view| view.preview.is_on());
            let mut on = capturing;
            let track = text(&ctx, Word::CameraTrack);
            let unbound = text(&ctx, Word::CameraChooseFirst);
            let toggle = Toggle::switch(&track, &mut on).id_salt("live.camera.track");
            let toggle = match reason.as_deref() {
                Some(reason) => toggle.disabled(reason),
                None if bound.is_none() && !capturing => toggle.disabled(&unbound),
                None => toggle,
            };
            if ui.add(toggle).changed() {
                self.model.set_tracking(runner, actor, on);
            }
            if let Some(view) = &view {
                self.camera_state(ui, runner, actor, view);
            }
        });
    }

    /// The input's state under the switch: what is tracked, an unplugged camera with Retry, or the permission.
    fn camera_state(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor, view: &InputState) {
        let ctx = ui.ctx().clone();
        match view.permission {
            Permission::Denied => {
                ui.add(StatusChip::new(Status::Danger, &text(&ctx, Word::CameraDenied)));
                return;
            }
            Permission::Unknown if self.model.ops().request_camera() == Control::Ready => {
                let allow = text(&ctx, Word::CameraAllow);
                if ui.add(Button::new(&allow)).clicked() {
                    self.model.request_camera(runner, actor);
                }
            }
            Permission::Unknown | Permission::Granted => {}
        }
        if view.camera.is_some() && !view.camera_present {
            ui.horizontal_wrapped(|ui| {
                ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::CameraUnplugged)));
                let retry = text(&ctx, Word::CameraRetry);
                if ui.add(Button::new(&retry).icon(icons::ARROW_COUNTER_CLOCKWISE)).clicked() {
                    self.model.set_tracking(runner, actor, true);
                }
            });
        } else if let (true, Some(camera)) = (view.preview.is_on(), view.cameras.iter().find(|camera| camera.bound)) {
            ui.add(StatusChip::new(Status::Ok, &say(&ctx, Word::CameraTracking, &[("name", &camera.name)])));
        }
    }
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

    /// The output window: its switch, its backdrop and stacking, and what it is (size, fallback, minimised). Every
    /// change runs an `output.window.*` op, so an agent sees and does the same.
    fn output(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor) {
        let ctx = ui.ctx().clone();
        let title = text(&ctx, Word::OutputSection);
        let missing = missing_reason(&ctx, self.model.ops().output());
        Section::new(&title).default_open(true).id_salt("live.output").show(ui, |ui| {
            let label = text(&ctx, Word::OutputWindow);
            let mut open = self.model.window_open();
            let switch = Toggle::switch(&label, &mut open).id_salt("live.output.open");
            if ui.add(disable(switch, missing.as_deref(), Toggle::disabled)).changed() {
                self.model.set_window_open(runner, actor, open);
            }
            let mut choice = self.model.window_choice();
            let (capture, desktop) = (text(&ctx, Word::OutputCapture), text(&ctx, Word::OutputDesktop));
            let places = [Segment::new(Place::Capture, &capture), Segment::new(Place::Desktop, &desktop)];
            let place = text(&ctx, Word::OutputPlace);
            let segmented = Segmented::new(&place, &mut choice.place, &places).id_salt("live.output.place").stretch();
            ui.add(disable(segmented, missing.as_deref(), Segmented::disabled));
            if let Some(reason) = self.model.unavailable(Place::Desktop) {
                // The host's own reason is detail for the curious; the line is the studio's words.
                let line = RichText::new(text(&ctx, Word::OutputNoDesktop)).font(ds(&ctx).text.small.clone());
                ui.label(line.color(ds(&ctx).theme.text2)).on_hover_text(reason);
            }
            match choice.place {
                Place::Capture => Self::capture_controls(ui, &mut choice, missing.as_deref()),
                Place::Desktop => self.desktop_controls(ui, &mut choice, missing.as_deref()),
            }
            if choice != self.model.window_choice() {
                self.model.set_window_choice(runner, actor, choice);
            }
            self.output_state(ui, choice);
        });
    }

    /// The capture window's backdrop and stacking.
    fn capture_controls(ui: &mut Ui, choice: &mut WindowChoice, missing: Option<&str>) {
        let ctx = ui.ctx().clone();
        let (key, clear) = (text(&ctx, Word::OutputKey), text(&ctx, Word::OutputTransparent));
        let segments = [Segment::new(Backdrop::Key, &key), Segment::new(Backdrop::Transparent, &clear)];
        // Its two options name themselves; the whole width keeps both whole in a narrow dock.
        let backdrop = text(&ctx, Word::OutputBackdrop);
        let segmented =
            Segmented::new(&backdrop, &mut choice.backdrop, &segments).id_salt("live.output.backdrop").stretch();
        ui.add(disable(segmented, missing, Segmented::disabled));
        let on_top = text(&ctx, Word::OutputOnTop);
        let toggle = Toggle::switch(&on_top, &mut choice.on_top).id_salt("live.output.on_top");
        ui.add(disable(toggle, missing, Toggle::disabled));
    }

    /// The desktop window's opacity and which clicks pass through it. The opacity is sent when the slider settles, so
    /// a drag is one change, not one per frame.
    fn desktop_controls(&mut self, ui: &mut Ui, choice: &mut WindowChoice, missing: Option<&str>) {
        let ctx = ui.ctx().clone();
        let label = text(&ctx, Word::OutputOpacity);
        let slider = ParamSlider::new(&label, &mut self.opacity, OPACITY_RANGE)
            .default_value(100.0)
            .step(5.0)
            .decimals(0)
            .op_call(OpCall { op: OP_WINDOW_SET, target: "" })
            .id_salt("live.output.opacity");
        let out = match missing {
            Some(reason) => slider.disabled(reason).show(ui),
            None => slider.show(ui),
        };
        if out.events.iter().any(|event| matches!(event, ParamEvent::Commit { .. })) {
            #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to 10..=100 first
            let percent = self.opacity.clamp(*OPACITY_RANGE.start(), *OPACITY_RANGE.end()).round() as u8;
            choice.opacity = percent;
        }
        let (off, empty, all) =
            (text(&ctx, Word::ClicksOff), text(&ctx, Word::ClicksEmpty), text(&ctx, Word::ClicksAll));
        let segments =
            [Segment::new(Clicks::Off, &off), Segment::new(Clicks::Empty, &empty), Segment::new(Clicks::All, &all)];
        let clicks = text(&ctx, Word::OutputClicks);
        let segmented = Segmented::new(&clicks, &mut choice.clicks, &segments).id_salt("live.output.clicks").stretch();
        ui.add(disable(segmented, missing, Segmented::disabled));
        if choice.clicks == Clicks::All {
            ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::ClicksAllNote)));
        }
    }

    /// What the output window is: its size and backdrop when open, where to find it when closed, and the two states a
    /// capture can't use (a fallback from transparency, a minimised window).
    fn output_state(&self, ui: &mut Ui, choice: WindowChoice) {
        let ctx = ui.ctx().clone();
        let small = ds(&ctx).text.small.clone();
        // Closed, not read yet, or a state this build doesn't know: where to find the window once it opens.
        let Some(WindowState::Open { physical, backdrop, minimised, desktop, .. }) = self.model.window() else {
            let hint = match choice.place {
                Place::Capture => say(&ctx, Word::OutputHint, &[("title", DEFAULT_TITLE)]),
                Place::Desktop => text(&ctx, Word::OutputDesktopHint).into_owned(),
            };
            ui.label(RichText::new(hint).font(small));
            return;
        };
        let (width, height) = (physical.x.to_string(), physical.y.to_string());
        let size = say(&ctx, Word::OutputSize, &[("width", &width), ("height", &height)]);
        let line = match (desktop, backdrop) {
            (Some(desktop), _) => {
                let percent = format!("{:.0}", desktop.opacity * 100.0);
                say(&ctx, Word::OutputOnDesktop, &[("size", &size), ("opacity", &percent)])
            }
            (None, backdrop) => match backdrop {
                Shown::Key(key) => say(&ctx, Word::OutputOverKey, &[("size", &size), ("key", &key.to_string())]),
                Shown::Transparent => say(&ctx, Word::OutputOverNothing, &[("size", &size)]),
                // A backdrop newer than this panel: the size alone.
                _ => size,
            },
        };
        ui.label(RichText::new(line).font(small));
        if self.model.window_fallback() && choice.backdrop == Backdrop::Transparent {
            ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::OutputFallback)));
        }
        if minimised {
            ui.add(StatusChip::new(Status::Warn, &text(&ctx, Word::OutputMinimised)));
        }
    }

    /// The health strip: tracking state, frame rate and latency on one line (nothing in it takes the keyboard). It reads
    /// the live-metrics snapshot when the shell has one, and the performer's status and the latency meter otherwise.
    fn health(&self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let snapshot = self.metrics.as_ref().map(MetricsReader::read);
        let perform = snapshot.as_ref().and_then(|live| live.perform);
        let tracking = match perform.map(|p| p.tracking) {
            Some(TrackingState::Tracking) => Some(Tracking::Tracking),
            Some(TrackingState::Lost) => Some(Tracking::Lost),
            Some(_) => Some(Tracking::NoFrames),
            None => self.model.tracking(),
        };
        // Wrapped: in a narrow dock the frame rate and latency move to a second line rather than widen the panel.
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = space::S16;
            ui.label(RichText::new(text(&ctx, Word::HealthSection)).strong());
            let (status, word) = match tracking {
                None => (Status::Info, Word::HealthUnknown),
                Some(Tracking::NoFrames) => (Status::Info, Word::HealthNoFrames),
                Some(Tracking::Tracking) => (Status::Ok, Word::HealthTracking),
                Some(Tracking::Lost) => (Status::Warn, Word::HealthLost),
            };
            ui.add(StatusChip::new(status, &text(&ctx, word)));
            // Each reading stays on one line: a wrap moves it whole, never splits "29" from "fps".
            let reading =
                |ui: &mut Ui, line: String| ui.add(egui::Label::new(line).wrap_mode(egui::TextWrapMode::Extend));
            if let Some(source_fps) =
                perform.map(|p| p.source_fps).or_else(|| self.model.status().map(|s| s.source_fps))
            {
                let fps = format!("{source_fps:.0}");
                reading(ui, say(&ctx, Word::HealthFps, &[("fps", &fps)]));
            }
            let latency = text(&ctx, Word::HealthLatency);
            // The snapshot's latency while a tracked face is in use, else the meter's; a dash until the first frame has
            // been through to the picture.
            let measured = match snapshot.as_ref().and_then(|snapshot| snapshot.frame) {
                Some(frame) => frame.track_to_submit_ms.map(|ms| f64::from(ms) / 1000.0),
                None => self.latency.reading().map(|d| d.as_secs_f64()),
            };
            let (line, detail) = match measured {
                Some(secs) => {
                    let ms = format!("{:.0}", secs * 1000.0);
                    (say(&ctx, Word::HealthLatencyValue, &[("ms", &ms)]), text(&ctx, Word::LatencyDetail))
                }
                None => (format!("{latency}: —"), text(&ctx, Word::LatencyUnknown)),
            };
            // The tip only adds detail to a plain label; nothing reads its response.
            let _ = reading(ui, line).tip(Tip::new(&latency).detail(&detail));
        });
        if tracking == Some(Tracking::Lost) {
            ui.label(RichText::new(text(&ctx, Word::HealthLostLine)).font(ds(&ctx).text.small.clone()));
        }
    }

    fn failure(&self, ui: &mut Ui) {
        if let Some(failure) = self.model.failure() {
            let line = say(ui.ctx(), Word::OpFailed, &[("op", failure.op), ("reason", &failure.message)]);
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
            let rows = self.rows(&ctx);
            let keys: Vec<String> = rows.iter().map(|(expression, _)| self.trigger_text(&ctx, expression)).collect();
            let columns = Columns::fit(ui, &keys);
            // Rows of cells sized from this frame's width, not a grid: a grid sizes its columns from the last frame,
            // which would cut the buttons' labels on the first, and a fixed width would push a narrow dock wider.
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = space::S8;
                cell(ui, columns.trigger, |ui| ui.label(RichText::new(&*trigger).strong()));
                cell(ui, columns.action, |ui| ui.label(RichText::new(&*action).strong()));
            });
            for (expression, name) in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = space::S8;
                    self.hotkey_row(ui, runner, actor, (&expression, &name), columns, missing.as_deref());
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
        (expression, name): (&str, &str),
        columns: Columns,
        missing: Option<&str>,
    ) {
        let ctx = ui.ctx().clone();
        let bound = self.model.trigger_for(expression).cloned();
        let capturing = self.capturing.as_deref() == Some(expression);
        let unbound = text(&ctx, Word::HotkeyUnbound);
        let key = bound.as_ref().map_or(&*unbound, |info| info.binding.key.as_str());
        let shown = self.trigger_text(&ctx, expression);
        let label = if capturing {
            say(&ctx, Word::HotkeyPress, &[("action", name)])
        } else {
            say(&ctx, Word::HotkeyTriggerOf, &[("action", name), ("key", key)])
        };
        let button = Button::new(&shown).ghost().accessible_label(&label);
        let button = match missing {
            Some(reason) => button.disabled(reason),
            None => button,
        };
        // Exactly the column's width: a chord too long for it ends in an ellipsis rather than widening the dock.
        let (_, rect) = ui.allocate_space(vec2(columns.trigger, ui.spacing().interact_size.y));
        let response = button.show_at(ui, egui::Id::new(("live.hotkey", expression)), rect);
        if response.clicked() {
            self.capturing = if capturing { None } else { Some(expression.to_owned()) };
        }
        if capturing && response.lost_focus() {
            // Focus moved elsewhere (Tab, a click): the wait ends without a bind.
            self.capturing = None;
        }
        // The row's state sits on its action, so a narrow dock keeps each row on one line: a showing expression gets
        // the Ok icon and colour, one the character lacks the Warn ones, each saying why on hover and to a screen reader.
        let state = bound.as_ref().and_then(|info| match (info.missing, info.active) {
            (true, _) => Some((Status::Warn, Word::HotkeyMissing)),
            (false, true) => Some((Status::Ok, Word::HotkeyShowing)),
            (false, false) => None,
        });
        cell(ui, columns.action, |ui| action_cell(ui, name, state));
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
    }

    /// What a row's trigger button says: its key, "Not bound", or "Press a key…" while it waits.
    fn trigger_text(&self, ctx: &egui::Context, expression: &str) -> String {
        if self.capturing.as_deref() == Some(expression) {
            return text(ctx, Word::HotkeyWaiting).into_owned();
        }
        match self.model.trigger_for(expression) {
            Some(info) => info.binding.key.clone(),
            None => text(ctx, Word::HotkeyUnbound).into_owned(),
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

/// The hotkey table's column widths for the room the table has: the trigger cell takes its share (between
/// [`TRIGGER_MIN`] and [`TRIGGER_WIDTH`]), the Remove button its natural width, and the action the rest.
#[derive(Copy, Clone, Debug)]
struct Columns {
    trigger: f32,
    action: f32,
}

impl Columns {
    /// The widths for rows whose trigger buttons say `keys`: the trigger column as wide as its widest button needs,
    /// within its bounds, so the action keeps the room a narrow dock has.
    fn fit(ui: &Ui, keys: &[String]) -> Self {
        let ctx = ui.ctx();
        let room = ui.available_width();
        let remove = Button::new(&text(ctx, Word::HotkeyRemoveShort)).icon(icons::TRASH).ghost().width(ctx);
        let widest = keys.iter().map(|key| Button::new(key).ghost().width(ctx)).fold(TRIGGER_MIN, f32::max);
        let trigger = widest.min(TRIGGER_WIDTH).min((room * TRIGGER_SHARE).max(TRIGGER_MIN));
        let action = (room - trigger - remove - 2.0 * space::S8).max(ACTION_MIN);
        Self { trigger, action }
    }
}

/// A hotkey row's action: its name, and when the row has a state, that state's icon and colour before it, with the
/// state's words as the tooltip and the accessible description.
fn action_cell(ui: &mut Ui, name: &str, state: Option<(Status, Word)>) {
    let Some((status, word)) = state else {
        ui.add(egui::Label::new(name).truncate());
        return;
    };
    let ds = ds(ui.ctx());
    let colour = ds.theme.status(status);
    ui.spacing_mut().item_spacing.x = space::S4;
    let (icon, _) = ui.allocate_exact_size(vec2(ICON_CELL, ICON_CELL), Sense::hover());
    paint::icon(ui.painter(), &ds, icon.center(), icons::status(status), ICON_CELL, colour, true);
    let words = text(ui.ctx(), word);
    let label = ui.add(egui::Label::new(RichText::new(name).color(colour)).truncate());
    a11y::describe(
        &label,
        || WidgetInfo::labeled(WidgetType::Label, true, name),
        |node| {
            node.set_description(words.to_string());
        },
    );
    let _ = label.tip(Tip::new(&words));
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

/// A target that takes a dropped file; returns its path when one is dropped while the pointer is over it.
pub(crate) fn drop_zone(ui: &mut Ui, label: &str) -> Option<String> {
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

/// `widget`, disabled with `reason` when there is one (`disabled` is the widget's own `disabled(reason)`).
fn disable<'a, W>(widget: W, reason: Option<&'a str>, disabled: impl FnOnce(W, &'a str) -> W) -> W {
    match reason {
        Some(reason) => disabled(widget, reason),
        None => widget,
    }
}

/// Why a control whose op is missing from this build is disabled.
fn missing_reason(ctx: &egui::Context, control: Control) -> Option<String> {
    match control {
        Control::Ready => None,
        Control::Missing(op) => Some(say(ctx, Word::NoOp, &[("op", op)])),
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
