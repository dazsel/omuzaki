//! The shell itself (`shell.md` sections 1 to 4): the top bar with the workspace tabs, the docks, the status bar and the
//! global keys, around the panels of the active workspace.
//!
//! One [`Shell::ui`] call draws a frame. It is reactive: nothing here asks for a repaint, so an idle window costs no
//! CPU. egui repaints on input and on the animations the widgets run for themselves.
//!
//! What the person arranges is saved per workspace when a drag or click ends, on a background thread. When the window is
//! too small to show the saved arrangement (Library folded into the Outliner's tabs, Agent and History into the
//! Inspector's, the left dock a rail), the folded form is shown, and what the person changes there is saved under its
//! own name (`Fold::key`), so a small window never overwrites the layout made on a big one and each is remembered.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use egui::{
    Align, CentralPanel, Context, Frame, Key, Layout as Flow, Margin, Panel, Popup, Response, RichText, Ui, Vec2,
    WidgetInfo, WidgetType,
};
use egui_tiles::{Tile, Tree};
use omuzaki_ops_api::{Actor, ErrorCode, Event, Grants, OpError, OpRecord, Runner, SessionInfo, SessionState};
use omuzaki_ui_api::panel::{PanelCtx, ThemeKind as ApiTheme, ViewModels};
use omuzaki_ui_api::registry::{PanelDef, PanelKind, Region, Registry, WorkspaceDef};

use super::class::{DockMode, Plan, RAIL, STATUS_BAR, TOP_BAR};
use super::dock::{self, Dock, PanelAsk};
use super::handles;
use super::history::{HistoryLog, Preview, step_label};
use super::host::Bodies;
use super::keys::{self, Action};
use super::layout::{Known, Layout, PanelId};
use super::palette::{Command, Palette, Place, PlaceTarget};
use super::persist::LayoutStore;
use super::registry;
use super::requests::{ShellRequest, ShellRequests};
use super::saver::Saver;
use super::sheet;
use super::text::{Word, say, title};
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::ds;
use crate::ds::tokens::{ThemeKind, radius};
use crate::ds::widgets::{Button, TabBar, TabItem, Tip, TipExt as _};
use crate::ds::{a11y, icons};
use crate::panels::agent::approvals::{self, Answer};
use crate::panels::agent::fences::{FenceBoard, fence_chip};
use crate::panels::agent::review::Reviews;
use crate::panels::agent::{self as agent, AgentFeed, Agents};
use crate::panels::edit::EditFocus;
use crate::settings::SettingsWindow;

const MENU_ICON: &str = egui_phosphor::regular::LIST;
/// How many op-log records the History panel keeps (the newest), and how many it asks the runner for at a time.
const HISTORY_PAGE: usize = 500;
/// The panels the window size folds together (`class::Plan`).
const OUTLINER: &str = "outliner";
const LIBRARY: &str = "library";
const INSPECTOR: &str = "inspector";
const AGENT_HISTORY: [&str; 2] = ["agent", "history"];
/// The left dock's width when its class has no room to show it open: the width it takes once the window grows.
const LEFT_WHEN_RAIL: f32 = 240.0;

/// Which docks the window size folds away. Part of a view's identity: a new fold rebuilds the docking tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Fold {
    library_as_tab: bool,
    agent_history_merged: bool,
    left_rail: bool,
}

impl Fold {
    const NONE: Self = Self { library_as_tab: false, agent_history_merged: false, left_rail: false };

    fn of(plan: &Plan) -> Self {
        Self {
            library_as_tab: plan.library_as_tab,
            agent_history_merged: plan.agent_history_merged,
            left_rail: plan.left == DockMode::Rail,
        }
    }

    /// Every fold there is, the one with nothing folded first.
    fn all() -> impl Iterator<Item = Self> {
        (0..8_u8).map(|bits| Self {
            library_as_tab: bits & 1 != 0,
            agent_history_merged: bits & 2 != 0,
            left_rail: bits & 4 != 0,
        })
    }

    /// The name a layout is saved under: the workspace's own for the full arrangement, and one per fold for the
    /// arrangements of smaller windows, so each is kept apart from the others.
    fn key(self, workspace: &str) -> String {
        if self == Self::NONE {
            return workspace.to_owned();
        }
        let flags = [(self.library_as_tab, 'l'), (self.agent_history_merged, 'a'), (self.left_rail, 'r')];
        let letters: String = flags.iter().filter(|(on, _)| *on).map(|(_, c)| *c).collect();
        format!("{workspace}-compact-{letters}")
    }
}

/// One workspace's docking tree, built from its layout and the fold in force.
struct View {
    /// The arrangement made at full size: the workspace's own saved layout.
    saved: Layout,
    /// What the tree was last built from or read back as, to tell an edit from no change.
    shown: Layout,
    tree: Tree<PanelId>,
    fold: Fold,
    /// The left-dock panels a rail stands for.
    rail: Vec<&'static PanelDef>,
}

/// What the status bar says about the last undo or redo.
#[derive(Clone, Debug, Default)]
enum Note {
    #[default]
    Nothing,
    Undid(String),
    Redid(String),
    NothingToUndo,
    NothingToRedo,
    Failed(String),
    /// A sentence the shell said about something the person did (an agent paused).
    Said(String),
}

/// The app shell.
#[allow(clippy::struct_excessive_bools, reason = "independent switches of the shell's own state, not a mode")]
pub struct Shell {
    runner: Arc<dyn Runner>,
    /// Everything registered when the shell started: its workspaces, the panels they dock and the View menu's list.
    registry: Registry,
    /// The panels built so far.
    bodies: Bodies,
    /// What panels read besides the runner: today the op log ([`HistoryLog`]).
    models: ViewModels,
    /// Who panels act for.
    actor: Actor,
    /// The runner's event stream, until the first frame hands it to the thread that waits on it ([`wake_on_events`]).
    events: Option<Receiver<Event>>,
    /// Events that thread has received and no frame has taken yet.
    inbox: Arc<Mutex<Vec<Event>>>,
    /// The events of this frame, which every panel sees; refilled once per egui frame, not per pass.
    frame_events: Vec<Event>,
    /// The egui frame `frame_events` belongs to.
    events_frame: u64,
    store: Option<LayoutStore>,
    saver: Option<Saver>,
    /// The compact layouts changed this session, by name. A save is queued, not done, so a view rebuilt before the
    /// writer catches up reads these and not the older file.
    compact: HashMap<String, Layout>,
    /// Layouts reset this session whose file may not be deleted yet, by name. They read as absent until a new save is
    /// queued under the name, which the writer runs after the delete.
    forgotten: HashSet<String>,
    workspaces: Vec<&'static WorkspaceDef>,
    /// Each workspace's registered default layout, checked against the registry, in the order of `workspaces`.
    defaults: Vec<Layout>,
    views: Vec<Option<View>>,
    active: usize,
    window: Vec2,
    /// The width the docks share: the root `Ui`'s, which is the window's unless the app frames the shell.
    room: f32,
    locked: bool,
    maximised: bool,
    /// The next view is built from the defaults, not from disk (Reset layout).
    fresh: bool,
    focus: Option<String>,
    drawer: Option<String>,
    note: Note,
    palette: Palette,
    requests: ShellRequests,
    /// The window title says an approval waits, because one arrived while the window was in the background.
    attention: bool,
    /// The shortcut sheet is open.
    sheet: bool,
    /// The Settings window, while it is open.
    settings: Option<SettingsWindow>,
}

impl Shell {
    /// A shell over `runner`. Layouts are read from and saved to `store`; with none they live in memory only.
    pub fn new(runner: Arc<dyn Runner>, store: Option<LayoutStore>) -> Self {
        let registry = registry::collect();
        let (workspaces, defaults) = workspaces_of(&registry);
        let actor = Actor::Person { name: "you".to_owned() };
        let events = match runner.subscribe(&Grants::person()) {
            Ok(events) => Some(events),
            Err(error) => {
                tracing::warn!(%error, "panels will see no events");
                None
            }
        };
        let saver = store.clone().and_then(|store| match Saver::spawn(store) {
            Ok(saver) => Some(saver),
            Err(error) => {
                tracing::warn!(%error, "layouts will not be saved");
                None
            }
        });
        let requests = ShellRequests::default();
        let mut models = ViewModels::new();
        models.insert(requests.clone());
        models.insert(Preview::default());
        let mut shell = Self {
            runner,
            registry,
            bodies: Bodies::default(),
            models,
            actor,
            events,
            inbox: Arc::default(),
            frame_events: Vec::new(),
            events_frame: u64::MAX,
            store,
            saver,
            compact: HashMap::new(),
            forgotten: HashSet::new(),
            views: workspaces.iter().map(|_| None).collect(),
            workspaces,
            defaults,
            active: 0,
            window: super::class::MIN_WINDOW,
            room: super::class::MIN_WINDOW.x,
            locked: false,
            maximised: false,
            fresh: false,
            focus: None,
            drawer: None,
            note: Note::Nothing,
            attention: false,
            sheet: false,
            settings: None,
            palette: Palette::default(),
            requests,
        };
        shell.refresh_history();
        shell
    }

    /// Shares `value` with every panel as a view model ([`PanelCtx::view`]), replacing one of the same type. Panels
    /// built later read it when they are built; the app shares what its panels need (the open character, the GPU)
    /// before the first frame.
    pub fn insert_view<T: std::any::Any>(&mut self, value: T) {
        self.models.insert(value);
    }

    /// Keeps the command palette's recent ops in `file` between runs of the studio.
    pub fn keep_palette_recents_in(&mut self, file: PathBuf) {
        self.palette.keep_recents_in(file);
    }

    /// The command palette's recent ops, most recent first.
    pub fn palette_recents(&self) -> &[String] {
        self.palette.recents()
    }

    /// The queue panels and the app ask the shell through ([`ShellRequests`]); it is carried out each frame.
    pub fn requests(&self) -> ShellRequests {
        self.requests.clone()
    }

    /// What the shell found registered at startup, in the order it shows it: workspaces by group, then order, then id;
    /// panels by region, then order, then id.
    pub const fn registry(&self) -> &Registry {
        &self.registry
    }

    /// The id of the active workspace, if any is registered.
    pub fn active_workspace(&self) -> Option<&'static str> {
        self.workspaces.get(self.active).map(|w| w.id)
    }

    /// The arrangement the active workspace shows now, as it would be saved.
    #[cfg(test)]
    pub(super) fn shown_layout(&self) -> Option<Layout> {
        let view = self.views.get(self.active)?.as_ref()?;
        Layout::from_tree(&view.tree).map(|l| l.without(&[]))
    }

    /// Whether the layout is locked against dragging.
    pub const fn locked(&self) -> bool {
        self.locked
    }

    /// The whole document as History's preview holds it, while a past state is on show in the viewport.
    pub fn previewed(&self) -> Option<omuzaki_doc_api::Value> {
        self.models.get::<Preview>().and_then(|preview| preview.with(|v| v.map(|v| (*v.doc).clone())))
    }

    /// Where each docked panel of the active workspace was drawn last frame, by id: its body, under its tab strip. For
    /// a test or a tool that says which panel a widget is in.
    pub fn panel_rects(&self) -> Vec<(String, egui::Rect)> {
        let Some(view) = self.views.get(self.active).and_then(Option::as_ref) else { return Vec::new() };
        view.tree
            .active_tiles()
            .into_iter()
            .filter_map(|tile| match view.tree.tiles.get(tile) {
                Some(Tile::Pane(PanelId(id))) => Some((id.clone(), view.tree.tiles.rect(tile)?)),
                _ => None,
            })
            .collect()
    }

    /// The panel that has the keyboard's attention, if any.
    pub fn focused_panel(&self) -> Option<&str> {
        self.focus.as_deref()
    }

    /// Waits until every layout change so far is on disk. Layouts are written in the background and the last ones are
    /// written when the shell is dropped, so the app needs this only to read the files back.
    pub fn flush(&self) {
        if let Some(saver) = &self.saver {
            saver.flush();
        }
    }

    /// Draws one frame of the shell into `ui`, which spans the window.
    pub fn ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.window = ctx.content_rect().size();
        self.room = ui.available_width();
        self.pump_events(&ctx);
        // Ctrl+K works from anywhere, a text field included: it is how a person reaches every op.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::K)) {
            self.toggle_palette(&ctx);
        }
        if let Some(action) = keys::resolve(&ctx) {
            self.act(&ctx, action);
        }
        if self.maximised && !ctx.text_edit_focused() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.maximised = false;
        }
        let plan = Plan::for_window(self.window, 0.0);
        self.top_bar(ui, &plan);
        self.status_bar(ui);
        self.ensure_view(&plan);
        self.carry_out(&ctx, &plan);
        if self.views.get(self.active).is_some_and(|v| v.as_ref().is_some_and(|v| !v.rail.is_empty())) {
            self.rail(ui);
        }
        CentralPanel::default().frame(Frame::new().fill(ds(&ctx).theme.bg1)).show(ui, |ui| self.body(ui));
        self.approval_stack(&ctx);
        self.palette_ui(&ctx);
        if self.sheet {
            self.sheet = sheet::show(&ctx);
        }
        if let Some(window) = self.settings.as_mut()
            && !window.show(&ctx, self.runner.as_ref(), &self.actor)
        {
            self.settings = None;
        }
        self.save_if_edited(&ctx);
    }

    /// The Settings window, while it is open.
    pub const fn settings_window(&self) -> Option<&SettingsWindow> {
        self.settings.as_ref()
    }

    /// Opens the Settings window, or closes it when it is open (Ctrl+, and the menu).
    pub fn toggle_settings(&mut self) {
        self.settings = if self.settings.is_some() { None } else { Some(SettingsWindow::new()) };
    }

    /// Whether the shortcut sheet is open.
    pub const fn sheet_open(&self) -> bool {
        self.sheet
    }

    /// Whether the command palette is open.
    pub const fn palette_open(&self) -> bool {
        self.palette.is_open()
    }

    /// Opens the command palette, or closes it when it is open.
    fn toggle_palette(&mut self, ctx: &Context) {
        if self.palette.is_open() {
            self.palette.close();
            return;
        }
        let places = self.places();
        self.palette.open(self.runner.as_ref(), &places, ctx);
    }

    /// The workspaces and panels the palette finds.
    fn places(&self) -> Vec<Place> {
        let mut places: Vec<Place> = self
            .workspaces
            .iter()
            .enumerate()
            .map(|(n, w)| Place { target: PlaceTarget::Workspace(n), title: w.title })
            .collect();
        places
            .extend(self.registry.panels().iter().map(|p| Place { target: PlaceTarget::Panel(p.id), title: p.title }));
        places
    }

    /// Draws the palette over everything, and does what it chose.
    fn palette_ui(&mut self, ctx: &Context) {
        let focus = self.models.get::<EditFocus>().cloned();
        match self.palette.ui(ctx, self.runner.as_ref(), &self.actor, focus.as_ref()) {
            Some(Command::ShowPanel(id)) => self.show_panel(id),
            Some(Command::Workspace(n)) => self.select(n),
            None => {}
        }
    }

    /// Takes what the runner sent since the last frame, once per egui frame: egui may run `ui` again in the same frame
    /// when a pass is discarded, and every panel (in each pass) must see the same events.
    fn pump_events(&mut self, ctx: &Context) {
        if let Some(events) = self.events.take() {
            wake_on_events(events, &self.inbox, ctx);
        }
        let frame = ctx.cumulative_frame_nr();
        if frame == self.events_frame {
            return;
        }
        self.events_frame = frame;
        self.frame_events.clear();
        if let Ok(mut inbox) = self.inbox.lock() {
            self.frame_events.append(&mut inbox);
        }
        self.follow_log();
        self.follow_agents();
    }

    /// Keeps the studio's copy of the agent sessions and approvals current from this frame's events: loaded once from
    /// the registry, then only followed, so every view of agent work agrees in every frame.
    fn follow_agents(&mut self) {
        let Some(Agents(sessions)) = self.models.get::<Agents>().cloned() else { return };
        match self.models.get_mut::<AgentFeed>() {
            Some(feed) => {
                feed.follow(&self.frame_events);
            }
            None => {
                self.models.insert(AgentFeed::load(sessions.as_ref()));
            }
        }
    }

    /// The approvals as a stack at the top right of the window while the Agent panel is not on screen, and the window
    /// title's marker while one waits and the window is in the background.
    fn approval_stack(&mut self, ctx: &Context) {
        let Some(feed) = self.models.get::<AgentFeed>().cloned() else { return };
        let waiting = !feed.approvals().is_empty();
        let focused = ctx.input(|i| i.focused);
        if waiting && !focused && !self.attention {
            self.attention = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(say(ctx, Word::WindowWaiting, &[])));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestUserAttention(egui::UserAttentionType::Informational));
        } else if self.attention && (focused || !waiting) {
            self.attention = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(say(ctx, Word::WindowTitle, &[])));
        }
        if !waiting || self.bodies.on_screen(ctx, agent::ID) || self.palette.is_open() {
            return;
        }
        let desk = self.models.get::<Reviews>().cloned();
        let (answer, open_panel) = approvals::stack(ctx, TOP_BAR, &feed, desk.as_ref().map(|d| d.0.as_ref()));
        if open_panel {
            self.show_panel(agent::ID);
        }
        let Some(Agents(sessions)) = self.models.get::<Agents>().cloned() else { return };
        let result = match answer {
            Some(Answer::Decide(ids, decision)) => {
                approvals::resolve_all(sessions.as_ref(), &ids, &decision, &self.actor)
            }
            Some(Answer::Export(id)) => {
                let approval = feed.approvals().iter().find(|a| a.id == id);
                match (desk, approval) {
                    (Some(desk), Some(approval)) => desk.0.export(approval).map(drop),
                    _ => Err(OpError::unavailable("review")),
                }
            }
            None => Ok(()),
        };
        if let Err(error) = result {
            self.note = Note::Failed(error.message);
        }
    }

    /// Keeps History's copy of the op log current from this frame's events, so an agent's op over MCP shows up at
    /// once: an applied op is appended from its event (no read of the log), and a rollback, which takes records back
    /// out, reads the log again.
    fn follow_log(&mut self) {
        if self.frame_events.iter().any(|e| matches!(e, Event::TxRolledBack { .. })) {
            return self.refresh_history();
        }
        let Some(log) = self.models.get_mut::<HistoryLog>() else { return };
        for event in &self.frame_events {
            match event {
                Event::OpApplied { record, .. } if log.records.last().is_none_or(|last| record.seq > last.seq) => {
                    log.seen.insert(record.seq, Instant::now());
                    log.records.push(record.clone());
                }
                Event::Undone { tx, .. } => {
                    log.undone.insert(*tx);
                }
                Event::Redone { tx, .. } => {
                    log.undone.remove(tx);
                }
                _ => {}
            }
        }
        let over = log.records.len().saturating_sub(HISTORY_PAGE);
        for gone in log.records.drain(..over) {
            log.seen.remove(&gone.seq);
        }
    }

    fn act(&mut self, ctx: &Context, action: Action) {
        match action {
            Action::Workspace(n) => self.select(n),
            Action::Theatre => {
                if let Some(n) = self.workspaces.iter().position(|w| w.id == "theatre") {
                    self.select(n);
                }
            }
            Action::Undo | Action::Redo => self.step_history(action == Action::Undo),
            Action::ToggleLock => self.locked = !self.locked,
            Action::ToggleMaximise => self.maximised = !self.maximised && self.focus.is_some(),
            Action::NextPanel => self.cycle_focus(1),
            Action::PreviousPanel => self.cycle_focus(-1),
            Action::ShortcutSheet => self.sheet = !self.sheet,
            Action::Settings => self.toggle_settings(),
            Action::PauseAgent => self.pause_agent(ctx),
        }
    }

    /// The agent the person means by "the active agent": the newest one working, else the newest one paused.
    fn active_agent(&self) -> Option<SessionInfo> {
        let feed = self.models.get::<AgentFeed>()?;
        let newest = |paused: bool| {
            feed.live().filter(|s| matches!(s.state, SessionState::Paused { .. }) == paused).last().cloned()
        };
        newest(false).or_else(|| newest(true))
    }

    /// Pauses the active agent, or resumes it when it is paused (Ctrl+Alt+P), and says so in the status bar.
    fn pause_agent(&mut self, ctx: &Context) {
        let (Some(Agents(sessions)), Some(session)) = (self.models.get::<Agents>().cloned(), self.active_agent())
        else {
            self.note = Note::Said(say(ctx, Word::StatusNoAgent, &[]));
            return;
        };
        let name = session.client.as_str().to_owned();
        let paused = matches!(session.state, SessionState::Paused { .. });
        let result =
            if paused { sessions.resume(session.id, &self.actor) } else { sessions.pause(session.id, &self.actor) };
        self.note = match result {
            Ok(_) if paused => Note::Said(say(ctx, Word::StatusAgentResumed, &[("name", &name)])),
            Ok(_) => Note::Said(say(ctx, Word::StatusAgentPaused, &[("name", &name)])),
            Err(error) => Note::Failed(error.message),
        };
    }

    fn select(&mut self, index: usize) {
        if index < self.workspaces.len() {
            self.active = index;
            // Focus, the maximised panel and the drawer belong to the workspace they were made in.
            self.maximised = false;
            self.focus = None;
            self.drawer = None;
        }
    }

    fn step_history(&mut self, undo: bool) {
        let grants = Grants::person();
        let result = if undo { self.runner.undo(&self.actor, &grants) } else { self.runner.redo(&self.actor, &grants) };
        // A step a later change depends on can't be undone alone: History shows why on a card until it is dismissed or
        // a step goes through.
        let conflict = result.as_ref().err().filter(|e| e.code == ErrorCode::Conflict).map(|e| e.message.clone());
        self.refresh_history();
        if let Some(log) = self.models.get_mut::<HistoryLog>() {
            log.conflict = conflict;
        }
        self.note = match (result, undo) {
            (Ok(Some(label)), true) => Note::Undid(label),
            (Ok(Some(label)), false) => Note::Redid(label),
            (Ok(None), true) => Note::NothingToUndo,
            (Ok(None), false) => Note::NothingToRedo,
            (Err(error), _) => Note::Failed(error.message),
        };
    }

    /// Reads the op log again: after an undo or redo here, and when the runner says the log changed.
    fn refresh_history(&mut self) {
        let log = match newest_records(self.runner.as_ref(), HISTORY_PAGE) {
            Ok(records) => HistoryLog { records, ..HistoryLog::default() },
            Err(error) => HistoryLog { error: Some(error.message), ..HistoryLog::default() },
        };
        // Which steps are undone is known only from the events, so it outlives a read of the log.
        let old = self
            .models
            .get_mut::<HistoryLog>()
            .map(|old| (std::mem::take(&mut old.undone), std::mem::take(&mut old.seen), old.conflict.take()));
        let (undone, mut seen, conflict) = old.unwrap_or_default();
        seen.retain(|seq, _| log.records.iter().any(|r| r.seq == *seq));
        let log = HistoryLog { undone, seen, conflict, ..log };
        self.models.insert(log);
    }

    /// Moves the focus to the next (or previous) panel of the workspace and shows it: a panel behind a tab is brought
    /// to the front, so what has the focus is always what is on screen.
    fn cycle_focus(&mut self, step: isize) {
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        let panels = view.shown.panels();
        if panels.is_empty() {
            return;
        }
        let at = self.focus.as_deref().and_then(|f| panels.iter().position(|p| *p == f));
        let next = at.map_or(0, |i| (i.cast_signed() + step).rem_euclid(panels.len().cast_signed()).cast_unsigned());
        let Some(target) = panels.get(next).map(|p| (*p).to_owned()) else { return };
        view.tree.make_active(|_, tile| matches!(tile, Tile::Pane(PanelId(panel)) if *panel == target));
        self.focus = Some(target);
    }

    /// Builds the active workspace's docking tree the first time, and again when the fold in force changes.
    fn ensure_view(&mut self, plan: &Plan) {
        let fold = Fold::of(plan);
        let Some(&workspace) = self.workspaces.get(self.active) else { return };
        let from_disk = !std::mem::take(&mut self.fresh);
        let saved = match self.views.get_mut(self.active).and_then(Option::take) {
            Some(view) if view.fold == fold => {
                self.views[self.active] = Some(view);
                self.fresh = !from_disk;
                return;
            }
            Some(view) => view.saved,
            None => self.full_layout(self.active, plan, from_disk),
        };
        let rail = if fold.left_rail { self.rail_of(&saved) } else { Vec::new() };
        let shown = if fold == Fold::NONE {
            saved.clone()
        } else {
            let derived = self.folded_layout(&saved, fold, &rail, plan);
            let key = fold.key(workspace.id);
            let stored =
                from_disk.then(|| self.compact.get(&key).cloned().or_else(|| self.stored(&key, &derived))).flatten();
            stored.map_or(derived, |l| l.without(&rail.iter().map(|p| p.id).collect::<Vec<_>>()))
        };
        // The panel the person was working in stays open when the fold changes: it becomes the drawer when its dock
        // folds into the rail, and the tab in front, focused, when it is docked again.
        let front = self.focus.clone().or_else(|| self.drawer.clone());
        if let Some(front) = front.as_deref().filter(|f| !shown.contains(f) && rail.iter().any(|p| p.id == *f)) {
            self.drawer = Some(front.to_owned());
        }
        // What was focused, maximised or open as a drawer may not be in the arrangement that is shown now.
        if self.focus.as_deref().is_some_and(|f| !shown.contains(f)) {
            self.focus = None;
            self.maximised = false;
        }
        if self.drawer.as_deref().is_some_and(|d| !rail.iter().any(|p| p.id == d)) {
            self.drawer = None;
        }
        let mut tree = shown.to_tree(egui::Id::new(("shell_tree", workspace.id)));
        if let Some(front) = front.filter(|f| shown.contains(f)) {
            tree.make_active(|_, tile| matches!(tile, Tile::Pane(PanelId(panel)) if *panel == front));
            self.focus = Some(front);
        }
        self.views[self.active] = Some(View { saved, shown, tree, fold, rail });
    }

    /// The left-dock panels of `saved`: what the rail stands for when the left dock is folded.
    fn rail_of(&self, saved: &Layout) -> Vec<&'static PanelDef> {
        let panels = saved.panels().into_iter().filter_map(|p| self.registry.panel(p));
        panels.filter(|p| p.region == Region::Left).collect()
    }

    /// The layout of `workspace` at full size: the saved one made safe, or the default sized for this window.
    fn full_layout(&self, workspace: usize, plan: &Plan, from_disk: bool) -> Layout {
        let default = self.defaults[workspace].clone().with_root_widths(&dock_widths(plan, &self.registry), self.room);
        let saved = from_disk.then(|| self.stored(self.workspaces[workspace].id, &default)).flatten();
        saved.unwrap_or(default)
    }

    /// The arrangement for a window that folds some docks: the full one with those docks folded, sized for the window.
    fn folded_layout(&self, saved: &Layout, fold: Fold, rail: &[&PanelDef], plan: &Plan) -> Layout {
        let mut shown = saved.clone();
        if fold.library_as_tab {
            shown = shown.folded(OUTLINER, &[LIBRARY]);
        }
        if fold.agent_history_merged {
            shown = shown.folded(INSPECTOR, &AGENT_HISTORY);
        }
        let hidden: Vec<&str> = rail.iter().map(|p| p.id).collect();
        let rail_width = if hidden.is_empty() { 0.0 } else { RAIL };
        // Panels removed with the rail leave their shares behind, so the others are sized again.
        shown.without(&hidden).with_root_widths(&dock_widths(plan, &self.registry), self.room - rail_width)
    }

    /// The layout saved under `key`, repaired against the registry; `None` when there is none or it is unreadable.
    pub(super) fn stored(&self, key: &str, default: &Layout) -> Option<Layout> {
        if self.forgotten.contains(key) {
            return None;
        }
        // A workspace that registered no primary panel is reported at startup; its saved layout then only has to be
        // made of panels that exist.
        let primary = default
            .panels()
            .into_iter()
            .find_map(|p| self.registry.panel(p).filter(|def| def.kind == PanelKind::Primary).map(|def| def.id));
        let known = Known { exists: &|p| self.registry.panel(p).is_some(), primary };
        match self.store.as_ref()?.load(key) {
            Ok(saved) => saved.map(|saved| saved.repaired(&known, default)),
            Err(error) => {
                tracing::warn!(layout = key, %error, "using the default layout");
                None
            }
        }
    }

    fn body(&mut self, ui: &mut Ui) {
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        let ctx = ui.ctx().clone();
        let panel_ctx = panel_context(self.runner.as_ref(), &self.actor, &self.frame_events, &self.models, &ctx);
        let maximised = self
            .maximised
            .then(|| self.focus.as_deref().filter(|f| view.shown.contains(f)).and_then(|f| self.registry.panel(f)))
            .flatten();
        if let Some(def) = maximised {
            self.bodies.draw(ui, def, &panel_ctx);
            return;
        }
        let rect = ui.max_rect();
        dock::enforce_minimums(&mut view.tree, &self.registry);
        let pinned = dock::pinned(&view.tree, &self.registry);
        let mut asks = Vec::new();
        let primary = view
            .shown
            .panels()
            .into_iter()
            .find_map(|p| self.registry.panel(p).filter(|d| d.kind == PanelKind::Primary).map(|d| d.id));
        let sides: Vec<(String, Option<bool>)> = view
            .shown
            .panels()
            .into_iter()
            .map(|p| (p.to_owned(), primary.and_then(|primary| view.shown.side_of(p, primary))))
            .collect();
        let mut dock = Dock {
            ctx: &ctx,
            registry: &self.registry,
            bodies: &mut self.bodies,
            panel_ctx: &panel_ctx,
            locked: self.locked,
            pinned: &pinned,
            focus: &mut self.focus,
            asks: &mut asks,
            sides: &sides,
            menus: Vec::new(),
        };
        view.tree.ui(&mut dock, ui);
        let moved = handles::name_handles(ui, &mut view.tree, &self.registry);
        let moved = moved.then(|| Layout::from_tree(&view.tree).map(|l| l.without(&[]))).flatten();
        self.drawer(&ctx, rect);
        if let Some(layout) = moved {
            self.record(&layout);
        }
        for ask in asks {
            self.panel_ask(ask);
        }
    }

    /// Carries out what a panel's "⋯" menu asked for.
    fn panel_ask(&mut self, ask: PanelAsk) {
        let (panel, left) = match ask {
            PanelAsk::Reset => return self.reset_layout(),
            PanelAsk::Close(panel) => {
                if let Some(def) = self.registry.panel(&panel) {
                    self.close_panel(def.id);
                }
                return;
            }
            PanelAsk::MoveLeft(panel) => (panel, true),
            PanelAsk::MoveRight(panel) => (panel, false),
        };
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        let primary = view
            .shown
            .panels()
            .into_iter()
            .find_map(|p| self.registry.panel(p).filter(|d| d.kind == PanelKind::Primary).map(|d| d.id));
        let Some(primary) = primary else { return };
        let layout = view.shown.moved(&panel, left, primary);
        if layout == view.shown {
            return;
        }
        view.tree = layout.to_tree(egui::Id::new(("shell_tree", self.workspaces[self.active].id)));
        view.tree.make_active(|_, tile| matches!(tile, Tile::Pane(PanelId(p)) if *p == panel));
        self.record(&layout);
        self.focus = Some(panel);
    }

    /// Saves the arrangement when a drag or click ends and it differs from the one before. The full-size arrangement
    /// is the workspace's own layout; a smaller window's is kept under its own name, so neither overwrites the other.
    fn save_if_edited(&mut self, ctx: &Context) {
        if !ctx.input(|i| i.pointer.any_released()) {
            return;
        }
        let Some(view) = self.views.get(self.active).and_then(Option::as_ref) else { return };
        let Some(current) = Layout::from_tree(&view.tree).map(|l| l.without(&[])) else {
            debug_assert!(false, "a docking tree always has a root");
            return;
        };
        if current != view.shown {
            self.record(&current);
        }
    }

    /// Takes `current` as the arrangement the active workspace now shows and queues it to be saved: under the
    /// workspace's own name at full size, and under the fold's name for a smaller window.
    fn record(&mut self, current: &Layout) {
        let Some(&workspace) = self.workspaces.get(self.active) else { return };
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        view.shown = current.clone();
        if view.fold == Fold::NONE {
            view.saved = current.clone();
        }
        let key = view.fold.key(workspace.id);
        if view.fold != Fold::NONE {
            self.compact.insert(key.clone(), current.clone());
        }
        if let Some(saver) = &self.saver {
            saver.save(&key, current);
            self.forgotten.remove(&key);
        }
    }

    /// Takes `layout` as the full-size arrangement of the active workspace, even while a smaller window shows a folded
    /// one, and queues it to be saved under the workspace's own name.
    fn record_full(&mut self, layout: &Layout) {
        let Some(&workspace) = self.workspaces.get(self.active) else { return };
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        view.saved = layout.clone();
        if let Some(saver) = &self.saver {
            saver.save(workspace.id, layout);
            self.forgotten.remove(workspace.id);
        }
    }

    /// Shows the panel registered as `id` in the active workspace (View, Show panel): a panel in the arrangement is
    /// brought to the front and focused; one the arrangement lacks joins the tab group of a panel of its region, or
    /// gets a column of its own; a left panel in a window that folds the left dock opens as the drawer.
    pub fn show_panel(&mut self, id: &str) {
        let Some(def) = self.registry.panel(id) else { return };
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        if view.fold.left_rail && def.region == Region::Left {
            // The rail stands for the left panels of the full-size arrangement, so a left panel it lacks is added there
            // and saved under the workspace's own name: it survives a resize that unfolds the dock and a restart.
            if !view.saved.contains(id) {
                let saved = view.saved.clone();
                let anchor = saved.panels().into_iter().find(|p| {
                    self.registry.panel(p).is_some_and(|d| d.region == Region::Left && d.kind != PanelKind::Primary)
                });
                let layout = saved.with_panel(id, anchor, true);
                self.record_full(&layout);
            }
            let rail = self.views.get(self.active).and_then(Option::as_ref).map(|v| self.rail_of(&v.saved));
            if let (Some(rail), Some(view)) = (rail, self.views.get_mut(self.active).and_then(Option::as_mut)) {
                view.rail = rail;
            }
            self.drawer = Some(id.to_owned());
            return;
        }
        if !view.shown.contains(id) {
            let anchor = view.shown.panels().into_iter().find(|p| {
                *p != id
                    && self.registry.panel(p).is_some_and(|d| d.region == def.region && d.kind != PanelKind::Primary)
            });
            let layout = view.shown.with_panel(id, anchor, def.region == Region::Left);
            let key = egui::Id::new(("shell_tree", self.workspaces[self.active].id));
            view.tree = layout.to_tree(key);
            let folded = view.fold != Fold::NONE && !view.saved.contains(id);
            let saved = view.saved.clone();
            self.record(&layout);
            // A smaller window keeps its arrangement apart from the full-size one, so the panel joins that too: a resize
            // that changes the fold builds the view again from it.
            if folded {
                let anchor = saved.panels().into_iter().find(|p| {
                    self.registry.panel(p).is_some_and(|d| d.region == def.region && d.kind != PanelKind::Primary)
                });
                self.record_full(&saved.with_panel(id, anchor, def.region == Region::Left));
            }
        }
        if let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) {
            view.tree.make_active(|_, tile| matches!(tile, Tile::Pane(PanelId(panel)) if panel == id));
        }
        self.focus = Some(id.to_owned());
    }

    /// Takes the panel registered as `id` out of the active workspace's arrangement; the rest closes up around it. A
    /// panel the arrangement lacks, or the last one left, stays as it is.
    pub fn close_panel(&mut self, id: &str) {
        let Some(&workspace) = self.workspaces.get(self.active) else { return };
        let Some(view) = self.views.get_mut(self.active).and_then(Option::as_mut) else { return };
        // A smaller window's arrangement is kept apart from the full-size one (and a folded left dock shows neither),
        // so the panel leaves both: a resize that changes the fold builds the view again from the full-size one.
        let saved = (view.fold != Fold::NONE && view.saved.contains(id)).then(|| view.saved.without(&[id]));
        if view.shown.contains(id) {
            let layout = view.shown.without(&[id]);
            if layout.contains(id) {
                return;
            }
            view.tree = layout.to_tree(egui::Id::new(("shell_tree", workspace.id)));
            self.record(&layout);
        }
        if let Some(saved) = saved.filter(|l| !l.contains(id)) {
            self.record_full(&saved);
            let rail = self.views.get(self.active).and_then(Option::as_ref).map(|v| self.rail_of(&v.saved));
            if let (Some(rail), Some(view)) = (rail, self.views.get_mut(self.active).and_then(Option::as_mut)) {
                view.rail = rail;
            }
        }
        if self.focus.as_deref() == Some(id) {
            self.focus = None;
        }
        if self.drawer.as_deref() == Some(id) {
            self.drawer = None;
        }
    }

    /// Carries out what panels and the app asked for since the last frame ([`ShellRequests`]).
    fn carry_out(&mut self, ctx: &Context, plan: &Plan) {
        for request in self.requests.take() {
            match request {
                ShellRequest::Workspace(id) => {
                    if let Some(index) = self.workspaces.iter().position(|w| w.id == id) {
                        self.select(index);
                        self.ensure_view(plan);
                    }
                }
                ShellRequest::ShowPanel(id) => self.show_panel(id),
                ShellRequest::Undo => self.step_history(true),
                ShellRequest::Redo => self.step_history(false),
                ShellRequest::DismissConflict => {
                    if let Some(log) = self.models.get_mut::<HistoryLog>() {
                        log.conflict = None;
                    }
                }
                ShellRequest::ClosePanel(id) => self.close_panel(id),
                ShellRequest::Adjust { op, args } => {
                    let places = self.places();
                    self.palette.adjust(self.runner.as_ref(), &places, ctx, &op, &args);
                }
            }
        }
    }

    /// Drops the background saver, as when the operating system refused to start its thread.
    #[cfg(test)]
    pub(super) fn lose_saver(&mut self) {
        self.saver = None;
    }

    /// Forgets every saved layout of the active workspace, the compact ones included; the default is built on the
    /// next frame.
    pub fn reset_layout(&mut self) {
        let Some(&workspace) = self.workspaces.get(self.active) else { return };
        let keys: Vec<String> = Fold::all().map(|fold| fold.key(workspace.id)).collect();
        if let Some(saver) = &self.saver {
            for key in &keys {
                saver.reset(key);
            }
        }
        // Remembered even when no saver could start: the files stay on disk, and must not be read back this session.
        self.forgotten.extend(keys.iter().cloned());
        self.compact.retain(|key, _| !keys.contains(key));
        self.views[self.active] = None;
        self.fresh = true;
        self.maximised = false;
    }

    fn top_bar(&mut self, ui: &mut Ui, plan: &Plan) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let frame = Frame::new().fill(ds.theme.bg2).inner_margin(Margin::symmetric(8, 0));
        Panel::top("shell_top").exact_size(TOP_BAR).frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                self.menu(ui);
                ui.label(RichText::new(say(&ctx, Word::ProjectName, &[])).font(ds.text.strong.clone()));
                ui.separator();
                // Setup is the whole task while it shows (`shell.md` section 14): the workspace tabs come back when it
                // ends or is skipped.
                if !self.bodies.on_screen_before(&ctx, crate::panels::setup::ID) {
                    self.workspace_tabs(ui, plan);
                }
                // At the bar's right end, in reading order: Tab meets search, then the agents, then Go Live.
                let search = say(&ctx, Word::SearchOrRun, &[]);
                let (agents, waiting) = self.agent_chip_label(&ctx);
                let off = say(&ctx, Word::GoLiveOff, &[]);
                let go_live = say(&ctx, Word::GoLive, &[]);
                let key = (&search, &agents, waiting, &go_live, plan.go_live_icon);
                a11y::trailing(ui, "shell_top_trailing", key, |ui| {
                    if ui.add(Button::new(&search).icon(icons::MAGNIFYING_GLASS).shortcut_hint("Ctrl K")).clicked() {
                        self.toggle_palette(&ctx);
                    }
                    let button = Button::new(&agents).icon(icons::ROBOT);
                    // Quiet while nothing waits; a raised button while a request waits for the person.
                    if ui.add(if waiting { button } else { button.ghost() }).clicked() {
                        self.show_panel(agent::ID);
                    }
                    if plan.go_live_icon {
                        icon_button(ui, icons::BROADCAST, &Tip::new(&go_live).detail(&off), false);
                    } else {
                        ui.add(Button::new(&go_live).icon(icons::BROADCAST).disabled(&off));
                    }
                });
            });
        });
    }

    fn menu(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let name = say(&ctx, Word::MenuName, &[]);
        let button = icon_button(ui, MENU_ICON, &Tip::new(&name), true);
        Popup::menu(&button).show(|ui| {
            let heading = |ui: &mut Ui, word: Word| {
                ui.label(RichText::new(say(&ctx, word, &[])).font(ds(&ctx).text.strong.clone()));
            };
            heading(ui, Word::MenuEdit);
            let next = self.models.get::<HistoryLog>().and_then(|log| log.next_undo(&self.actor)).map(step_label);
            let nothing = say(&ctx, Word::MenuNothingToUndo, &[]);
            let undo = next.as_ref().map_or_else(
                || say(&ctx, Word::SheetUndo, &[]),
                |label| say(&ctx, Word::MenuUndo, &[("label", label)]),
            );
            let button = Button::new(&undo).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE).shortcut_hint("Ctrl Z");
            if ui.add(button.disabled_if(next.is_none(), &nothing)).clicked() {
                self.step_history(true);
            }
            let redo = say(&ctx, Word::MenuRedo, &[]);
            if ui.add(Button::new(&redo).ghost().icon(icons::ARROW_CLOCKWISE).shortcut_hint("Ctrl Shift Z")).clicked() {
                self.step_history(false);
            }
            ui.separator();
            heading(ui, Word::MenuView);
            let lock = say(&ctx, Word::LockLayout, &[]);
            let glyph = if self.locked { icons::LOCK } else { icons::LOCK_OPEN };
            if ui.add(Button::new(&lock).ghost().icon(glyph).shortcut_hint("Ctrl Shift L")).clicked() {
                self.locked = !self.locked;
            }
            if ui
                .add(Button::new(&say(&ctx, Word::ResetLayout, &[])).ghost().icon(icons::ARROW_COUNTER_CLOCKWISE))
                .clicked()
            {
                self.reset_layout();
            }
            // Every registered panel, in the registry's order, in a submenu so the menu stays short; a panel the
            // person closed returns here.
            let panels: Vec<&'static PanelDef> = self.registry.panels().to_vec();
            let show_panel = say(&ctx, Word::ShowPanel, &[]);
            let submenu = ui.menu_button(&show_panel, |ui| {
                for def in panels {
                    let name = title(&ctx, &self.bodies.title(def)).into_owned();
                    if ui.add(Button::new(&name).ghost().icon(def.icon)).clicked() {
                        self.show_panel(def.id);
                    }
                }
            });
            // The submenu's arrow is drawn, not read out: its name is the words.
            submenu.response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, true, &show_panel));
            ui.separator();
            heading(ui, Word::MenuAgents);
            if ui.add(Button::new(&say(&ctx, Word::MenuConnectAgent, &[])).ghost().icon(icons::ROBOT)).clicked() {
                self.show_panel(agent::ID);
            }
            if let Some(session) = self.active_agent() {
                let name = session.client.as_str();
                let paused = matches!(session.state, SessionState::Paused { .. });
                let (word, glyph) =
                    if paused { (Word::MenuResumeAgent, icons::PLAY) } else { (Word::MenuPauseAgent, icons::PAUSE) };
                let label = say(&ctx, word, &[("name", name)]);
                if ui.add(Button::new(&label).ghost().icon(glyph).shortcut_hint("Ctrl Alt P")).clicked() {
                    self.pause_agent(&ctx);
                }
            }
            ui.separator();
            let settings = say(&ctx, Word::MenuSettings, &[]);
            if ui.add(Button::new(&settings).ghost().icon(icons::GEAR).shortcut_hint("Ctrl ,")).clicked() {
                self.toggle_settings();
            }
            ui.separator();
            heading(ui, Word::MenuHelp);
            if ui.add(Button::new(&say(&ctx, Word::MenuRunSetup, &[])).ghost().icon(icons::SPARKLE)).clicked() {
                self.requests.push(ShellRequest::Workspace(crate::live::registration::PERFORM_WORKSPACE));
                self.requests.push(ShellRequest::ShowPanel(crate::panels::setup::ID));
            }
            let shortcuts = say(&ctx, Word::MenuShortcuts, &[]);
            if ui.add(Button::new(&shortcuts).ghost().icon(icons::KEYBOARD).shortcut_hint("Ctrl /")).clicked() {
                self.sheet = true;
            }
        });
    }

    fn workspace_tabs(&mut self, ui: &mut Ui, plan: &Plan) {
        let ctx = ui.ctx().clone();
        let titles: Vec<_> = self.workspaces.iter().map(|w| title(&ctx, w.title)).collect();
        let items: Vec<TabItem<'_>> = self
            .workspaces
            .iter()
            .zip(&titles)
            .enumerate()
            .map(|(i, (w, label))| {
                let item = TabItem::new(label).icon(w.icon);
                if plan.icon_tabs && i != self.active { item.icon_only() } else { item }
            })
            .collect();
        let mut selected = self.active;
        let label = say(&ctx, Word::WorkspaceTabs, &[]);
        let output = TabBar::new(&label, &mut selected, &items).id_salt("shell_workspaces").show(ui);
        if output.response.changed() {
            self.select(selected);
        }
    }

    /// The top bar's agent chip: how many agents are connected, or how many requests wait for the person. True when
    /// it was clicked (it opens the Agent panel).
    /// The agent chip's words, and whether a request waits for the person.
    fn agent_chip_label(&self, ctx: &egui::Context) -> (String, bool) {
        let ctx = ctx.clone();
        let feed = self.models.get::<AgentFeed>();
        let live = feed.map_or(0, |f| f.live().count());
        let waiting = feed.map_or(0, |f| f.approvals().len());
        let label = match (live, waiting) {
            (_, w) if w > 0 => say(&ctx, Word::AgentChipWaiting, &[("count", &w.to_string())]),
            (0, _) => say(&ctx, Word::AgentChip, &[]),
            (n, _) => say(&ctx, Word::AgentChipCount, &[("count", &n.to_string())]),
        };
        (label, waiting > 0)
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let frame = Frame::new().fill(ds.theme.bg2).inner_margin(Margin::symmetric(8, 0));
        let next = self.models.get::<HistoryLog>().and_then(|log| log.next_undo(&self.actor)).map(step_label);
        let undo = match (&self.note, next) {
            (Note::Nothing | Note::NothingToUndo, Some(label)) => say(&ctx, Word::StatusCanUndo, &[("label", &label)]),
            (Note::Nothing | Note::NothingToUndo, None) => say(&ctx, Word::StatusNothingToUndo, &[]),
            (Note::Undid(label), _) => say(&ctx, Word::StatusUndid, &[("label", label)]),
            (Note::Redid(label), _) => say(&ctx, Word::StatusRedid, &[("label", label)]),
            (Note::NothingToRedo, _) => say(&ctx, Word::StatusNothingToRedo, &[]),
            (Note::Failed(why) | Note::Said(why), _) => why.clone(),
        };
        let selection = match self.models.get::<EditFocus>().map(|f| (f.count(), f.primary())) {
            Some((1, Some(part))) => say(&ctx, Word::StatusSelected, &[("part", part.as_str())]),
            Some((n, _)) if n > 1 => say(&ctx, Word::StatusSelectedMany, &[("count", &n.to_string())]),
            _ => say(&ctx, Word::StatusSelection, &[]),
        };
        let mut show_agent = false;
        Panel::bottom("shell_status").exact_size(STATUS_BAR).frame(frame).show(ui, |ui| {
            ui.horizontal_centered(|ui| {
                let small = |text: String| RichText::new(text).font(ds.text.small.clone()).color(ds.theme.text2);
                ui.label(small(selection));
                ui.separator();
                ui.label(small(undo));
                ui.separator();
                let board = self.models.get::<AgentFeed>().map(FenceBoard::from_feed).unwrap_or_default();
                let live = self.models.get::<AgentFeed>().is_some_and(|f| f.live().next().is_some());
                if !board.is_empty() {
                    if fence_chip(ui, &board).clicked() {
                        show_agent = true;
                    }
                } else if live {
                    ui.label(small(say(&ctx, Word::StatusNoFence, &[])));
                } else {
                    ui.label(small(say(&ctx, Word::StatusFence, &[])));
                }
                ui.with_layout(Flow::right_to_left(Align::Center), |ui| {
                    ui.label(small(say(&ctx, Word::StatusHealth, &[])));
                });
            });
        });
        if show_agent {
            self.show_panel(agent::ID);
        }
    }

    /// The icon rail that stands for a collapsed left dock; a button opens the panel as a drawer over the viewport.
    fn rail(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        let Some(view) = self.views.get(self.active).and_then(Option::as_ref) else { return };
        let frame = Frame::new().fill(ds(&ctx).theme.bg1);
        Panel::left("shell_rail").exact_size(RAIL).resizable(false).frame(frame).show(ui, |ui| {
            ui.vertical_centered(|ui| {
                for def in &view.rail {
                    let name = say(&ctx, Word::OpenDrawer, &[("panel", &title(&ctx, &self.bodies.title(def)))]);
                    if icon_button(ui, def.icon, &Tip::new(&name), true).clicked() {
                        self.drawer =
                            if self.drawer.as_deref() == Some(def.id) { None } else { Some(def.id.to_owned()) };
                    }
                }
            });
        });
    }

    /// The panel opened from the rail, over the left edge of the viewport. A click outside or Esc closes it.
    fn drawer(&mut self, ctx: &Context, rect: egui::Rect) {
        let Some(def) = self.drawer.as_deref().and_then(|d| self.registry.panel(d)) else { return };
        let panel_context = panel_context(self.runner.as_ref(), &self.actor, &self.frame_events, &self.models, ctx);
        let ds = ds(ctx);
        let area = egui::Area::new(egui::Id::new("shell_drawer"))
            .order(egui::Order::Foreground)
            .fixed_pos(rect.min)
            .show(ctx, |ui| {
                Frame::new().fill(ds.theme.bg1).stroke(egui::Stroke::new(1.0, ds.theme.line)).show(ui, |ui| {
                    ui.set_min_size(Vec2::new(LEFT_WHEN_RAIL, rect.height()));
                    ui.set_max_size(Vec2::new(LEFT_WHEN_RAIL, rect.height()));
                    self.bodies.draw(ui, def, &panel_context);
                });
            });
        let outside = ctx.input(|i| i.pointer.any_pressed()) && !area.response.contains_pointer();
        if outside || ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.drawer = None;
        }
    }
}

/// An icon-only button: its tooltip is also its accessible name (`design-system.md`, Tooltips).
pub(super) fn icon_button(ui: &mut Ui, glyph: &str, tip: &Tip<'_>, enabled: bool) -> Response {
    let ds = ds(ui.ctx());
    let colour = if enabled { ds.theme.text } else { ds.theme.text_disabled };
    let label = RichText::new(glyph).font(ds.text.icon(16.0, false)).color(colour);
    let response =
        ui.add_enabled(enabled, egui::Button::new(label).frame(false).min_size(Vec2::splat(ds.metrics.icon_button)));
    // The glyph is not a name: the tooltip's words are what a screen reader says.
    a11y::describe(&response, || WidgetInfo::labeled(WidgetType::Button, enabled, tip.name), |_| {});
    paint::focus_ring(ui.painter(), &ds.theme, &response, response.rect, radius::CONTROL, RingPlacement::Outside);
    response.tip(*tip)
}

/// Moves the runner's events into `inbox` as they arrive and asks egui for a frame each time, so an event reaches the
/// panels while the window is otherwise idle (the shell never repaints on its own). The thread blocks on the stream
/// and so costs nothing at rest; it ends when the runner closes the stream, or, after the next event, once the shell
/// that owns `inbox` is gone.
fn wake_on_events(events: Receiver<Event>, inbox: &Arc<Mutex<Vec<Event>>>, ctx: &Context) {
    let (inbox, ctx): (Weak<Mutex<Vec<Event>>>, Context) = (Arc::downgrade(inbox), ctx.clone());
    let spawned = std::thread::Builder::new().name("shell-events".to_owned()).spawn(move || {
        for event in events {
            let Some(inbox) = inbox.upgrade() else { return };
            let Ok(mut held) = inbox.lock() else { return };
            held.push(event);
            drop(held);
            ctx.request_repaint();
        }
    });
    if let Err(error) = spawned {
        tracing::warn!(%error, "panels will see no events");
    }
}

/// The widths the docks have in a window of this plan: the left and right docks by region, nothing for the others.
fn dock_widths<'a>(plan: &Plan, registry: &'a Registry) -> impl Fn(&str) -> Option<f32> + 'a {
    let width = |mode: DockMode, fallback: f32| if mode == DockMode::Rail { fallback } else { mode.width() };
    let (left, right) = (width(plan.left, LEFT_WHEN_RAIL), plan.right.width());
    move |panel| match registry.panel(panel).map(|p| p.region) {
        Some(Region::Left) => Some(left),
        Some(Region::Right) => Some(right),
        _ => None,
    }
}

/// The workspaces to show, in tab order, each with its default layout made safe against the registry. A workspace
/// whose layout names panels nobody registered shows without them (each is reported); one whose layout cannot be read
/// at all is left out and reported, and so is one without a primary panel (it shows, but its saved layouts are not
/// held to one).
fn workspaces_of(registry: &Registry) -> (Vec<&'static WorkspaceDef>, Vec<Layout>) {
    let mut shown = Vec::new();
    let mut defaults = Vec::new();
    for &workspace in registry.workspaces() {
        let tree = (workspace.default_layout)();
        let missing: Vec<&str> = tree.panels().into_iter().filter(|p| registry.panel(p).is_none()).collect();
        if !missing.is_empty() {
            tracing::warn!(workspace = workspace.id, panels = ?missing, "the default layout names panels nobody registered");
        }
        let Some(layout) = Layout::from_registered(&tree, &|p| registry.panel(p).is_some()) else {
            tracing::warn!(
                workspace = workspace.id,
                "the default layout has no panel the shell can show; the workspace is left out"
            );
            continue;
        };
        if !layout.panels().iter().any(|p| registry.panel(p).is_some_and(|d| d.kind == PanelKind::Primary)) {
            tracing::warn!(workspace = workspace.id, "the default layout has no primary panel");
        }
        shown.push(workspace);
        defaults.push(layout);
    }
    (shown, defaults)
}

/// What a panel reads and runs this frame: the runner as the person, the events, the view models and the palette the
/// design system is drawing with.
fn panel_context<'a>(
    runner: &'a dyn Runner,
    actor: &'a Actor,
    events: &'a [Event],
    models: &'a ViewModels,
    ctx: &Context,
) -> PanelCtx<'a> {
    let theme = match ds(ctx).theme.kind {
        ThemeKind::Dark => ApiTheme::Dark,
        ThemeKind::Light => ApiTheme::Light,
        ThemeKind::HighContrast => ApiTheme::HighContrast,
    };
    PanelCtx::new(runner, actor).with_events(events).with_views(models).with_theme(theme)
}

/// The newest `keep` records of the runner's log. The log is read oldest first a page at a time until an empty page
/// (a runner may return fewer than it was asked for while more remain), so a long project's History shows its latest
/// changes and not its first.
pub(super) fn newest_records(runner: &dyn Runner, keep: usize) -> Result<Vec<OpRecord>, OpError> {
    let grants = Grants::person();
    let (mut after, mut newest) = (0, Vec::<OpRecord>::new());
    loop {
        let page = runner.log(after, keep, &grants)?;
        let Some(last) = page.last().map(|record| record.seq) else { return Ok(newest) };
        if last <= after {
            return Err(OpError::internal("the op log did not advance"));
        }
        after = last;
        newest.extend(page);
        newest.drain(..newest.len().saturating_sub(keep));
    }
}
