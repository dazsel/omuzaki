//! The Setup panel's drawing (`shell.md` section 14, `wireframes/firstrun-1366.svg`): the step list, each step's body
//! and the footer. Every function here draws [`View`] and the host's [`HostStatus`] and returns what the person asked
//! for; none of them decides anything, so the flow ([`omuzaki_setup::SetupFlow`]) stays the one place that does.

use egui::{
    Align, Align2, Color32, Frame, Layout, Margin, Rect, Response, RichText, Sense, Stroke, StrokeKind, Ui, WidgetInfo,
    WidgetType, pos2, vec2,
};
use omuzaki_setup::{CharacterChoice, Event, InputChoice, Note, OutputKind, Probes, Step, View};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;
use omuzaki_stream_api::window::DEFAULT_TITLE;

use super::host::{HostStatus, SetupHost};
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint;
use crate::ds::theme::Ds;
use crate::ds::tokens::{Status, radius, space};
use crate::ds::widgets::{Banner, Button, Choice, Dropdown, StatusChip};
use crate::live::panel::drop_zone;
use crate::panels::text::{self, say};

/// What the person asked for this frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Ask {
    /// An event for the flow.
    Flow(Event),
    /// Leave setup for this place (Go-live's buttons), after the flow finishes.
    Exit(Exit),
    /// The camera at this index of [`HostStatus::cameras`].
    Camera(usize),
    /// Import this model file (the Import card's drop zone).
    Import(String),
    /// Show the Import card's drop zone.
    OpenImport,
    /// Open the Agent panel ("Connect an agent").
    Agent,
}

/// Where Go-live's buttons go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Exit {
    /// The Perform workspace.
    Perform,
    /// The Build workspace.
    Build,
    /// Where the person is.
    Stay,
}

/// What a step draws from.
pub(super) struct Page<'a> {
    pub ds: &'a Ds,
    pub view: &'a View,
    pub probes: &'a Probes,
    pub status: &'a HostStatus,
    pub host: &'a dyn SetupHost,
    /// Steps the person skipped so far.
    pub skipped: &'a [Step],
    /// The Import card's drop zone is open.
    pub importing: bool,
}

/// Height of the step list.
const STEPPER_H: f32 = 48.0;
/// The step list's circles.
const DOT: f32 = 16.0;
/// A choice card's radio mark.
const RADIO: f32 = 16.0;

// ---- chrome ----

/// The top line: "Setup · 2 of 5" and Skip setup with its Esc cap.
pub(super) fn header(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    ui.horizontal(|ui| {
        if let Some((n, of)) = page.view.position {
            let (n, of) = (n.to_string(), of.to_string());
            let line = say(ds, text::SETUP_HEADER, &[("n", &n), ("of", &of)]);
            ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text2));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            let skip = say(ds, text::SETUP_SKIP, &[]);
            if ui.add(Button::new(&skip).ghost().shortcut(egui::Key::Escape)).clicked() {
                asks.push(Ask::Flow(Event::SkipSetup));
            }
        });
    });
    asks
}

#[allow(clippy::cast_precision_loss)] // five steps at most
/// The steps after Welcome as numbered circles on a line, each a shape and a word as well as a colour (P9): a check
/// for done, a filled number for the current one, a dashed circle for skipped and an outlined number for the rest.
pub(super) fn stepper(ui: &mut Ui, page: &Page<'_>) {
    let ds = page.ds;
    let t = &ds.theme;
    let steps: Vec<Step> = page.view.shown.iter().copied().filter(|s| *s != Step::Welcome).collect();
    if steps.is_empty() || page.view.step == Step::Welcome {
        return;
    }
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(vec2(width, STEPPER_H), Sense::hover());
    let slot = width / steps.len() as f32;
    let current = page.view.step;
    for (i, step) in steps.iter().enumerate() {
        let centre = pos2(rect.left() + slot * (i as f32 + 0.5), rect.top() + DOT / 2.0 + 4.0);
        let state = if *step == current {
            State::Current
        } else if page.skipped.contains(step) {
            State::Skipped
        } else if step.index() < current.index() {
            State::Done
        } else {
            State::Next
        };
        if i + 1 < steps.len() {
            let next = pos2(centre.x + slot, centre.y);
            let colour = if step.index() < current.index() { t.accent } else { t.line };
            ui.painter().line_segment([pos2(centre.x + DOT, centre.y), pos2(next.x - DOT, next.y)], (2.0, colour));
        }
        let number = (i + 1).to_string();
        match state {
            State::Done => {
                paint::icon(ui.painter(), ds, centre, icons::CHECK_CIRCLE, DOT + 2.0, t.ok, true);
            }
            State::Current => {
                ui.painter().circle_filled(centre, DOT / 2.0, t.accent);
                paint::text(ui.painter(), centre, Align2::CENTER_CENTER, &number, &ds.text.micro, t.on_accent);
            }
            State::Skipped => {
                paint::icon(ui.painter(), ds, centre, icons::CIRCLE_DASHED, DOT + 2.0, t.text3, false);
            }
            State::Next => {
                ui.painter().circle_stroke(centre, DOT / 2.0 - 0.5, Stroke::new(1.0, t.text3));
                paint::text(ui.painter(), centre, Align2::CENTER_CENTER, &number, &ds.text.micro, t.text2);
            }
        }
        let name = say(ds, step_name(*step), &[]);
        let (font, colour) =
            if state == State::Current { (&ds.text.strong, t.text) } else { (&ds.text.small, t.text2) };
        let label_at = pos2(centre.x, centre.y + DOT / 2.0 + 4.0);
        paint::text(ui.painter(), label_at, Align2::CENTER_TOP, &name, font, colour);
        // Each step is a named node, so a screen reader hears the list as the eye sees it.
        let cell = Rect::from_center_size(pos2(centre.x, rect.center().y), vec2(slot, STEPPER_H));
        let response = ui.interact(cell, ui.id().with(("setup_step", i)), Sense::hover());
        let word = say(ds, state.word(), &[]);
        let spoken = say(ds, text::SETUP_STEP_STATE, &[("n", &number), ("name", &name), ("state", &word)]);
        a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Label, true, spoken.as_ref()), |_| {});
    }
}

/// A step's place in the list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Done,
    Current,
    Skipped,
    Next,
}

impl State {
    const fn word(self) -> &'static str {
        match self {
            Self::Done => text::SETUP_STATE_DONE,
            Self::Current => text::SETUP_STATE_CURRENT,
            Self::Skipped => text::SETUP_STATE_SKIPPED,
            Self::Next => text::SETUP_STATE_NEXT,
        }
    }
}

const fn step_name(step: Step) -> &'static str {
    match step {
        Step::Welcome => text::SETUP_STEP_WELCOME,
        Step::Character => text::SETUP_STEP_CHARACTER,
        Step::Input => text::SETUP_STEP_INPUT,
        Step::Calibrate => text::SETUP_STEP_CALIBRATE,
        Step::Output => text::SETUP_STEP_OUTPUT,
        Step::GoLive => text::SETUP_STEP_GO_LIVE,
    }
}

/// The footer: Back (off on the first shown step, with the reason), Continue with its Enter cap, how long the step
/// takes, and the project line.
pub(super) fn footer(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let step = page.view.step;
    let mut asks = Vec::new();
    ui.add_space(space::S8);
    ui.horizontal(|ui| {
        let back = say(ds, text::SETUP_BACK, &[]);
        let first = say(ds, text::SETUP_BACK_FIRST, &[]);
        let button = Button::new(&back).icon(icons::ARROW_LEFT).disabled_if(!page.view.can_back, &first);
        if ui.add(button).clicked() {
            asks.push(Ask::Flow(Event::Back));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !matches!(step, Step::Welcome | Step::GoLive) {
                let label = say(ds, text::SETUP_CONTINUE, &[]);
                let why = say(ds, text::SETUP_CHOOSE_INPUT, &[]);
                let button = Button::new(&label).primary().shortcut(egui::Key::Enter);
                let button = button.disabled_if(!can_continue(page), &why);
                if ui.add(button).clicked() {
                    asks.push(continue_ask(page));
                }
            }
            let seconds = step.budget_s().to_string();
            let about = say(ds, text::SETUP_ABOUT, &[("seconds", &seconds)]);
            ui.label(RichText::new(about).font(ds.text.small.clone()).color(ds.theme.text3));
        });
    });
    ui.add_space(space::S4);
    let line = say(ds, text::SETUP_PROJECT_LINE, &[]);
    ui.label(RichText::new(line).font(ds.text.small.clone()).color(ds.theme.text3));
    asks
}

/// Whether Continue does anything: on Input, only once a way to track is chosen.
pub(super) fn can_continue(page: &Page<'_>) -> bool {
    page.view.step != Step::Input || page.view.input.is_some()
}

/// What Continue (and Enter) does on the step showing: Next, except on Character, whose cards are the choice, where
/// it takes the first card (Keep on a rerun, else the sample).
pub(super) fn continue_ask(page: &Page<'_>) -> Ask {
    match page.view.step {
        Step::Character => {
            let choice = if page.probes.project_open { CharacterChoice::Keep } else { CharacterChoice::Sample };
            Ask::Flow(Event::ChooseCharacter(choice))
        }
        Step::GoLive => Ask::Exit(Exit::Perform),
        _ => Ask::Flow(Event::Next),
    }
}

// ---- steps ----

/// The step's body.
pub(super) fn body(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    match page.view.step {
        Step::Welcome => welcome(ui, page),
        Step::Character => character(ui, page),
        Step::Input => input(ui, page),
        Step::Calibrate => calibrate(ui, page),
        Step::Output => output(ui, page),
        Step::GoLive => go_live(ui, page),
    }
}

fn heading(ui: &mut Ui, ds: &Ds, title: &str, line: &str) {
    ui.add_space(space::S4);
    let title = say(ds, title, &[]);
    let response = ui.label(RichText::new(title.as_ref()).font(ds.text.display.clone()).color(ds.theme.text));
    // Focus moves to a new step's heading (the flow's Show effect), so a screen reader starts there.
    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, title.as_ref()));
    if !line.is_empty() {
        ui.label(RichText::new(say(ds, line, &[])).color(ds.theme.text2));
    }
    ui.add_space(space::S8);
}

fn notes(ui: &mut Ui, page: &Page<'_>) {
    for note in &page.view.notes {
        let (status, line) = match *note {
            Note::NoCamera => (Status::Info, text::SETUP_NOTE_NO_CAMERA),
            Note::PermissionDenied => (Status::Warn, text::SETUP_NOTE_DENIED),
            Note::TrackerDownloading(_) => (Status::Info, text::SETUP_NOTE_DOWNLOADING),
            Note::TrackerUnavailable => (Status::Warn, text::SETUP_NOTE_TRACKER),
            Note::IphoneNotPaired => (Status::Info, text::SETUP_NOTE_IPHONE),
            Note::InputNotApplied => (Status::Warn, text::SETUP_NOTE_INPUT),
            Note::OutputNotApplied => (Status::Warn, text::SETUP_NOTE_OUTPUT),
            Note::CharacterNotApplied => (Status::Warn, text::SETUP_NOTE_CHARACTER),
            Note::CalibrationNotDone => (Status::Warn, text::SETUP_NOTE_CALIBRATION),
            Note::OutputUnavailable => (Status::Info, text::SETUP_NOTE_NO_OUTPUT),
            // Welcome draws these as its own controls.
            Note::NoAssistant | Note::OfferSimplifiedChinese => continue,
        };
        let percent = match *note {
            Note::TrackerDownloading(p) => p.to_string(),
            _ => String::new(),
        };
        let line = say(page.ds, line, &[("percent", &percent)]);
        let _ = Banner::new(status, &line).show(ui);
        ui.add_space(space::S4);
    }
}

fn welcome(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    ui.add_space(space::S16);
    heading(ui, ds, text::SETUP_WELCOME, "");
    if page.probes.privacy_claim_true {
        ui.label(RichText::new(say(ds, text::SETUP_WELCOME_LINE, &[])).color(ds.theme.text));
    }
    ui.label(RichText::new(say(ds, text::SETUP_WELCOME_HOW, &[])).color(ds.theme.text2));
    ui.label(RichText::new(say(ds, text::SETUP_FOLLOWS_SYSTEM, &[])).font(ds.text.small.clone()).color(ds.theme.text3));
    ui.add_space(space::S16);
    let start = say(ds, text::SETUP_START, &[]);
    let width = ui.available_width();
    if ui.add(Button::new(&start).primary().shortcut(egui::Key::Enter).min_width(width)).clicked() {
        asks.push(Ask::Flow(Event::Next));
    }
    ui.add_space(space::S8);
    let label = if page.view.notes.contains(&Note::NoAssistant) { text::SETUP_CONNECT } else { text::SETUP_ASSISTANT };
    if ui.add(Button::new(&say(ds, label, &[])).ghost().icon(icons::ROBOT)).clicked() {
        asks.push(Ask::Agent);
    }
    asks
}

fn character(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    heading(ui, ds, text::SETUP_CHARACTER, text::SETUP_CHARACTER_HOW);
    notes(ui, page);
    let mut cards = vec![
        (CharacterChoice::Sample, icons::SMILEY, text::SETUP_SAMPLE, text::SETUP_SAMPLE_LINE),
        (CharacterChoice::FromSheetOrIdea, icons::SPARKLE, text::SETUP_SHEET, text::SETUP_SHEET_LINE),
        (CharacterChoice::ImportVrm, icons::UPLOAD, text::SETUP_IMPORT, text::SETUP_IMPORT_LINE),
    ];
    if page.probes.project_open {
        cards.insert(0, (CharacterChoice::Keep, icons::CUBE, text::SETUP_KEEP, text::SETUP_KEEP_LINE));
    }
    let off = say(ds, text::SETUP_NOT_IN_BUILD, &[]);
    for (choice, icon, title, line) in cards {
        let ready = page.host.character(choice).is_ok();
        let (title, line) = (say(ds, title, &[]), say(ds, line, &[]));
        let card =
            Card { icon, title: &title, line: &line, status: None, selected: false, off: (!ready).then_some(&*off) };
        if card.show(ui, ds, ("setup_character", choice_key(choice))).clicked() && ready {
            asks.push(if choice == CharacterChoice::ImportVrm {
                Ask::OpenImport
            } else {
                Ask::Flow(Event::ChooseCharacter(choice))
            });
        }
        if choice == CharacterChoice::ImportVrm && page.importing {
            if let Some(path) = drop_zone(ui, &say(ds, text::SETUP_VRM, &[])) {
                asks.push(Ask::Import(path));
            }
            if let Some(file) = &page.status.imported {
                ui.add(StatusChip::new(Status::Ok, &say(ds, text::SETUP_VRM_DONE, &[("file", file)])));
            }
        }
        ui.add_space(space::S8);
    }
    asks
}

const fn choice_key(choice: CharacterChoice) -> u8 {
    match choice {
        CharacterChoice::Sample => 0,
        CharacterChoice::FromSheetOrIdea => 1,
        CharacterChoice::ImportVrm => 2,
        CharacterChoice::Keep => 3,
    }
}

fn input(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    heading(ui, ds, text::SETUP_INPUT, text::SETUP_INPUT_HOW);
    notes(ui, page);
    let chosen = page.view.input;
    let webcam = page.probes.cameras.webcam_possible();
    // The webcam's live line: the face and the camera, in words beside the colour.
    let face = webcam.then(|| {
        let camera = page.status.camera.clone().unwrap_or_default();
        match page.status.tracking {
            Some(Tracking::Tracking) => (Status::Ok, say(ds, text::SETUP_FACE_FOUND, &[("camera", &camera)])),
            Some(Tracking::Lost) => (Status::Warn, say(ds, text::SETUP_FACE_LOOKING, &[("camera", &camera)])),
            _ => (Status::Info, say(ds, text::SETUP_FACE_STARTING, &[("camera", &camera)])),
        }
    });
    let cards = [
        (InputChoice::Webcam, icons::WEBCAM, text::SETUP_WEBCAM, text::SETUP_WEBCAM_LINE),
        (InputChoice::Iphone, icons::DEVICE_MOBILE, text::SETUP_IPHONE, text::SETUP_IPHONE_LINE),
        (InputChoice::LifeLayerOnly, icons::HEARTBEAT, text::SETUP_SKIP_FOR_NOW, text::SETUP_SKIP_FOR_NOW_LINE),
    ];
    let no_camera = say(ds, text::SETUP_NO_WEBCAM, &[]);
    for (choice, icon, title, line) in cards {
        let (title, line) = (say(ds, title, &[]), say(ds, line, &[]));
        let status = match choice {
            InputChoice::Webcam if chosen == Some(InputChoice::Webcam) => {
                face.as_ref().map(|(status, line)| (*status, line.as_ref()))
            }
            _ => None,
        };
        let pair = say(ds, text::SETUP_IPHONE_PAIR, &[]);
        let status = status.or_else(|| (choice == InputChoice::Iphone).then_some((Status::Info, pair.as_ref())));
        let off = (choice == InputChoice::Webcam && !webcam).then_some(&*no_camera);
        let card = Card { icon, title: &title, line: &line, status, selected: chosen == Some(choice), off };
        if card.show(ui, ds, ("setup_input", choice as u8)).clicked() && off.is_none() {
            // "Skip for now" is the step's skip: the life layer, recorded as skipped, and on to the next step.
            let event = if choice == InputChoice::LifeLayerOnly { Event::Skip } else { Event::ChooseInput(choice) };
            asks.push(Ask::Flow(event));
        }
        if choice == InputChoice::Webcam && page.view.camera_picker && chosen == Some(InputChoice::Webcam) {
            let choices: Vec<Choice<'_, Option<usize>>> =
                page.status.cameras.iter().enumerate().map(|(i, name)| Choice::new(Some(i), name)).collect();
            let bound = page.status.camera.as_ref().and_then(|c| page.status.cameras.iter().position(|n| n == c));
            let mut picked = bound;
            let label = say(ds, text::SETUP_CAMERA, &[]);
            ui.add(Dropdown::new(&label, &mut picked, &choices).id_salt("setup.camera").stretch());
            if picked != bound
                && let Some(index) = picked
            {
                asks.push(Ask::Camera(index));
            }
        }
        ui.add_space(space::S8);
    }
    if chosen == Some(InputChoice::Webcam) && webcam {
        if page.probes.cameras.permission() == Some(omuzaki_setup::Permission::Undecided) {
            ui.label(RichText::new(say(ds, text::SETUP_PERMISSION_WHY, &[])).color(ds.theme.text2));
        }
        ui.add_space(space::S4);
        ui.label(RichText::new(say(ds, text::SETUP_QUICK_CHECK, &[])).font(ds.text.strong.clone()));
        ui.label(RichText::new(say(ds, text::SETUP_QUICK_CHECK_LINE, &[])).color(ds.theme.text2));
    }
    asks
}

fn calibrate(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    heading(ui, ds, text::SETUP_CALIBRATE, text::SETUP_CALIBRATE_HOW);
    notes(ui, page);
    match page.status.calibration {
        Some(CalibrationState::Capturing { frames, of }) => {
            let (frames, of) = (frames.to_string(), of.to_string());
            let line = say(ds, text::SETUP_CALIBRATING, &[("frames", &frames), ("of", &of)]);
            ui.add(StatusChip::new(Status::Info, &line));
        }
        Some(CalibrationState::Done { .. }) => {
            ui.add(StatusChip::new(Status::Ok, &say(ds, text::SETUP_CALIBRATED, &[])));
            ui.label(RichText::new(say(ds, text::SETUP_CALIBRATED_LINE, &[])).font(ds.text.small.clone()));
        }
        _ => {}
    }
    ui.add_space(space::S8);
    let defaults = say(ds, text::SETUP_USE_DEFAULTS, &[]);
    if ui.add(Button::new(&defaults).ghost()).clicked() {
        asks.push(Ask::Flow(Event::Skip));
    }
    ui.label(
        RichText::new(say(ds, text::SETUP_USE_DEFAULTS_LINE, &[])).font(ds.text.small.clone()).color(ds.theme.text3),
    );
    asks
}

fn output(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let mut asks = Vec::new();
    heading(ui, ds, text::SETUP_OUTPUT, text::SETUP_OUTPUT_HOW);
    notes(ui, page);
    if let Some(kind) = page.probes.recommended_output {
        let (title, line) = output_words(kind);
        let (title, line) = (say(ds, title, &[]), say(ds, line, &[("title", DEFAULT_TITLE)]));
        let on = say(ds, text::SETUP_OUTPUT_ON, &[]);
        let status = (page.view.output == Some(kind) && page.status.output_on).then_some((Status::Ok, on.as_ref()));
        let card = Card {
            icon: icons::MONITOR,
            title: &title,
            line: &line,
            status,
            selected: page.view.output == Some(kind),
            off: None,
        };
        if card.show(ui, ds, "setup_output").clicked() {
            asks.push(Ask::Flow(Event::ChooseOutput(kind)));
        }
        ui.add_space(space::S8);
        let obs = say(ds, text::SETUP_OBS, &[]);
        egui::CollapsingHeader::new(RichText::new(obs.as_ref()).font(ds.text.strong.clone()))
            .id_salt("setup_obs")
            .show(ui, |ui| {
                for line in [text::SETUP_OBS_1, text::SETUP_OBS_2, text::SETUP_OBS_3] {
                    ui.label(RichText::new(say(ds, line, &[("title", DEFAULT_TITLE)])).color(ds.theme.text2));
                }
            });
    }
    ui.add_space(space::S8);
    let later = say(ds, text::SETUP_LATER, &[]);
    if ui.add(Button::new(&later).ghost()).clicked() {
        asks.push(Ask::Flow(Event::Skip));
    }
    asks
}

const fn output_words(kind: OutputKind) -> (&'static str, &'static str) {
    match kind {
        OutputKind::WindowCapture => (text::SETUP_OUT_WINDOW, text::SETUP_OUT_WINDOW_LINE),
        OutputKind::Spout => (text::SETUP_OUT_SPOUT, text::SETUP_OUT_SPOUT_LINE),
        OutputKind::Syphon => (text::SETUP_OUT_SYPHON, text::SETUP_OUT_SYPHON_LINE),
        OutputKind::Ndi => (text::SETUP_OUT_NDI, text::SETUP_OUT_NDI_LINE),
        OutputKind::VirtualCamera => (text::SETUP_OUT_CAMERA, text::SETUP_OUT_CAMERA_LINE),
    }
}

#[allow(clippy::cast_precision_loss)] // five checks
fn go_live(ui: &mut Ui, page: &Page<'_>) -> Vec<Ask> {
    let ds = page.ds;
    let t = &ds.theme;
    let mut asks = Vec::new();
    heading(ui, ds, text::SETUP_DONE, text::SETUP_DONE_HOW);
    let tracked = page.view.input == Some(InputChoice::Webcam) && page.status.tracking == Some(Tracking::Tracking);
    let calibrated = matches!(page.status.calibration, Some(CalibrationState::Done { .. }));
    let checks = [
        (true, text::SETUP_CHECK_CHARACTER, text::SETUP_CHECK_CHARACTER),
        (tracked, text::SETUP_CHECK_TRACKING, text::SETUP_CHECK_IDLE),
        (calibrated, text::SETUP_CHECK_CALIBRATED, text::SETUP_CHECK_DEFAULTS),
        (page.status.output_on, text::SETUP_CHECK_OUTPUT, text::SETUP_CHECK_NO_OUTPUT),
        (true, text::SETUP_CHECK_LAYOUT, text::SETUP_CHECK_LAYOUT),
    ];
    let ready = checks.iter().filter(|c| c.0).count();
    let (ready_s, all_s) = (ready.to_string(), checks.len().to_string());
    // The health meter: how much is ready, as a bar and in words.
    let words = say(ds, text::SETUP_HEALTH, &[("ready", &ready_s), ("of", &all_s)]);
    ui.label(RichText::new(words.as_ref()).font(ds.text.strong.clone()));
    let (bar, _) = ui.allocate_exact_size(vec2(ui.available_width(), 6.0), Sense::hover());
    ui.painter().rect_filled(bar, radius::FULL, t.bg3);
    let filled = Rect::from_min_size(bar.min, vec2(bar.width() * ready as f32 / checks.len() as f32, bar.height()));
    ui.painter().rect_filled(filled, radius::FULL, t.ok);
    ui.add_space(space::S8);
    for (ok, yes, no) in checks {
        let (status, line) = if ok { (Status::Ok, yes) } else { (Status::Info, no) };
        ui.add(StatusChip::new(status, &say(ds, line, &[])));
    }
    ui.add_space(space::S16);
    let width = ui.available_width();
    let perform = say(ds, text::SETUP_PERFORM, &[]);
    if ui.add(Button::new(&perform).primary().icon(icons::PLAY).shortcut(egui::Key::Enter).min_width(width)).clicked() {
        asks.push(Ask::Exit(Exit::Perform));
    }
    if ui.add(Button::new(&say(ds, text::SETUP_CUSTOMISE, &[])).icon(icons::CUBE).min_width(width)).clicked() {
        asks.push(Ask::Exit(Exit::Build));
    }
    if ui.add(Button::new(&say(ds, text::SETUP_CLOSE, &[])).ghost().min_width(width)).clicked() {
        asks.push(Ask::Exit(Exit::Stay));
    }
    asks
}

// ---- the choice card ----

/// A large choice: a radio mark, an icon, a title, a line and an optional live status, drawn as a card. The whole card
/// is one radio button for focus, keys and screen readers; a card this build can't carry out is drawn off with its
/// reason, never left out.
struct Card<'a> {
    icon: &'a str,
    title: &'a str,
    line: &'a str,
    status: Option<(Status, &'a str)>,
    selected: bool,
    off: Option<&'a str>,
}

impl Card<'_> {
    fn show(self, ui: &mut Ui, ds: &Ds, salt: impl std::hash::Hash + std::fmt::Debug) -> Response {
        let t = &ds.theme;
        let id = ui.id().with(("setup_card", salt));
        let enabled = self.off.is_none();
        let ink = |colour: Color32| paint::ink(t, enabled, colour);
        let inner = Frame::new().inner_margin(Margin::symmetric(12, 10)).corner_radius(radius::CARD).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.add_space(RADIO + space::S8);
                ui.vertical(|ui| {
                    ui.spacing_mut().item_spacing.y = space::S2;
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(self.icon).font(ds.text.icon(16.0, false)).color(ink(t.text2)));
                        ui.label(RichText::new(self.title).font(ds.text.section.clone()).color(ink(t.text)));
                    });
                    ui.label(RichText::new(self.line).color(ink(t.text2)));
                    if let Some(reason) = self.off {
                        ui.label(RichText::new(reason).font(ds.text.small.clone()).color(t.text3));
                    } else if let Some((status, line)) = self.status {
                        ui.horizontal(|ui| {
                            ui.label(
                                RichText::new(icons::status(status))
                                    .font(ds.text.icon(12.0, true))
                                    .color(t.status(status)),
                            );
                            ui.label(RichText::new(line).font(ds.text.small.clone()).color(t.status(status)));
                        });
                    }
                });
            });
        });
        let rect = inner.response.rect;
        let sense = if enabled { Sense::click() } else { Sense::hover() };
        let response = ui.interact(rect, id, sense);
        let hover = paint::fade(ui.ctx(), ds, id.with("hover"), enabled && paint::hot(&response));
        let stroke = if self.selected {
            Stroke::new(2.0, t.accent)
        } else {
            Stroke::new(1.0, paint::lerp(t.line, t.border_control, hover))
        };
        let fill = if self.selected { t.selection } else { Color32::TRANSPARENT };
        let painter = ui.painter();
        painter.rect_filled(rect, radius::CARD, fill.gamma_multiply(0.35));
        painter.rect_stroke(rect, radius::CARD, stroke, StrokeKind::Inside);
        let mark = pos2(rect.left() + 12.0 + RADIO / 2.0, rect.top() + 10.0 + 10.0);
        painter.circle_stroke(
            mark,
            RADIO / 2.0 - 0.5,
            Stroke::new(1.5, ink(if self.selected { t.accent } else { t.text3 })),
        );
        if self.selected {
            painter.circle_filled(mark, RADIO / 4.0, t.accent);
        }
        paint::focus_ring(painter, t, &response, rect, radius::CARD, paint::RingPlacement::Outside);
        a11y::describe(
            &response,
            || WidgetInfo::selected(WidgetType::RadioButton, enabled, self.selected, self.title),
            |node| {
                let mut said = String::from(self.line);
                if let Some(extra) = self.off.or(self.status.map(|s| s.1)) {
                    said.push_str(". ");
                    said.push_str(extra);
                }
                node.set_description(said);
            },
        );
        response
    }
}
