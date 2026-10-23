//! The Rules panel (`docs/design/automation.md` D11, D12, D17; #1324): the Broadcast workspace's right column, the
//! show's graphs as a table.
//!
//! - **The show:** its name, whether it runs, and Start, Pause, Resume and Stop through the runtime ops. A project with
//!   no show offers to make one (`show.create`).
//! - **The approval card**, while the show needs something this machine hasn't allowed: each need in plain words
//!   ([`needs::line`]), "Allow on this machine" and "Not now". Allow goes to the app's [`ShowApprover`] as the person,
//!   never through an op. A secret the show sends that the keychain lacks gets a masked field that writes it straight
//!   to the keychain under `automation/<name>`.
//! - **The graphs**, in their groups (each group a section with its switch, `automation.set_group`): a switch
//!   (`automation.set_graph {enabled}`), the name, what it does and its state (running, off, suspended with Resume,
//!   needs approval, faulted). A graph that is a straight chain ([`model::chain`]) reads "When <trigger> → <steps>"
//!   and opens inline to edit its steps' constants (`automation.set_node {set}`); any graph opens in the editor.
//!
//! It reads the show when the document changes and the engine's status once a second while it is drawn, and after
//! each of its own changes.

use std::collections::{BTreeMap, BTreeSet};

use egui::{RichText, ScrollArea, Ui};
use omuzaki_live_api::ids::{GraphId, GroupId, NodeId, PortName};
use omuzaki_live_api::need::Need;
use omuzaki_live_api::services::{InertReason, Status};
use omuzaki_ops_api::{OpError, OpInfo};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretValue};
use serde_json::{Value, json};

use super::inspector::constant_field;
use super::model::{self, Described, KindInfo, Show};
use super::needs;
use super::text::{Word, say};
use super::{ShowApprover, ShowFocus};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Banner, Button, EmptyState, Section, StatusChip, TextField, Toggle};
use crate::panels::io::{DocWatch, Io};
use crate::settings::connectors::Keychain;
use crate::shell::requests::{ShellRequest, ShellRequests};

/// Seconds between status reads while the panel is drawn.
const STATUS_EVERY_S: f64 = 1.0;
/// The id of the show a project's first "Make a show" creates.
const FIRST_SHOW: &str = "main";

/// What the person asked for this frame.
enum Action {
    CreateShow,
    Start,
    Pause,
    Resume,
    Stop,
    SetGraph { graph: GraphId, enabled: bool },
    SetGroup { group: GroupId, enabled: bool },
    ResumeGraph(GraphId),
    Open(GraphId),
    SetConstant { graph: GraphId, node: NodeId, port: PortName, value: Value },
    Allow,
    NotNow,
    SaveSecret(String),
}

/// The Rules panel's state.
#[derive(Default)]
pub struct RulesPanel {
    watch: DocWatch,
    show: Option<Show>,
    described: Option<Described>,
    kinds: Vec<KindInfo>,
    ops: Vec<OpInfo>,
    status: Option<Status>,
    status_at: Option<f64>,
    /// Why the show could not be read.
    unreadable: Option<String>,
    /// Why the last change was refused.
    refused: Option<String>,
    /// The person put the approval card away until the needs change.
    not_now: Option<Vec<Need>>,
    /// The chains open for inline editing.
    open: BTreeSet<GraphId>,
    /// Text being typed into a text constant, by node and port.
    typing: BTreeMap<(GraphId, NodeId, PortName), String>,
    /// A secret being typed, by its name; moved into the keychain on Save.
    secrets: BTreeMap<String, String>,
    /// The secrets the keychain holds, as last read (never their values).
    stored: BTreeSet<String>,
}

impl RulesPanel {
    /// A panel that has read nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// The show as last read.
    pub fn show_read(&self) -> Option<&Show> {
        self.show.as_ref()
    }

    /// Draws the panel. `approver` and `keychain` are the app's, when it has them; `requests` asks the shell to show
    /// the graph editor.
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        io: Io<'_>,
        focus: &ShowFocus,
        approver: Option<&ShowApprover>,
        keychain: Option<&Keychain>,
        requests: Option<&ShellRequests>,
    ) {
        let now = ui.ctx().input(|i| i.time);
        if self.watch.stale(io.events) {
            self.read(&io, keychain);
            self.watch.built();
        }
        if self.status_at.is_none_or(|at| now - at >= STATUS_EVERY_S) {
            self.status_at = Some(now);
            self.status = model::status(&io).ok();
        }
        ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(STATUS_EVERY_S));
        let mut actions = Vec::new();
        if let Some(reason) = &self.refused {
            Banner::new(Tone::Danger, &say(ui.ctx(), Word::Refused, &[("reason", reason)])).show(ui);
            ui.add_space(space::S8);
        }
        if let Some(reason) = &self.unreadable {
            let ctx = ui.ctx().clone();
            let line = say(&ctx, Word::ShowUnreadable, &[("reason", reason)]);
            EmptyState::new(icons::WARNING, &say(&ctx, Word::RulesTitle, &[]), &line).show(ui);
            return;
        }
        match self.show.clone() {
            None => Self::empty(ui, &mut actions),
            Some(show) => {
                self.header(ui, &show, &mut actions);
                ui.add_space(space::S8);
                self.approval(ui, approver.is_some(), keychain.is_some(), &mut actions);
                ScrollArea::vertical().id_salt("broadcast.rules.scroll").auto_shrink([false, false]).show(ui, |ui| {
                    self.table(ui, &show, &mut actions);
                });
            }
        }
        let ctx = ui.ctx().clone();
        for action in actions {
            self.act(&ctx, &io, action, focus, approver, keychain, requests);
        }
    }

    fn read(&mut self, io: &Io<'_>, keychain: Option<&Keychain>) {
        match model::read_show(io.runner) {
            Ok(show) => {
                self.unreadable = None;
                self.described = show.as_ref().and_then(|s| model::describe(io, &s.id).ok());
                self.show = show;
            }
            Err(error) => {
                self.unreadable = Some(error.message);
                self.show = None;
            }
        }
        if self.kinds.is_empty() {
            self.kinds = model::node_kinds(io).unwrap_or_default();
        }
        if self.ops.is_empty() {
            self.ops = io.runner.ops();
        }
        self.stored.clear();
        if let (Some(keychain), Some(described)) = (keychain, &self.described) {
            for (name, _) in needs::secrets(described.needs.iter().map(|n| &n.need)) {
                let held = SecretLabel::new(needs::secret_label(&name))
                    .is_ok_and(|label| keychain.0.contains(&label).unwrap_or(false));
                if held {
                    self.stored.insert(name);
                }
            }
        }
    }

    fn empty(ui: &mut Ui, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let out = EmptyState::new(icons::LIGHTNING, &say(&ctx, Word::NoShow, &[]), &say(&ctx, Word::NoShowLine, &[]))
            .primary(&say(&ctx, Word::MakeShow, &[]))
            .show(ui);
        if out.primary {
            actions.push(Action::CreateShow);
        }
    }

    fn header(&self, ui: &mut Ui, show: &Show, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let running = self.status.as_ref().and_then(|s| s.running.as_ref()).is_some_and(|r| r.show == show.id);
        let other = self.status.as_ref().and_then(|s| s.running.as_ref()).filter(|r| r.show != show.id);
        let paused = running && self.status.as_ref().is_some_and(|s| s.paused);
        ui.horizontal(|ui| {
            ui.label(RichText::new(&show.def.name).font(ds(&ctx).text.section.clone()));
            let (tone, word) = match (running, paused) {
                (true, true) => (Tone::Warn, Word::Paused),
                (true, false) => (Tone::Ok, Word::Running),
                _ => (Tone::Info, Word::Stopped),
            };
            ui.add(StatusChip::new(tone, &say(&ctx, word, &[])));
        });
        ui.horizontal_wrapped(|ui| {
            if !running {
                let busy = other.map(|r| say(&ctx, Word::OtherRunning, &[("show", r.show.as_str())]));
                let start_label = say(&ctx, Word::Start, &[]);
                let start = Button::new(&start_label).primary().icon(icons::PLAY).op("automation.start");
                let start = match &busy {
                    Some(reason) => start.disabled(reason),
                    None => start,
                };
                if ui.add(start).clicked() {
                    actions.push(Action::Start);
                }
            } else if paused {
                if ui
                    .add(Button::new(&say(&ctx, Word::Resume, &[])).icon(icons::PLAY).op("automation.resume"))
                    .clicked()
                {
                    actions.push(Action::Resume);
                }
            } else if ui
                .add(Button::new(&say(&ctx, Word::Pause, &[])).icon(icons::PAUSE).op("automation.pause"))
                .clicked()
            {
                actions.push(Action::Pause);
            }
            if running
                && ui.add(Button::new(&say(&ctx, Word::Stop, &[])).icon(icons::STOP).op("automation.stop")).clicked()
            {
                actions.push(Action::Stop);
            }
        });
    }

    fn approval(&mut self, ui: &mut Ui, can_allow: bool, has_keychain: bool, actions: &mut Vec<Action>) {
        let Some(described) = &self.described else { return };
        let missing: Vec<Need> = described.missing().into_iter().cloned().collect();
        if missing.is_empty() || self.not_now.as_ref() == Some(&missing) {
            return;
        }
        let ctx = ui.ctx().clone();
        let theme = ds(&ctx).theme;
        egui::Frame::new()
            .fill(theme.bg2)
            .stroke(egui::Stroke::new(1.0, theme.warn))
            .corner_radius(6.0)
            .inner_margin(space::S12)
            .show(ui, |ui| {
                ui.label(RichText::new(say(&ctx, Word::ApprovalTitle, &[])).font(ds(&ctx).text.strong.clone()));
                ui.label(RichText::new(say(&ctx, Word::ApprovalLine, &[])).color(theme.text2));
                ui.add_space(space::S4);
                for need in &missing {
                    ui.label(format!("• {}", needs::line(&ctx, need, &self.ops)));
                }
                for (name, host) in needs::secrets(&missing) {
                    if self.stored.contains(&name) {
                        continue;
                    }
                    ui.add_space(space::S4);
                    let label = say(&ctx, Word::SecretField, &[("name", &name), ("host", &host)]);
                    let typed = self.secrets.entry(name.clone()).or_default();
                    let (no_keychain, save_label) =
                        (say(&ctx, Word::NoKeychain, &[]), say(&ctx, Word::SaveSecret, &[]));
                    let type_first = say(&ctx, Word::TypeSecretFirst, &[]);
                    ui.horizontal(|ui| {
                        let field = TextField::new(&label, typed).password().width(200.0).id_salt(("secret", &name));
                        let field = if has_keychain { field } else { field.disabled(&no_keychain) };
                        ui.add(field);
                        let save = Button::new(&save_label).disabled_if(typed.is_empty() || !has_keychain, &type_first);
                        if ui.add(save).clicked() {
                            actions.push(Action::SaveSecret(name.clone()));
                        }
                    });
                }
                ui.add_space(space::S8);
                ui.horizontal(|ui| {
                    let (allow_label, no_approver) = (say(&ctx, Word::Allow, &[]), say(&ctx, Word::NoApprover, &[]));
                    let allow = Button::new(&allow_label).primary().disabled_if(!can_allow, &no_approver);
                    if ui.add(allow).clicked() {
                        actions.push(Action::Allow);
                    }
                    if ui.add(Button::new(&say(&ctx, Word::NotNow, &[])).ghost()).clicked() {
                        actions.push(Action::NotNow);
                    }
                });
            });
        ui.add_space(space::S8);
    }

    fn table(&mut self, ui: &mut Ui, show: &Show, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        if show.def.graphs.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::NoGraphs, &[])).color(ds(&ctx).theme.text2));
            return;
        }
        let groups = show.def.groups.keys().cloned().collect::<Vec<_>>();
        for group in &groups {
            let def = &show.def.groups[group];
            let title = def.name.clone().unwrap_or_else(|| group.to_string());
            let on = def.enabled;
            let count = show.def.graphs.values().filter(|g| g.group.as_ref() == Some(group)).count().to_string();
            Section::new(&title)
                .badge(&count)
                .default_open(true)
                .id_salt(("broadcast.rules.group", group.as_str()))
                .show(ui, |ui| {
                    let mut value = on;
                    let switch_label = say(&ctx, Word::GroupOn, &[("group", &title)]);
                    let toggle = Toggle::switch(&switch_label, &mut value).id_salt(("group-on", group.as_str()));
                    if ui.add(toggle).changed() {
                        actions.push(Action::SetGroup { group: group.clone(), enabled: value });
                    }
                    for (id, _) in show.def.graphs.iter().filter(|(_, g)| g.group.as_ref() == Some(group)) {
                        self.row(ui, show, id, actions);
                    }
                });
        }
        let loose: Vec<GraphId> = show
            .def
            .graphs
            .iter()
            .filter(|(_, g)| g.group.as_ref().is_none_or(|group| !show.def.groups.contains_key(group)))
            .map(|(id, _)| id.clone())
            .collect();
        if !loose.is_empty() && !groups.is_empty() {
            ui.add_space(space::S8);
            ui.label(RichText::new(say(&ctx, Word::Ungrouped, &[])).color(ds(&ctx).theme.text2));
        }
        for id in &loose {
            self.row(ui, show, id, actions);
        }
    }

    fn row(&mut self, ui: &mut Ui, show: &Show, id: &GraphId, actions: &mut Vec<Action>) {
        let Some(graph) = show.graph(id) else { return };
        let ctx = ui.ctx().clone();
        let theme = ds(&ctx).theme;
        let name = graph.name.clone().unwrap_or_else(|| id.to_string());
        let nodes = self.described.as_ref().and_then(|d| d.graphs.get(id)).map(|g| g.nodes.clone()).unwrap_or_default();
        let chain = model::chain(graph, &nodes);
        ui.add_space(space::S4);
        egui::Frame::new().fill(theme.bg1).corner_radius(6.0).inner_margin(space::S8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let mut enabled = graph.enabled;
                let toggle = Toggle::switch(&name, &mut enabled).id_salt(("graph-on", id.as_str()));
                if ui.add(toggle).changed() {
                    actions.push(Action::SetGraph { graph: id.clone(), enabled });
                }
                let (tone, state) = self.state(id, graph.enabled, &nodes, &ctx);
                ui.add(StatusChip::new(tone, &state));
            });
            let what = match &chain {
                Some(chain) => {
                    let named = |n: &NodeId| {
                        graph.nodes.get(n).map_or_else(|| n.to_string(), |d| model::node_name(n, d, &self.kinds))
                    };
                    let steps: Vec<String> = chain.steps.iter().map(named).collect();
                    // A trigger's name already reads "When …" ("When someone types a chat command").
                    say(&ctx, Word::When, &[("trigger", &named(&chain.trigger)), ("steps", &steps.join(" → "))])
                }
                None => graph
                    .summary
                    .clone()
                    .unwrap_or_else(|| say(&ctx, Word::NodeCount, &[("count", &graph.nodes.len().to_string())])),
            };
            ui.label(RichText::new(what).color(theme.text2));
            ui.horizontal(|ui| {
                let suspended = self.graph_status(id).and_then(|g| g.suspended.as_ref()).is_some();
                if suspended
                    && ui.add(Button::new(&say(&ctx, Word::ResumeGraph, &[])).op("automation.resume_graph")).clicked()
                {
                    actions.push(Action::ResumeGraph(id.clone()));
                }
                if chain.is_some() {
                    let open = self.open.contains(id);
                    let word = if open { Word::DoneEditing } else { Word::EditInline };
                    if ui.add(Button::new(&say(&ctx, word, &[])).ghost()).clicked() {
                        if open {
                            self.open.remove(id);
                        } else {
                            self.open.insert(id.clone());
                        }
                    }
                }
                if ui.add(Button::new(&say(&ctx, Word::OpenEditor, &[])).ghost()).clicked() {
                    actions.push(Action::Open(id.clone()));
                }
            });
            if let Some(chain) = chain.filter(|_| self.open.contains(id)) {
                for node in std::iter::once(&chain.trigger).chain(&chain.steps) {
                    self.constants(ui, id, node, show, &nodes, actions);
                }
            }
        });
    }

    /// The row's state chip: what the engine says while the show runs, else whether the graph is on.
    fn state(
        &self,
        id: &GraphId,
        enabled: bool,
        nodes: &BTreeMap<NodeId, model::NodeInfo>,
        ctx: &egui::Context,
    ) -> (Tone, String) {
        let not_approved = nodes.values().any(|n| matches!(n.inert(), Some(InertReason::NotApproved { .. })));
        if let Some(status) = self.graph_status(id) {
            if status.suspended.is_some() {
                return (Tone::Warn, say(ctx, Word::Suspended, &[]));
            }
            if !status.faulted.is_empty() {
                return (Tone::Danger, say(ctx, Word::Faulted, &[("count", &status.faulted.len().to_string())]));
            }
            if not_approved {
                return (Tone::Warn, say(ctx, Word::NeedsApproval, &[]));
            }
            if status.active {
                return (Tone::Ok, say(ctx, Word::Running, &[]));
            }
            return (Tone::Info, say(ctx, Word::Off, &[]));
        }
        if not_approved {
            (Tone::Warn, say(ctx, Word::NeedsApproval, &[]))
        } else if enabled {
            (Tone::Info, say(ctx, Word::Ready, &[]))
        } else {
            (Tone::Info, say(ctx, Word::Off, &[]))
        }
    }

    fn graph_status(&self, id: &GraphId) -> Option<&omuzaki_live_api::services::GraphStatus> {
        let status = self.status.as_ref()?;
        status.running.as_ref()?;
        status.graphs.iter().find(|g| &g.graph == id)
    }

    /// One step's unwired value inputs, each a field for its constant.
    fn constants(
        &mut self,
        ui: &mut Ui,
        graph: &GraphId,
        node: &NodeId,
        show: &Show,
        nodes: &BTreeMap<NodeId, model::NodeInfo>,
        actions: &mut Vec<Action>,
    ) {
        let (Some(def), Some(info)) = (show.node(graph, node), nodes.get(node)) else { return };
        let ctx = ui.ctx().clone();
        let inputs: Vec<_> =
            info.inputs.iter().filter(|p| !p.ty.is_event() && !def.inputs.contains_key(&p.name)).collect();
        if inputs.is_empty() {
            return;
        }
        ui.add_space(space::S4);
        ui.label(RichText::new(model::node_name(node, def, &self.kinds)).color(ds(&ctx).theme.text2));
        for port in inputs {
            let current = def.set.get(&port.name).cloned().or_else(|| port.default.clone()).unwrap_or(Value::Null);
            let typing = self.typing.entry((graph.clone(), node.clone(), port.name.clone())).or_default();
            let salt = ("rules-const", graph.as_str());
            if let Some(value) = constant_field(ui, port, &current, typing, salt) {
                actions.push(Action::SetConstant {
                    graph: graph.clone(),
                    node: node.clone(),
                    port: port.name.clone(),
                    value,
                });
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn act(
        &mut self,
        ctx: &egui::Context,
        io: &Io<'_>,
        action: Action,
        focus: &ShowFocus,
        approver: Option<&ShowApprover>,
        keychain: Option<&Keychain>,
        requests: Option<&ShellRequests>,
    ) {
        let show = self.show.as_ref().map(|s| s.id.clone());
        let ran = match action {
            Action::CreateShow => {
                let name = say(ctx, Word::FirstShowName, &[]);
                io.run("show.create", json!({ "id": FIRST_SHOW, "name": name }), None).map(drop)
            }
            Action::Start => io.run("automation.start", json!({ "show": show }), None).map(drop),
            Action::Pause => io.run("automation.pause", json!({}), None).map(drop),
            Action::Resume => io.run("automation.resume", json!({}), None).map(drop),
            Action::Stop => io.run("automation.stop", json!({}), None).map(drop),
            Action::SetGraph { graph, enabled } => io
                .run("automation.set_graph", json!({ "show": show, "graph": graph, "enabled": enabled }), None)
                .map(drop),
            Action::SetGroup { group, enabled } => io
                .run("automation.set_group", json!({ "show": show, "group": group, "enabled": enabled }), None)
                .map(drop),
            Action::ResumeGraph(graph) => io.run("automation.resume_graph", json!({ "graph": graph }), None).map(drop),
            Action::SetConstant { graph, node, port, value } => io
                .run(
                    "automation.set_node",
                    json!({ "show": show, "graph": graph, "node": node, "set": { port.as_str(): value } }),
                    None,
                )
                .map(drop),
            Action::Open(graph) => {
                focus.open(Some(graph));
                if let Some(requests) = requests {
                    requests.push(ShellRequest::ShowPanel(super::registration::GRAPH_PANEL));
                }
                Ok(())
            }
            Action::NotNow => {
                self.not_now = self.described.as_ref().map(|d| d.missing().into_iter().cloned().collect());
                Ok(())
            }
            Action::Allow => self.allow(io, approver),
            Action::SaveSecret(name) => self.save_secret(&name, keychain),
        };
        self.refused = ran.err().map(|e| e.message);
        self.status_at = None;
        self.watch.force();
    }

    fn allow(&mut self, io: &Io<'_>, approver: Option<&ShowApprover>) -> Result<(), OpError> {
        let approver = approver.ok_or_else(|| OpError::unsupported("no approvals store in this build"))?;
        let (Some(show), Some(described)) = (&self.show, &self.described) else { return Ok(()) };
        let missing: Vec<Need> = described.missing().into_iter().cloned().collect();
        approver.0.allow(&show.id, &missing, io.actor).map_err(OpError::internal)
    }

    fn save_secret(&mut self, name: &str, keychain: Option<&Keychain>) -> Result<(), OpError> {
        let keychain = keychain.ok_or_else(|| OpError::unsupported("no keychain in this build"))?;
        let label =
            SecretLabel::new(needs::secret_label(name)).map_err(|e| OpError::invalid("secret", e.to_string()))?;
        // Moved, not copied: the keychain's value zeroes the one buffer the field held.
        let typed = self.secrets.remove(name).unwrap_or_default();
        keychain.0.set(&label, SecretValue::new(typed)).map_err(|e| OpError::internal(e.to_string()))?;
        self.stored.insert(name.to_owned());
        Ok(())
    }
}
