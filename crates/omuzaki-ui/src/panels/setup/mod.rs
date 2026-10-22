//! The first-run flow (`shell.md` section 14, SE-01): a dock panel beside the live stage, so the first thing on screen
//! is the character moving. Welcome, Character, Input, Calibrate, Output and Go-live, each a few lines and one obvious
//! choice, with Back, Continue (Enter) and Skip setup (Esc) always there.
//!
//! The panel holds no logic of its own: [`omuzaki_setup::SetupFlow`] decides which step shows, what each one says and
//! what the studio must do, from what the machine can do ([`Probes`]) and what the person did ([`Event`]). The panel
//! draws the flow's [`View`](omuzaki_setup::View) and hands the flow's [`Effect`]s to a [`SetupHost`], which carries them
//! out through the same ops as the Live panel: the camera through `input.*`, calibration through `perform.calibrate`,
//! the output window through `output.window.*`, a dropped model through `io.import`. Nothing the flow does is out of
//! reach later, and nothing is irreversible.
//!
//! Whether the flow ran, and where it stopped, is remembered by [`FirstRun`] in the studio's config folder: the app
//! opens the flow on a first start and resumes it after a restart, and it opens again from the menu.

mod draw;
mod host;

use std::cell::Cell;
use std::collections::VecDeque;
use std::path::PathBuf;

use egui::{Frame, Key, Margin, Modifiers, ScrollArea, Ui};
use omuzaki_setup::{Effect, Event, Probes, Progress, SetupFlow, Step};
use omuzaki_ui_api::panel::{Panel, PanelCtx};

use self::draw::{Ask, Exit, Page};
pub use self::host::{HostStatus, LiveHost, SetupHost, Unavailable};
use super::text::{self, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::space;
use crate::live::registration::PERFORM_WORKSPACE;
use crate::shell::persist::user_config_dir;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// The Setup panel's id.
pub const ID: &str = "setup";
/// The workspace "Customise the character" opens.
const BUILD_WORKSPACE: &str = "build";
/// The mark's file name, in the studio's config folder.
const MARK: &str = "setup-done";
/// Where the flow stopped, beside the mark.
const PROGRESS: &str = "setup-progress.json";
/// Seconds between reads of the camera, the performer and the output while setup shows.
const REFRESH_S: f64 = 0.25;
/// The most events one frame passes through the flow, answers included: a bound, never reached by a real host.
const MAX_EVENTS: usize = 32;

/// Whether the first-run flow has run, kept as a mark file so it opens once, on the first start, and where it stopped.
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

    /// Where the flow stopped last time. A finished flow, or none saved, starts at Welcome; so does a file that doesn't
    /// read, which is logged.
    pub fn progress(&self) -> Progress {
        let Some(path) = self.progress_path() else { return Progress::default() };
        let Ok(bytes) = std::fs::read(&path) else { return Progress::default() };
        match serde_json::from_slice::<Progress>(&bytes) {
            Ok(progress) if !progress.finished => progress,
            Ok(_) => Progress::default(),
            Err(error) => {
                tracing::warn!(%error, path = %path.display(), "the setup progress doesn't read: setup starts over");
                Progress::default()
            }
        }
    }

    /// Saves where the flow is, so a restart resumes there. A write that fails is logged.
    pub fn save(&self, progress: &Progress) {
        let Some(path) = self.progress_path() else { return };
        let written = serde_json::to_vec(progress).map_err(std::io::Error::other).and_then(|bytes| {
            path.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::write(&path, bytes))
        });
        if let Err(error) = written {
            tracing::warn!(%error, path = %path.display(), "the setup progress was not saved");
        }
    }

    fn progress_path(&self) -> Option<PathBuf> {
        self.mark.as_ref().map(|mark| mark.with_file_name(PROGRESS))
    }
}

/// Makes the Setup panel's host, shared as a view model: a studio without one gets a [`LiveHost`]. Tests and the
/// visual evidence give one with chosen probes.
#[derive(Clone)]
pub struct SetupHosts(pub std::rc::Rc<MakeHost>);

/// Makes a host over the runner; `true` when setup ran here before.
pub type MakeHost = dyn Fn(&dyn omuzaki_ops_api::Runner, bool) -> Box<dyn SetupHost>;

/// The Setup panel.
#[derive(Default)]
pub struct SetupPanel {
    host: Option<Box<dyn SetupHost>>,
    flow: Option<SetupFlow>,
    /// The probes the flow last heard, so a change is sent once.
    probes: Option<Probes>,
    /// When the host was last read, in egui seconds.
    read_at: Option<f64>,
    /// The Import card's drop zone is open.
    importing: bool,
    /// Where Go-live's button asked to go, while the flow finishes.
    exit: Option<Exit>,
    /// The egui frame the panel was last drawn in.
    drawn: u64,
}

impl std::fmt::Debug for SetupPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SetupPanel").field("step", &self.step()).finish_non_exhaustive()
    }
}

impl SetupPanel {
    /// A panel over `host` (tests give a fake one; the studio's is made on the first frame).
    pub fn with_host(host: Box<dyn SetupHost>) -> Self {
        Self { host: Some(host), ..Self::default() }
    }

    /// The step showing; Welcome before the first frame.
    pub fn step(&self) -> Step {
        self.flow.as_ref().map_or(Step::Welcome, |flow| flow.view().step)
    }

    /// What would be saved now.
    pub fn progress(&self) -> Option<Progress> {
        self.flow.as_ref().map(SetupFlow::progress)
    }

    /// Passes `asks` through the flow and the host, then the host's answers, until nothing is left.
    fn settle(&mut self, ctx: &PanelCtx<'_>, now_ms: u64, asks: Vec<Ask>) {
        let (runner, actor) = (ctx.runner(), ctx.actor());
        let mut events: VecDeque<Event> = VecDeque::new();
        for ask in asks {
            match ask {
                Ask::Flow(event) => events.push_back(event),
                Ask::Exit(exit) => {
                    self.exit = Some(exit);
                    events.push_back(Event::Finish);
                }
                Ask::Camera(index) => {
                    if let Some(host) = self.host.as_mut() {
                        host.pick_camera(runner, actor, index);
                    }
                }
                Ask::Import(path) => {
                    if let Some(host) = self.host.as_mut() {
                        host.stage_import(path);
                    }
                    events.push_back(Event::ChooseCharacter(omuzaki_setup::CharacterChoice::ImportVrm));
                }
                Ask::OpenImport => self.importing = true,
                Ask::Agent => {
                    if let Some(shell) = ctx.view::<ShellRequests>() {
                        shell.push(ShellRequest::ShowPanel(super::agent::ID));
                    }
                }
            }
        }
        let (Some(flow), Some(host)) = (self.flow.as_mut(), self.host.as_mut()) else { return };
        events.extend(host.poll());
        let mut handled = 0;
        while let Some(event) = (handled < MAX_EVENTS).then(|| events.pop_front()).flatten() {
            handled += 1;
            for effect in flow.handle(event, now_ms) {
                match &effect {
                    Effect::Persist(progress) => {
                        if let Some(first_run) = ctx.view::<FirstRun>() {
                            first_run.save(progress);
                        }
                    }
                    Effect::Finished { .. } => {
                        if let Some(first_run) = ctx.view::<FirstRun>() {
                            first_run.save(&flow.progress());
                            first_run.end();
                        }
                        if let Some(shell) = ctx.view::<ShellRequests>() {
                            shell.push(ShellRequest::ClosePanel(ID));
                            match self.exit.take() {
                                Some(Exit::Perform) => shell.push(ShellRequest::Workspace(PERFORM_WORKSPACE)),
                                Some(Exit::Build) => shell.push(ShellRequest::Workspace(BUILD_WORKSPACE)),
                                Some(Exit::Stay) | None => {}
                            }
                        }
                    }
                    _ => {}
                }
                // Answers go first: the flow numbers its requests and expects each answered before the next event.
                for answer in host.run_effect(runner, actor, &effect).into_iter().rev() {
                    events.push_front(answer);
                }
            }
        }
    }
}

impl Panel for SetupPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &PanelCtx<'_>) {
        let ds = ds(ui.ctx());
        let (runner, actor) = (ctx.runner(), ctx.actor());
        let now = ui.ctx().input(|i| i.time);
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // egui's clock starts at 0 and counts up
        let now_ms = (now * 1000.0) as u64;
        let rerun = ctx.view::<FirstRun>().is_some_and(FirstRun::done);
        let host = self.host.get_or_insert_with(|| match ctx.view::<SetupHosts>() {
            Some(hosts) => (hosts.0)(runner, rerun),
            None => Box::new(LiveHost::new(runner, rerun)),
        });
        if self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            host.refresh(runner, actor);
            self.read_at = Some(now);
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_S));
        let probes = host.probes();
        // Shown again after it ended and was closed (Help, Run setup again): the flow starts over.
        let frame = ui.ctx().cumulative_frame_nr();
        let reopened = frame > self.drawn.saturating_add(1);
        self.drawn = frame;
        if reopened && self.flow.as_ref().is_some_and(|flow| flow.progress().finished) {
            (self.flow, self.probes, self.exit, self.importing) = (None, None, None, false);
        }
        let flow = self.flow.get_or_insert_with(|| {
            let resume = ctx.view::<FirstRun>().map(FirstRun::progress).unwrap_or_default();
            SetupFlow::new(probes, resume, host.current())
        });
        let mut asks = Vec::new();
        if self.probes != Some(probes) {
            self.probes = Some(probes);
            asks.push(Ask::Flow(Event::Probed(probes)));
        }
        let view = flow.view();
        let status = host.status();
        let skipped = flow.progress().skipped;
        let page = Page {
            ds: &ds,
            view: &view,
            probes: &probes,
            status: &status,
            host: host.as_ref(),
            skipped: &skipped,
            importing: self.importing,
        };
        Frame::new().inner_margin(Margin::same(12)).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = space::S8;
            asks.extend(draw::header(ui, &page));
            draw::stepper(ui, &page);
            let footer_h = 76.0;
            let body_h = (ui.available_height() - footer_h).max(120.0);
            ScrollArea::vertical().id_salt("setup_body").max_height(body_h).auto_shrink([false, true]).show(ui, |ui| {
                asks.extend(draw::body(ui, &page));
                if let Some((op, message)) = &status.failure {
                    let line = say(&ds, text::SETUP_FAILED, &[("op", op), ("message", message)]);
                    ui.label(egui::RichText::new(line).color(ds.theme.danger));
                }
            });
            asks.extend(draw::footer(ui, &page));
            // Keyboard first: Enter continues and Esc skips setup, unless a control has the key (a focused button
            // takes Enter as its own press; an open list takes Esc to close).
            let input = ui.ctx().clone();
            let free = input.memory(|m| m.focused().is_none()) && !egui::Popup::is_any_open(&input);
            if free && draw::can_continue(&page) && input.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Enter)) {
                asks.push(draw::continue_ask(&page));
            }
            if !egui::Popup::is_any_open(&input) && input.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape)) {
                asks.push(Ask::Flow(Event::SkipSetup));
            }
        });
        self.settle(ctx, now_ms, asks);
    }
}

omuzaki_ui_api::register_panel! {
    id: "setup", title: "Setup", icon: icons::SPARKLE, region: omuzaki_ui_api::registry::Region::Left,
    min_size: [320.0, 160.0], order: 5,
    build: |_| Box::new(SetupPanel::default()),
}
