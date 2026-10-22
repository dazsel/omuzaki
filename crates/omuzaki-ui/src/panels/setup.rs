//! The first-run flow (`shell.md` section 14, SE-01): a dock panel beside the live stage, so the first thing on screen
//! is the character moving. Welcome, character, camera, calibrate, output and "You're set", each a few lines and one
//! obvious button, with Back and Skip setup always there.
//!
//! Every step acts through the same ops as the Live panel ([`LiveModel`]): the camera through `input.*`, calibration
//! through `perform.calibrate`, the output window through `output.window.*`, a dropped model through `io.import`. Nothing
//! the flow does is out of reach later, and nothing is irreversible. A step whose ops this build lacks says so and lets
//! the person go on.
//!
//! Whether the flow ran is remembered by [`FirstRun`], a mark in the studio's config folder: the app opens the flow on a
//! first start, and the flow sets the mark when it ends or is skipped. It opens again from the command palette
//! ("Setup").

use std::cell::Cell;
use std::path::PathBuf;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_ops_api::{Actor, Runner};
use omuzaki_stage::perform::Tracking;
use omuzaki_stage::perform::host::CalibrationState;
use omuzaki_stream_api::window::DEFAULT_TITLE;
use omuzaki_ui_api::panel::{Panel, PanelCtx};

use super::text::{self, say};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Button, Choice, Dropdown, StatusChip};
use crate::live::model::{Control, LiveModel};
use crate::live::panel::drop_zone;
use crate::live::registration::PERFORM_WORKSPACE;
use crate::shell::persist::user_config_dir;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// The Setup panel's id.
pub const ID: &str = "setup";
/// The workspace "Customise the character" opens.
const BUILD_WORKSPACE: &str = "build";
/// The mark's file name, in the studio's config folder.
const MARK: &str = "setup-done";
/// Seconds between reads of the camera, the performer and the output window while a step shows them.
const REFRESH_S: f64 = 0.25;

/// Whether the first-run flow has run, kept as a mark file so it opens once, on the first start.
#[derive(Debug)]
pub struct FirstRun {
    mark: Option<PathBuf>,
    /// Set when the flow ended this session, so a mark that could not be written still keeps it closed until quit.
    ended: Cell<bool>,
}

impl FirstRun {
    /// The mark in the studio's config folder; with no config folder known, it lasts until the studio quits.
    pub fn in_user_config() -> Self {
        Self { mark: user_config_dir().map(|dir| dir.join(MARK)), ended: Cell::new(false) }
    }

    /// The mark at `path` (tests, and a studio given its own config folder).
    pub fn at(path: PathBuf) -> Self {
        Self { mark: Some(path), ended: Cell::new(false) }
    }

    /// Whether the flow has run here before.
    pub fn done(&self) -> bool {
        self.ended.get() || self.mark.as_ref().is_some_and(|mark| mark.exists())
    }

    /// Asks `shell` for the flow when it has not run: the Perform workspace, with the Setup panel beside the stage.
    pub fn open_if_new(&self, shell: &ShellRequests) {
        if !self.done() {
            shell.push(ShellRequest::Workspace(PERFORM_WORKSPACE));
            shell.push(ShellRequest::ShowPanel(ID));
        }
    }

    /// Records that the flow ran. A mark that can't be written is logged: the flow then opens again next start.
    pub fn end(&self) {
        self.ended.set(true);
        let Some(mark) = &self.mark else { return };
        let written = mark.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(mark, b""));
        if let Err(error) = written {
            tracing::warn!(%error, mark = %mark.display(), "the setup mark was not written: setup opens again next start");
        }
    }
}

/// The steps, in order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub enum Step {
    /// What the flow does; Start.
    #[default]
    Welcome,
    /// The sample, or the person's own VRM.
    Character,
    /// The camera follows the face.
    Input,
    /// The rest face.
    Calibrate,
    /// The output window, for OBS.
    Output,
    /// Where to go next.
    Done,
}

impl Step {
    const ALL: [Self; 6] = [Self::Welcome, Self::Character, Self::Input, Self::Calibrate, Self::Output, Self::Done];

    fn index(self) -> usize {
        Self::ALL.iter().position(|step| *step == self).unwrap_or(0)
    }

    fn back(self) -> Self {
        self.index().checked_sub(1).and_then(|i| Self::ALL.get(i).copied()).unwrap_or(Self::Welcome)
    }
}

/// The Setup panel.
#[derive(Debug, Default)]
pub struct SetupPanel {
    step: Step,
    model: Option<LiveModel>,
    /// When the model was last read, in egui seconds.
    read_at: Option<f64>,
    /// The camera step started the first camera once; it doesn't again if the person stops it.
    camera_started: bool,
}

/// What a step asks for once it is drawn.
enum Go {
    To(Step),
    End(Option<&'static str>),
}

impl Panel for SetupPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let ds = ds(ui.ctx());
        let (runner, actor) = (ctx.runner(), ctx.actor());
        let model = self.model.get_or_insert_with(|| LiveModel::new(runner));
        let now = ui.ctx().input(|i| i.time);
        if matches!(self.step, Step::Input | Step::Calibrate | Step::Output)
            && self.read_at.is_none_or(|at| now - at >= REFRESH_S)
        {
            model.refresh(runner, actor);
            self.read_at = Some(now);
        }
        if matches!(self.step, Step::Input | Step::Calibrate | Step::Output) {
            ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_S));
        }
        let mut go = None;
        ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::S8;
            go = egui::Frame::new()
                .inner_margin(space::S12)
                .show(ui, |ui| {
                    let go = self.body(ui, &ds, runner, actor);
                    if let Some(failure) = self.model.as_ref().and_then(LiveModel::failure) {
                        let line = say(&ds, text::SETUP_FAILED, &[("op", failure.op), ("message", &failure.message)]);
                        ui.label(RichText::new(line).color(ds.theme.danger));
                    }
                    go
                })
                .inner;
        });
        match go {
            Some(Go::To(step)) => {
                self.step = step;
                self.read_at = None;
            }
            Some(Go::End(workspace)) => {
                if let Some(first_run) = ctx.view::<FirstRun>() {
                    first_run.end();
                }
                if let Some(shell) = ctx.view::<ShellRequests>() {
                    shell.push(ShellRequest::ClosePanel(ID));
                    if let Some(workspace) = workspace {
                        shell.push(ShellRequest::Workspace(workspace));
                    }
                }
                self.step = Step::Welcome;
            }
            None => {}
        }
    }
}

impl SetupPanel {
    /// The step being shown.
    pub const fn step(&self) -> Step {
        self.step
    }

    fn body(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner, actor: &Actor) -> Option<Go> {
        let mut go = None;
        ui.horizontal(|ui| {
            if self.step != Step::Done {
                let (n, of) = (self.step.index().to_string(), (Step::ALL.len() - 1).to_string());
                if self.step != Step::Welcome {
                    ui.label(RichText::new(say(ds, text::SETUP_STEP, &[("n", &n), ("of", &of)])).color(ds.theme.text3));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.add(Button::new(&say(ds, text::SETUP_SKIP, &[])).ghost()).clicked() {
                        go = Some(Go::End(None));
                    }
                });
            }
        });
        let step = match self.step {
            Step::Welcome => welcome(ui, ds),
            Step::Character => self.character(ui, ds, runner, actor),
            Step::Input => self.input(ui, ds, runner, actor),
            Step::Calibrate => self.calibrate(ui, ds, runner, actor),
            Step::Output => self.output(ui, ds, runner, actor),
            Step::Done => done(ui, ds),
        };
        let step = step.or_else(|| {
            let back = self.step != Step::Welcome && self.step != Step::Done;
            (back && ui.add(Button::new(&say(ds, text::SETUP_BACK, &[])).ghost().icon(icons::ARROW_LEFT)).clicked())
                .then(|| Go::To(self.step.back()))
        });
        go.or(step)
    }

    fn character(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner, actor: &Actor) -> Option<Go> {
        heading(ui, ds, text::SETUP_CHARACTER, None);
        let model = self.model.as_mut()?;
        let mut go = None;
        if ui.add(Button::new(&say(ds, text::SETUP_SAMPLE, &[])).primary().min_width(ui.available_width())).clicked() {
            go = Some(Go::To(Step::Input));
        }
        ui.add_space(8.0);
        if model.ops().import() == Control::Ready
            && let Some(path) = drop_zone(ui, &say(ds, text::SETUP_VRM, &[]))
        {
            model.import(runner, actor, &path);
        }
        if let Some(file) = model.imported() {
            ui.add(StatusChip::new(Status::Ok, &say(ds, text::SETUP_VRM_DONE, &[("file", file)])));
            if ui.add(Button::new(&say(ds, text::SETUP_NEXT, &[])).primary()).clicked() {
                go = Some(Go::To(Step::Input));
            }
        }
        go
    }

    fn input(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner, actor: &Actor) -> Option<Go> {
        heading(ui, ds, text::SETUP_INPUT, Some(text::SETUP_INPUT_HOW));
        let model = self.model.as_mut()?;
        let skip = |ui: &mut Ui, label: &str| {
            ui.add(Button::new(&say(ds, label, &[])).primary()).clicked().then_some(Go::To(Step::Calibrate))
        };
        let Some(view) = model.input().cloned() else {
            // The camera ops aren't in this build, or their first read hasn't come: the life layer has the face.
            return skip(ui, text::SETUP_SKIP_FOR_NOW);
        };
        if view.cameras.is_empty() || model.ops().camera() != Control::Ready {
            ui.label(RichText::new(say(ds, text::SETUP_NO_CAMERA, &[])).color(ds.theme.text2));
            return skip(ui, text::SETUP_SKIP_FOR_NOW);
        }
        // Preselected, as the spec asks: the first camera starts at once, so the character mirrors the person.
        if !self.camera_started {
            self.camera_started = true;
            if view.camera.is_none()
                && let Some(first) = view.cameras.first()
            {
                model.bind_camera(runner, actor, Some(&first.id.0));
            }
            model.set_tracking(runner, actor, true);
        }
        if view.cameras.len() > 1 {
            let choices: Vec<Choice<'_, Option<usize>>> =
                view.cameras.iter().enumerate().map(|(i, camera)| Choice::new(Some(i), &camera.name)).collect();
            let bound = view.cameras.iter().position(|camera| camera.bound);
            let mut chosen = bound;
            let label = say(ds, text::SETUP_CAMERA, &[]);
            ui.add(Dropdown::new(&label, &mut chosen, &choices).id_salt("setup.camera").stretch());
            if chosen != bound
                && let Some(camera) = chosen.and_then(|i| view.cameras.get(i))
            {
                model.bind_camera(runner, actor, Some(&camera.id.0));
                model.set_tracking(runner, actor, true);
            }
        }
        let (status, line) = match model.tracking() {
            Some(Tracking::Tracking) => (Status::Ok, text::SETUP_TRACKING),
            Some(Tracking::Lost) => (Status::Warn, text::SETUP_LOOKING),
            _ => (Status::Info, text::SETUP_STARTING),
        };
        ui.add(StatusChip::new(status, &say(ds, line, &[])));
        ui.add_space(8.0);
        skip(ui, text::SETUP_NEXT)
    }

    fn calibrate(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner, actor: &Actor) -> Option<Go> {
        heading(ui, ds, text::SETUP_CALIBRATE, Some(text::SETUP_CALIBRATE_HOW));
        let model = self.model.as_mut()?;
        let tracked = model.tracking() == Some(Tracking::Tracking);
        let ready = model.ops().calibrate() == Control::Ready;
        if ready {
            let needs = say(ds, text::SETUP_CALIBRATE_NEEDS, &[]);
            let go = say(ds, text::SETUP_CALIBRATE_GO, &[]);
            let button = Button::new(&go).op("perform.calibrate").disabled_if(!tracked || model.calibrating(), &needs);
            if ui.add(button).clicked() {
                model.calibrate(runner, actor);
            }
        }
        match model.calibration() {
            Some(CalibrationState::Capturing { frames, of }) => {
                let (frames, of) = (frames.to_string(), of.to_string());
                let line = say(ds, text::SETUP_CALIBRATING, &[("frames", &frames), ("of", &of)]);
                ui.add(StatusChip::new(Status::Info, &line));
            }
            Some(CalibrationState::Done { .. }) => {
                ui.add(StatusChip::new(Status::Ok, &say(ds, text::SETUP_CALIBRATED, &[])));
            }
            Some(CalibrationState::Failed { .. }) => {
                ui.add(StatusChip::new(Status::Warn, &say(ds, text::SETUP_CALIBRATE_FAILED, &[])));
            }
            _ => {}
        }
        ui.add_space(8.0);
        let done = matches!(model.calibration(), Some(CalibrationState::Done { .. }));
        let label = if done { text::SETUP_NEXT } else { text::SETUP_SKIP_FOR_NOW };
        let label = say(ds, label, &[]);
        let next = Button::new(&label);
        let next = if done { next.primary() } else { next };
        ui.add(next).clicked().then_some(Go::To(Step::Output))
    }

    fn output(&mut self, ui: &mut Ui, ds: &Ds, runner: &dyn Runner, actor: &Actor) -> Option<Go> {
        let how = say(ds, text::SETUP_OUTPUT_HOW, &[("title", DEFAULT_TITLE)]);
        heading(ui, ds, text::SETUP_OUTPUT, None);
        ui.label(RichText::new(how).color(ds.theme.text2));
        ui.add_space(8.0);
        let model = self.model.as_mut()?;
        if model.ops().output() == Control::Ready {
            if model.window_open() {
                ui.add(StatusChip::new(Status::Ok, &say(ds, text::SETUP_OUTPUT_OPENED, &[])));
            } else if ui.add(Button::new(&say(ds, text::SETUP_OUTPUT_OPEN, &[])).primary()).clicked() {
                model.set_window_open(runner, actor, true);
            }
        }
        ui.add_space(8.0);
        ui.add(Button::new(&say(ds, text::SETUP_NEXT, &[]))).clicked().then_some(Go::To(Step::Done))
    }
}

/// A step's title and, when it has one, its line.
fn heading(ui: &mut Ui, ds: &Ds, title: &str, line: Option<&str>) {
    ui.add_space(4.0);
    ui.label(RichText::new(say(ds, title, &[])).font(ds.text.title.clone()).color(ds.theme.text));
    if let Some(line) = line {
        ui.label(RichText::new(say(ds, line, &[])).color(ds.theme.text2));
    }
    ui.add_space(8.0);
}

fn welcome(ui: &mut Ui, ds: &Ds) -> Option<Go> {
    heading(ui, ds, text::SETUP_WELCOME, Some(text::SETUP_WELCOME_LINE));
    ui.label(RichText::new(say(ds, text::SETUP_WELCOME_HOW, &[])).color(ds.theme.text2));
    ui.add_space(8.0);
    ui.add(Button::new(&say(ds, text::SETUP_START, &[])).primary()).clicked().then_some(Go::To(Step::Character))
}

fn done(ui: &mut Ui, ds: &Ds) -> Option<Go> {
    heading(ui, ds, text::SETUP_DONE, Some(text::SETUP_DONE_HOW));
    let mut go = None;
    if ui.add(Button::new(&say(ds, text::SETUP_PERFORM, &[])).primary().icon(icons::PLAY)).clicked() {
        go = Some(Go::End(Some(PERFORM_WORKSPACE)));
    }
    if ui.add(Button::new(&say(ds, text::SETUP_CUSTOMISE, &[])).icon(icons::CUBE)).clicked() {
        go = Some(Go::End(Some(BUILD_WORKSPACE)));
    }
    if ui.add(Button::new(&say(ds, text::SETUP_CLOSE, &[])).ghost()).clicked() {
        go = Some(Go::End(None));
    }
    go
}

omuzaki_ui_api::register_panel! {
    id: "setup", title: "Setup", icon: icons::SPARKLE, region: omuzaki_ui_api::registry::Region::Right,
    min_size: [288.0, 160.0], order: 5,
    build: |_| Box::new(SetupPanel::default()),
}
