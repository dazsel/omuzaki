//! The automation graph editor (`docs/design/automation.md` D3, D17; #1279): a centre tab of the Broadcast workspace,
//! the show's graphs as node graphs on `egui-snarl`, with the selected node's [`inspector`](super::inspector) beside
//! it.
//!
//! **The document is the truth.** The tab rebuilds its [`Snarl`] from the graph whenever the document changes, and
//! the canvas never keeps an edit of its own: a wire drawn is `automation.connect` (and the next frame draws it from
//! the document), a wire cut is `automation.disconnect`, a node from the palette is `automation.add_node`, a node
//! deleted is `automation.remove_node`, and a node dragged is `automation.set_node {pos}` every frame it moves, inside
//! one transaction that the release commits (one undo step) and Esc rolls back.
//!
//! Pins are coloured by port type from the theme's tokens, events as triangles and values as circles; a wire between
//! types that don't convert is refused with the reason. While the show runs (`automation.values`, at most four reads
//! a second while the tab is drawn), a node that just fired glows, an inert or faulted node carries a badge, a
//! suspended graph says so, and a wire shows its last value on hover.
//!
//! **Keyboard:** every control around the canvas is a widget Tab reaches: the graph picker, **Add node** (a searchable
//! palette from `automation.node_kinds`), the node list (selecting a node opens its inspector), and the inspector's
//! **Connect to…** and **Delete**. Node titles and port names are labels, so assistive technology reads them.

use std::collections::BTreeMap;

use egui::{Color32, Pos2, RichText, ScrollArea, Ui, pos2};
use egui_snarl::ui::{PinInfo, SnarlStyle, SnarlViewer, SnarlWidget};
use egui_snarl::{InPin, InPinId, NodeId as SnarlNode, OutPin, OutPinId, Snarl};
use omuzaki_live_api::ids::{GraphId, NodeId, PortName, PortRef};
use omuzaki_live_api::services::ValuesSnapshot;
use omuzaki_live_api::value::PortType;
use omuzaki_ops_api::TxId;
use serde_json::json;

use super::ShowFocus;
use super::inspector::{InspectorAction, NodeInspector, compatible, inert_text};
use super::model::{self, Described, KindInfo, PortInfo, Show};
use super::text::{Word, say, text};
use super::values::READ_EVERY;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status as Tone, Theme, space};
use crate::ds::widgets::{Banner, Button, Choice, Dropdown, EmptyState, StatusChip, TextField};
use crate::panels::io::{DocWatch, Io};

/// How long a node glows after it fired, in seconds of the runner clock.
const GLOW_S: f64 = 0.8;
/// How many matches the palette lists.
const PALETTE_ROWS: usize = 12;
/// The width of the inspector column, in points.
const INSPECTOR_WIDTH: f32 = 300.0;
/// How far right of the rightmost node a node added without a place goes, in points.
const ADD_GAP: f32 = 220.0;

/// One node as the canvas draws it.
#[derive(Clone, Debug)]
pub struct GraphNode {
    /// Its id in the document.
    pub id: NodeId,
    /// Its title: its label, or its id.
    pub title: String,
    /// What its kind does, in a few words.
    pub kind: String,
    /// Its inputs and outputs, as the engine resolved them.
    pub inputs: Vec<PortInfo>,
    /// Its outputs.
    pub outputs: Vec<PortInfo>,
    /// Why it won't run, in words.
    pub inert: Option<String>,
    /// It panicked this run.
    pub faulted: bool,
    /// It fired a moment ago.
    pub glowing: bool,
}

/// An edit asked of the canvas this frame.
#[derive(Clone, Debug, PartialEq)]
enum Edit {
    Connect { from: PortRef, to: PortRef },
    Disconnect { from: PortRef, to: PortRef },
    DropInputs(PortRef),
    DropOutputs(PortRef),
    Remove(NodeId),
    Select(NodeId),
    Palette(Pos2),
    Refused(String),
}

/// The editor's state.
#[derive(Default)]
pub struct GraphPanel {
    watch: DocWatch,
    show: Option<Show>,
    described: Option<Described>,
    kinds: Vec<KindInfo>,
    /// The graph the canvas was built from, and the focus generation it was built at.
    built: Option<(GraphId, u64)>,
    snarl: Snarl<GraphNode>,
    /// Each document node's canvas node.
    ids: BTreeMap<NodeId, SnarlNode>,
    /// Each node's position as the document holds it (or as last sent during a drag).
    positions: BTreeMap<NodeId, [f32; 2]>,
    /// The open drag's transaction.
    drag: Option<TxId>,
    values: Option<ValuesSnapshot>,
    values_at: Option<f64>,
    /// Each output's last value, from the firings.
    last: BTreeMap<(NodeId, PortName), String>,
    /// The palette: open, its query, and where an added node goes.
    palette: Option<(String, Option<Pos2>)>,
    inspector: NodeInspector,
    refused: Option<String>,
    unreadable: Option<String>,
}

impl GraphPanel {
    /// An editor that has read nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many nodes the canvas holds.
    pub fn nodes(&self) -> usize {
        self.snarl.nodes().count()
    }

    /// How many wires the canvas holds.
    pub fn wires(&self) -> usize {
        self.snarl.wires().count()
    }

    /// Moves canvas node `id` by `delta`, as the canvas does while the person drags it: the next frames send its
    /// position as they would for a drag on the canvas. For tests, which can't aim a pointer through the canvas's own
    /// pan and zoom.
    pub fn drag_node(&mut self, id: &str, delta: egui::Vec2) {
        for node in self.snarl.nodes_info_mut() {
            if node.value.id.as_str() == id {
                node.pos += delta;
            }
        }
    }

    /// Whether the node with `id` glows on the canvas.
    pub fn glowing(&self, id: &str) -> bool {
        self.snarl.nodes().any(|n| n.id.as_str() == id && n.glowing)
    }

    /// Draws the editor.
    pub fn ui(&mut self, ui: &mut Ui, io: Io<'_>, focus: &ShowFocus) {
        let now = ui.ctx().input(|i| i.time);
        let busy = self.drag.is_some() || self.inspector.in_gesture();
        if !busy && (self.watch.stale(io.events) || self.show.is_none() && self.unreadable.is_none()) {
            self.read(&io);
            self.watch.built();
            self.built = None;
            self.inspector.reset();
        }
        if self.values_at.is_none_or(|at| now - at >= READ_EVERY.as_secs_f64()) {
            self.values_at = Some(now);
            self.values = model::values(&io).ok().flatten();
            self.collect_last();
            self.overlay(focus.graph().as_ref());
        }
        ui.ctx().request_repaint_after(READ_EVERY);
        let ctx = ui.ctx().clone();
        if let Some(reason) = &self.unreadable {
            let line = say(&ctx, Word::ShowUnreadable, &[("reason", reason)]);
            EmptyState::new(icons::WARNING, &say(&ctx, Word::GraphTitle, &[]), &line).show(ui);
            return;
        }
        let Some(show) = self.show.clone() else {
            let line = say(&ctx, Word::NoShowGraphLine, &[]);
            EmptyState::new(icons::TREE_STRUCTURE, &say(&ctx, Word::NoShow, &[]), &line).show(ui);
            return;
        };
        if focus.graph().is_none_or(|g| !show.def.graphs.contains_key(&g)) {
            focus.open(show.def.graphs.keys().next().cloned());
        }
        let mut edits = Vec::new();
        self.toolbar(ui, &io, &show, focus);
        let Some(graph) = focus.graph() else {
            let line = say(&ctx, Word::NoGraphsLine, &[]);
            EmptyState::new(icons::TREE_STRUCTURE, &say(&ctx, Word::NoGraphs, &[]), &line).show(ui);
            return;
        };
        if !busy && self.built.as_ref() != Some(&(graph.clone(), focus.generation())) {
            self.build(&ctx, &show, &graph);
            self.built = Some((graph.clone(), focus.generation()));
        }
        if let Some(reason) = &self.refused {
            Banner::new(Tone::Danger, &say(&ctx, Word::Refused, &[("reason", reason)])).show(ui);
        }
        let suspended = self
            .values
            .as_ref()
            .and_then(|v| v.graphs.iter().find(|g| g.graph == graph))
            .is_some_and(|g| g.suspended.is_some());
        if suspended {
            Banner::new(Tone::Warn, &say(&ctx, Word::GraphSuspended, &[])).show(ui);
        }
        let mut inspector_action = None;
        ui.horizontal_top(|ui| {
            let canvas_width = (ui.available_width() - INSPECTOR_WIDTH - space::S12).max(200.0);
            ui.vertical(|ui| {
                ui.set_width(canvas_width);
                self.canvas(ui, &mut edits);
            });
            ui.separator();
            ui.vertical(|ui| {
                ui.set_width(INSPECTOR_WIDTH);
                ScrollArea::vertical().id_salt("broadcast.graph.side").auto_shrink([false, false]).show(ui, |ui| {
                    self.palette(ui, &io, &show, &graph);
                    self.node_list(ui, focus);
                    ui.separator();
                    if let Some(node) = focus.node() {
                        let described = self.described.as_ref();
                        inspector_action = self.inspector.ui(ui, &io, &show, described, &self.kinds, &graph, &node);
                    } else {
                        ui.label(RichText::new(say(&ctx, Word::NothingSelected, &[])).color(ds(&ctx).theme.text2));
                    }
                });
            });
        });
        match inspector_action {
            Some(InspectorAction::Connect { from, to }) => edits.push(Edit::Connect { from, to }),
            Some(InspectorAction::Delete(node)) => edits.push(Edit::Remove(node)),
            None => {}
        }
        self.follow_drag(ui, &io, &show, &graph);
        for edit in edits {
            self.carry_out(&io, &show, &graph, focus, edit);
        }
    }

    fn read(&mut self, io: &Io<'_>) {
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
    }

    /// Each output's last value, from the newest firing that carried it.
    fn collect_last(&mut self) {
        self.last.clear();
        let Some(values) = &self.values else { return };
        for firing in &values.firings {
            for (port, value) in &firing.values {
                self.last.insert((firing.node.node.clone(), port.clone()), value.clone());
            }
        }
    }

    /// The canvas, rebuilt from graph `graph` of `show`.
    fn build(&mut self, ctx: &egui::Context, show: &Show, graph: &GraphId) {
        self.snarl = Snarl::new();
        self.ids.clear();
        self.positions.clear();
        let Some(def) = show.graph(graph) else { return };
        for (id, node) in &def.nodes {
            let info = self.described.as_ref().and_then(|d| d.node(graph, id));
            let gnode = GraphNode {
                id: id.clone(),
                title: node.label.clone().unwrap_or_else(|| id.to_string()),
                kind: model::node_name(id, &omuzaki_live_api::graph::NodeDef::new(node.kind.clone()), &self.kinds),
                inputs: info.map(|i| i.inputs.clone()).unwrap_or_default(),
                outputs: info.map(|i| i.outputs.clone()).unwrap_or_default(),
                inert: info.and_then(|i| inert_reason(ctx, i)),
                faulted: false,
                glowing: false,
            };
            let at = self.snarl.insert_node(pos2(node.pos[0], node.pos[1]), gnode);
            self.ids.insert(id.clone(), at);
            self.positions.insert(id.clone(), node.pos);
        }
        for (id, node) in &def.nodes {
            let Some(&to_node) = self.ids.get(id) else { continue };
            let Some(info) = self.described.as_ref().and_then(|d| d.node(graph, id)) else { continue };
            for (port, wire) in &node.inputs {
                let Some(input) = info.inputs.iter().position(|p| &p.name == port) else { continue };
                for source in wire.sources() {
                    let Some(&from_node) = self.ids.get(&source.node) else { continue };
                    let Some(from_info) = self.described.as_ref().and_then(|d| d.node(graph, &source.node)) else {
                        continue;
                    };
                    let Some(output) = from_info.outputs.iter().position(|p| p.name == source.port) else { continue };
                    self.snarl.connect(OutPinId { node: from_node, output }, InPinId { node: to_node, input });
                }
            }
        }
        self.overlay(Some(graph));
    }

    /// Marks the nodes that fired a moment ago and the faulted ones, from the last values read.
    fn overlay(&mut self, graph: Option<&GraphId>) {
        let values = self.values.as_ref();
        let fired_after = values.map(|v| v.at.0 - GLOW_S);
        let status = values.and_then(|v| v.graphs.iter().find(|g| Some(&g.graph) == graph));
        for node in self.snarl.nodes_mut() {
            node.glowing = fired_after.is_some_and(|after| {
                values
                    .iter()
                    .flat_map(|v| &v.firings)
                    .any(|f| f.node.node == node.id && Some(&f.node.graph) == graph && f.at.0 >= after)
            });
            node.faulted = status.is_some_and(|s| s.faulted.contains(&node.id));
        }
    }

    fn toolbar(&mut self, ui: &mut Ui, io: &Io<'_>, show: &Show, focus: &ShowFocus) {
        let ctx = ui.ctx().clone();
        ui.horizontal_wrapped(|ui| {
            let ids: Vec<GraphId> = show.def.graphs.keys().cloned().collect();
            if !ids.is_empty() {
                let names: Vec<String> =
                    ids.iter().map(|id| show.def.graphs[id].name.clone().unwrap_or_else(|| id.to_string())).collect();
                let choices: Vec<Choice<'_, usize>> =
                    names.iter().enumerate().map(|(i, name)| Choice::new(i, name.as_str())).collect();
                let mut at = focus.graph().and_then(|g| ids.iter().position(|i| *i == g)).unwrap_or(0);
                let label = say(&ctx, Word::GraphPicker, &[]);
                if ui.add(Dropdown::new(&label, &mut at, &choices).id_salt("broadcast.graph.picker")).changed() {
                    focus.open(ids.get(at).cloned());
                }
            }
            if ui
                .add(Button::new(&say(&ctx, Word::NewGraph, &[])).icon(icons::PLUS).op("automation.add_graph"))
                .clicked()
            {
                let id = (1..=show.def.graphs.len() + 1)
                    .map(|n| format!("rule-{n}"))
                    .find(|id| GraphId::new(id.as_str()).is_ok_and(|g| !show.def.graphs.contains_key(&g)));
                if let Some(id) = id {
                    let name = say(&ctx, Word::NewGraphName, &[]);
                    let args = json!({ "show": show.id, "id": id, "graph": { "graph": { "name": name } } });
                    self.refused = io.run("automation.add_graph", args, None).err().map(|e| e.message);
                    focus.open(GraphId::new(id.as_str()).ok());
                    self.watch.force();
                }
            }
            let add_label = say(&ctx, Word::AddNode, &[]);
            let add = Button::new(&add_label).icon(icons::PLUS).op("automation.add_node");
            if ui.add(add).clicked() {
                self.palette = Some((String::new(), None));
            }
        });
    }

    fn canvas(&mut self, ui: &mut Ui, edits: &mut Vec<Edit>) {
        let ctx = ui.ctx().clone();
        let theme = ds(&ctx).theme;
        let mut viewer = Viewer { edits: Vec::new(), theme: &theme, ctx: &ctx, last: &self.last };
        let mut style = SnarlStyle::new();
        // The first view frames every node at no more than their own size; zooming in further adds nothing to read.
        style.max_scale = Some(1.0);
        style.pin_placement = Some(egui_snarl::ui::PinPlacement::Edge);
        let height = ui.available_height().max(240.0);
        let widget = SnarlWidget::new()
            .id_salt("broadcast.graph.canvas")
            .style(style)
            .min_size(egui::vec2(ui.available_width(), height));
        widget.show(&mut self.snarl, &mut viewer, ui);
        edits.append(&mut viewer.edits);
    }

    fn palette(&mut self, ui: &mut Ui, io: &Io<'_>, show: &Show, graph: &GraphId) {
        let Some((query, at)) = self.palette.as_mut() else { return };
        let ctx = ui.ctx().clone();
        let label = say(&ctx, Word::FindNode, &[]);
        let field = ui.add(TextField::new(&label, query).search().id_salt("broadcast.graph.palette"));
        if field.gained_focus() || !field.has_focus() && query.is_empty() {
            field.request_focus();
        }
        let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
        let matches: Vec<&KindInfo> = self
            .kinds
            .iter()
            .filter(|k| {
                let hay = format!("{} {}", k.id, k.summary).to_lowercase();
                words.iter().all(|w| hay.contains(w.as_str()))
            })
            .take(PALETTE_ROWS)
            .collect();
        let mut chosen = None;
        if field.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
            chosen = matches.first().map(|k| k.id.clone());
        }
        for kind in &matches {
            let button = Button::new(&kind.summary).ghost().accessible_label(&kind.summary);
            if ui.add(button).on_hover_text(&kind.id).clicked() {
                chosen = Some(kind.id.clone());
            }
        }
        if ui.add(Button::new(&say(&ctx, Word::Close, &[])).ghost()).clicked()
            || ui.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.palette = None;
            return;
        }
        let Some(kind) = chosen else { return };
        let pos = at.map_or_else(
            || {
                let right = self.positions.values().map(|p| p[0]).fold(f32::NEG_INFINITY, f32::max);
                [if right.is_finite() { right + ADD_GAP } else { 0.0 }, 0.0]
            },
            |p| [p.x, p.y],
        );
        let args = json!({ "show": show.id, "graph": graph, "kind": kind, "pos": pos });
        self.refused = io.run("automation.add_node", args, None).err().map(|e| e.message);
        self.palette = None;
        self.watch.force();
    }

    fn node_list(&mut self, ui: &mut Ui, focus: &ShowFocus) {
        let ctx = ui.ctx().clone();
        ui.label(RichText::new(say(&ctx, Word::Nodes, &[])).font(ds(&ctx).text.strong.clone()));
        let selected = focus.node();
        let rows: Vec<(NodeId, String, Option<String>, bool)> = self
            .snarl
            .nodes()
            .map(|n| (n.id.clone(), format!("{} · {}", n.title, n.kind), n.inert.clone(), n.faulted))
            .collect();
        for (id, title, inert, faulted) in rows {
            ui.horizontal(|ui| {
                let mut button = Button::new(&title).ghost();
                if selected.as_ref() == Some(&id) {
                    button = button.primary();
                }
                if ui.add(button).clicked() {
                    focus.select(Some(id.clone()));
                }
                if inert.is_some() {
                    ui.add(StatusChip::new(Tone::Warn, &say(&ctx, Word::Inert, &[])));
                }
                if faulted {
                    ui.add(StatusChip::new(Tone::Danger, &say(&ctx, Word::Faulted, &[("count", "1")])));
                }
            });
        }
    }

    /// Sends the positions of nodes the person is dragging, in one transaction per drag.
    fn follow_drag(&mut self, ui: &Ui, io: &Io<'_>, show: &Show, graph: &GraphId) {
        let down = ui.input(|i| i.pointer.any_down());
        if self.drag.is_some() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            if let Some(tx) = self.drag.take() {
                self.refused = io.runner.rollback(tx, io.actor).err().map(|e| e.message);
            }
            self.built = None;
            self.watch.force();
            return;
        }
        let moved: Vec<(NodeId, [f32; 2])> = self
            .snarl
            .nodes_pos()
            .filter_map(|(pos, node)| {
                let was = self.positions.get(&node.id)?;
                let now = [pos.x.round(), pos.y.round()];
                ((now[0] - was[0]).abs() >= 0.5 || (now[1] - was[1]).abs() >= 0.5).then(|| (node.id.clone(), now))
            })
            .collect();
        if !moved.is_empty() {
            let tx = match self.drag {
                Some(tx) => Some(tx),
                None if down => {
                    let tx = io.runner.begin(&text(ui.ctx(), Word::MoveNodes), io.actor);
                    self.drag = Some(tx);
                    Some(tx)
                }
                None => None,
            };
            for (node, pos) in moved {
                let args = json!({ "show": show.id, "graph": graph, "node": node, "pos": pos });
                match io.run("automation.set_node", args, tx) {
                    Ok(_) => {
                        self.positions.insert(node, pos);
                    }
                    Err(error) => self.refused = Some(error.message),
                }
            }
        }
        if !down && let Some(tx) = self.drag.take() {
            self.refused = io.runner.commit(tx, io.actor).err().map(|e| e.message);
            self.watch.force();
        }
    }

    fn carry_out(&mut self, io: &Io<'_>, show: &Show, graph: &GraphId, focus: &ShowFocus, edit: Edit) {
        let base = |extra: serde_json::Value| {
            let mut args = json!({ "show": show.id, "graph": graph });
            if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
                args.extend(extra.clone());
            }
            args
        };
        let ran = match edit {
            Edit::Connect { from, to } => {
                io.run("automation.connect", base(json!({ "from": from.to_string(), "to": to.to_string() })), None)
            }
            Edit::Disconnect { from, to } => {
                io.run("automation.disconnect", base(json!({ "from": from.to_string(), "to": to.to_string() })), None)
            }
            Edit::DropInputs(to) => io.run("automation.disconnect", base(json!({ "to": to.to_string() })), None),
            Edit::DropOutputs(from) => {
                let targets: Vec<PortRef> = show
                    .graph(graph)
                    .map(|g| {
                        g.nodes
                            .iter()
                            .flat_map(|(id, n)| {
                                n.inputs
                                    .iter()
                                    .filter(|(_, w)| w.sources().contains(&from))
                                    .map(|(port, _)| PortRef::new(id.clone(), port.clone()))
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                let mut last = Ok(None);
                for to in targets {
                    let args = base(json!({ "from": from.to_string(), "to": to.to_string() }));
                    last = io.run("automation.disconnect", args, None).map(Some);
                    if last.is_err() {
                        break;
                    }
                }
                match last {
                    Ok(Some(outcome)) => Ok(outcome),
                    Ok(None) => return,
                    Err(error) => Err(error),
                }
            }
            Edit::Remove(node) => {
                if focus.node().as_ref() == Some(&node) {
                    focus.select(None);
                }
                io.run("automation.remove_node", base(json!({ "node": node })), None)
            }
            Edit::Select(node) => {
                focus.select(Some(node));
                return;
            }
            Edit::Palette(at) => {
                self.palette = Some((String::new(), Some(at)));
                return;
            }
            Edit::Refused(reason) => {
                self.refused = Some(reason);
                return;
            }
        };
        self.refused = ran.err().map(|e| e.message);
        self.watch.force();
    }
}

/// The canvas's view of the nodes: it draws them and turns every gesture into an [`Edit`], changing nothing itself.
struct Viewer<'a> {
    edits: Vec<Edit>,
    theme: &'a Theme,
    ctx: &'a egui::Context,
    last: &'a BTreeMap<(NodeId, PortName), String>,
}

impl Viewer<'_> {
    fn colour(&self, ty: PortType) -> Color32 {
        match ty {
            PortType::Event => self.theme.text,
            PortType::Bool => self.theme.ok,
            PortType::Number => self.theme.accent,
            PortType::Text => self.theme.warn,
            PortType::List => self.theme.agent,
            PortType::Json => self.theme.text3,
        }
    }

    fn pin(&self, ty: PortType) -> PinInfo {
        let pin = if ty.is_event() { PinInfo::triangle() } else { PinInfo::circle() };
        pin.with_fill(self.colour(ty))
    }
}

/// The document's name for port `index` of `node`, an input or an output.
fn port_ref(node: &GraphNode, input: bool, index: usize) -> Option<PortRef> {
    let ports = if input { &node.inputs } else { &node.outputs };
    ports.get(index).map(|p| PortRef::new(node.id.clone(), p.name.clone()))
}

impl SnarlViewer<GraphNode> for Viewer<'_> {
    fn title(&mut self, node: &GraphNode) -> String {
        node.title.clone()
    }

    fn show_header(
        &mut self,
        node: SnarlNode,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GraphNode>,
    ) {
        let Some(n) = snarl.get_node(node) else { return };
        ui.label(RichText::new(&n.title).strong()).on_hover_text(&n.kind);
        let inert = n.inert.as_ref().map(|why| (why, say(self.ctx, Word::Inert, &[])));
        let faulted = n.faulted.then(|| say(self.ctx, Word::Faulted, &[("count", "1")]));
        // A node is as wide as its pins; the header grows it to hold its chips whole, or "Won't run" reads "…".
        let gap = ui.spacing().item_spacing.x;
        let chips: f32 =
            inert.iter().map(|(_, word)| StatusChip::new(Tone::Warn, word).width(self.ctx) + gap).sum::<f32>()
                + faulted.iter().map(|word| StatusChip::new(Tone::Danger, word).width(self.ctx) + gap).sum::<f32>();
        if chips > 0.0 {
            ui.set_min_width(ui.min_rect().width() + chips);
        }
        if let Some((why, word)) = &inert {
            ui.add(StatusChip::new(Tone::Warn, word)).on_hover_text(why.as_str());
        }
        if let Some(word) = &faulted {
            ui.add(StatusChip::new(Tone::Danger, word));
        }
    }

    fn node_frame(
        &mut self,
        default: egui::Frame,
        node: SnarlNode,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        snarl: &Snarl<GraphNode>,
    ) -> egui::Frame {
        match snarl.get_node(node) {
            Some(n) if n.glowing => default.stroke(egui::Stroke::new(2.0, self.theme.accent)),
            Some(n) if n.faulted => default.stroke(egui::Stroke::new(1.5, self.theme.danger)),
            Some(n) if n.inert.is_some() => default.stroke(egui::Stroke::new(1.5, self.theme.warn)),
            _ => default,
        }
    }

    fn final_node_rect(&mut self, node: SnarlNode, rect: egui::Rect, ui: &mut Ui, snarl: &mut Snarl<GraphNode>) {
        let clicked =
            ui.input(|i| i.pointer.primary_clicked() && i.pointer.interact_pos().is_some_and(|p| rect.contains(p)));
        if clicked && let Some(n) = snarl.get_node(node) {
            self.edits.push(Edit::Select(n.id.clone()));
        }
    }

    fn inputs(&mut self, node: &GraphNode) -> usize {
        node.inputs.len()
    }

    fn outputs(&mut self, node: &GraphNode) -> usize {
        node.outputs.len()
    }

    #[allow(refining_impl_trait)]
    fn show_input(&mut self, pin: &InPin, ui: &mut Ui, snarl: &mut Snarl<GraphNode>) -> PinInfo {
        let Some(port) = snarl.get_node(pin.id.node).and_then(|n| n.inputs.get(pin.id.input)) else {
            return PinInfo::circle();
        };
        ui.add(egui::Label::new(port.name.as_str()).wrap_mode(egui::TextWrapMode::Extend))
            .on_hover_text(port.ty.as_str());
        self.pin(port.ty)
    }

    #[allow(refining_impl_trait)]
    fn show_output(&mut self, pin: &OutPin, ui: &mut Ui, snarl: &mut Snarl<GraphNode>) -> PinInfo {
        let Some(port) = snarl.get_node(pin.id.node).and_then(|n| n.outputs.get(pin.id.output)) else {
            return PinInfo::circle();
        };
        ui.add(egui::Label::new(port.name.as_str()).wrap_mode(egui::TextWrapMode::Extend))
            .on_hover_text(port.ty.as_str());
        self.pin(port.ty)
    }

    fn has_wire_widget(&mut self, from: &OutPinId, _to: &InPinId, snarl: &Snarl<GraphNode>) -> bool {
        snarl
            .get_node(from.node)
            .and_then(|n| port_ref(n, false, from.output))
            .is_some_and(|p| self.last.contains_key(&(p.node, p.port)))
    }

    fn show_wire_widget(&mut self, from: &OutPin, _to: &InPin, ui: &mut Ui, snarl: &mut Snarl<GraphNode>) {
        let Some(port) = snarl.get_node(from.id.node).and_then(|n| port_ref(n, false, from.id.output)) else {
            return;
        };
        if let Some(value) = self.last.get(&(port.node.clone(), port.port.clone())) {
            let dot = ui.label(RichText::new("●").color(self.theme.text3).small());
            dot.on_hover_text(say(self.ctx, Word::LastValue, &[("value", value)]));
        }
    }

    fn connect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<GraphNode>) {
        let (Some(a), Some(b)) = (snarl.get_node(from.id.node), snarl.get_node(to.id.node)) else { return };
        let (Some(out), Some(inp)) = (a.outputs.get(from.id.output), b.inputs.get(to.id.input)) else { return };
        match compatible(out.ty, inp.ty) {
            Ok(()) => {
                let (Some(from), Some(to)) = (port_ref(a, false, from.id.output), port_ref(b, true, to.id.input))
                else {
                    return;
                };
                self.edits.push(Edit::Connect { from, to });
            }
            Err(word) => {
                let reason = say(self.ctx, word, &[("from", out.ty.as_str()), ("to", inp.ty.as_str())]);
                self.edits.push(Edit::Refused(reason));
            }
        }
    }

    fn disconnect(&mut self, from: &OutPin, to: &InPin, snarl: &mut Snarl<GraphNode>) {
        let (Some(a), Some(b)) = (snarl.get_node(from.id.node), snarl.get_node(to.id.node)) else { return };
        if let (Some(from), Some(to)) = (port_ref(a, false, from.id.output), port_ref(b, true, to.id.input)) {
            self.edits.push(Edit::Disconnect { from, to });
        }
    }

    fn drop_inputs(&mut self, pin: &InPin, snarl: &mut Snarl<GraphNode>) {
        if let Some(port) = snarl.get_node(pin.id.node).and_then(|n| port_ref(n, true, pin.id.input)) {
            self.edits.push(Edit::DropInputs(port));
        }
    }

    fn drop_outputs(&mut self, pin: &OutPin, snarl: &mut Snarl<GraphNode>) {
        if let Some(port) = snarl.get_node(pin.id.node).and_then(|n| port_ref(n, false, pin.id.output)) {
            self.edits.push(Edit::DropOutputs(port));
        }
    }

    fn has_graph_menu(&mut self, _pos: Pos2, _snarl: &mut Snarl<GraphNode>) -> bool {
        true
    }

    fn show_graph_menu(&mut self, pos: Pos2, ui: &mut Ui, _snarl: &mut Snarl<GraphNode>) {
        if ui.add(Button::new(&say(self.ctx, Word::AddNode, &[])).ghost()).clicked() {
            self.edits.push(Edit::Palette(pos));
            ui.close();
        }
    }

    fn has_node_menu(&mut self, _node: &GraphNode) -> bool {
        true
    }

    fn show_node_menu(
        &mut self,
        node: SnarlNode,
        _inputs: &[InPin],
        _outputs: &[OutPin],
        ui: &mut Ui,
        snarl: &mut Snarl<GraphNode>,
    ) {
        let Some(n) = snarl.get_node(node) else { return };
        if ui.add(Button::new(&say(self.ctx, Word::DeleteNode, &[])).danger()).clicked() {
            self.edits.push(Edit::Remove(n.id.clone()));
            ui.close();
        }
    }
}

/// Why a node won't run, in words, for its badge's tooltip.
pub fn inert_reason(ctx: &egui::Context, info: &model::NodeInfo) -> Option<String> {
    info.inert().map(|why| inert_text(ctx, &why))
}
