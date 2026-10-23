//! The automation node inspector (`docs/design/automation.md` D2, D17, D18; #782): the graph editor's side column for
//! the selected node.
//!
//! - **Its parameters** as a form generated from its kind's JSON Schema, with the inspector's usual widgets
//!   (`ds::schema_form`); a shape the form can't map is a read-only row, never lost.
//! - **Its unwired value inputs** as `set` constants, one field per port type ([`constant_field`]).
//! - **Its label**, **what the engine says about it** (armed, or inert and why, and its findings), and for the
//!   keyboard: **Connect to…** (each output to a compatible input of another node) and **Delete**.
//!
//! Every edit is `automation.set_node` run as the person. A gesture (a slider's drag, text being typed) is one
//! transaction: begun at its first preview, committed at its end, rolled back on Esc, so it is one undo step.

use egui::{RichText, Ui};
use omuzaki_live_api::graph::NodeDef;
use omuzaki_live_api::ids::{GraphId, NodeId, PortName, PortRef};
use omuzaki_live_api::services::InertReason;
use omuzaki_live_api::value::{Coercion, PortType};
use omuzaki_ops_api::{OpError, TxId};
use schemars::Schema;
use serde_json::{Map, Value, json};

use super::model::{Described, KindInfo, NodeInfo, PortInfo, Show, node_name};
use super::text::{Word, say};
use crate::ds::schema_form::{EditPhase, Form, FormValue, SchemaForm};
use crate::ds::theme::ds;
use crate::ds::tokens::{Status as Tone, space};
use crate::ds::widgets::{Button, Choice, Dropdown, NumberField, StatusChip, TextField, Toggle};
use crate::panels::io::Io;

/// What the inspector asks its graph editor to do.
#[derive(Debug, Clone, PartialEq)]
pub enum InspectorAction {
    /// Wire `from` into `to`.
    Connect {
        /// The output.
        from: PortRef,
        /// The input.
        to: PortRef,
    },
    /// Remove the node.
    Delete(NodeId),
}

/// The form shown for one node, kept while it stays selected and the document doesn't change.
struct Shown {
    graph: GraphId,
    node: NodeId,
    form: Form,
    value: FormValue,
    label: String,
}

/// The inspector's state.
#[derive(Default)]
pub struct NodeInspector {
    shown: Option<Shown>,
    /// The open gesture's transaction.
    gesture: Option<TxId>,
    /// The output port picked in "Connect to…", and the input picked for it.
    connect_from: usize,
    connect_to: usize,
    /// Text being typed into a constant, by port.
    typing: std::collections::BTreeMap<PortName, String>,
    refused: Option<String>,
}

impl NodeInspector {
    /// Whether a gesture's transaction is open (the editor doesn't rebuild under it).
    pub fn in_gesture(&self) -> bool {
        self.gesture.is_some()
    }

    /// Forgets the form, so the next frame builds it from the document again.
    pub fn reset(&mut self) {
        if self.gesture.is_none() {
            self.shown = None;
        }
    }

    /// Draws the inspector for `node` of `graph` and returns what it asks the editor to do.
    #[allow(
        clippy::too_many_arguments,
        reason = "the editor's view of one node: the show, what the engine says of it and the kinds"
    )]
    pub fn ui(
        &mut self,
        ui: &mut Ui,
        io: &Io<'_>,
        show: &Show,
        described: Option<&Described>,
        kinds: &[KindInfo],
        graph: &GraphId,
        node: &NodeId,
    ) -> Option<InspectorAction> {
        let ctx = ui.ctx().clone();
        let theme = ds(&ctx).theme;
        let (Some(def), info) = (show.node(graph, node), described.and_then(|d| d.node(graph, node))) else {
            ui.label(RichText::new(say(&ctx, Word::NothingSelected, &[])).color(theme.text2));
            return None;
        };
        let kind = kinds.iter().find(|k| k.id == def.kind.as_str());
        if self.shown.as_ref().is_none_or(|s| &s.graph != graph || &s.node != node) {
            self.shown = Some(build(graph, node, def, kind));
            self.typing.clear();
            self.connect_from = 0;
            self.connect_to = 0;
        }
        let mut action = None;
        ui.label(RichText::new(node_name(node, def, kinds)).font(ds(&ctx).text.section.clone()));
        ui.label(RichText::new(format!("{} · {}", def.kind, node)).color(theme.text3).font(ds(&ctx).text.mono.clone()));
        match info.and_then(NodeInfo::inert) {
            None if info.is_some() => {
                ui.add(StatusChip::new(Tone::Ok, &say(&ctx, Word::Armed, &[])));
            }
            None => {}
            Some(why) => {
                ui.add(StatusChip::new(Tone::Warn, &inert_text(&ctx, &why)));
            }
        }
        for finding in described.map(|d| d.findings.as_slice()).unwrap_or_default() {
            if finding.graph.as_ref() == Some(graph) && finding.node.as_ref() == Some(node) {
                ui.label(RichText::new(format!("⚠ {}", finding.text)).color(theme.warn));
            }
        }
        if let Some(reason) = &self.refused {
            ui.label(RichText::new(say(&ctx, Word::Refused, &[("reason", reason)])).color(theme.danger));
        }
        ui.add_space(space::S8);
        self.label(ui, io, show, graph, node, def);
        ui.add_space(space::S8);
        self.params(ui, io, show, graph, node);
        if let Some(info) = info {
            self.constants(ui, io, show, graph, node, def, info);
            if let Some(picked) = self.connect(ui, show, described, graph, node, info) {
                action = Some(picked);
            }
        }
        ui.add_space(space::S8);
        let delete_label = say(&ctx, Word::DeleteNode, &[]);
        let delete = Button::new(&delete_label).danger().op("automation.remove_node");
        if ui.add(delete).clicked() {
            action = Some(InspectorAction::Delete(node.clone()));
        }
        action
    }

    fn label(&mut self, ui: &mut Ui, io: &Io<'_>, show: &Show, graph: &GraphId, node: &NodeId, def: &NodeDef) {
        let ctx = ui.ctx().clone();
        let Some(shown) = self.shown.as_mut() else { return };
        let field = say(&ctx, Word::NodeLabel, &[]);
        let response = ui.add(TextField::new(&field, &mut shown.label).id_salt(("node-label", node.as_str())));
        if response.lost_focus() {
            let label = shown.label.trim().to_owned();
            if def.label.as_deref().unwrap_or("") != label {
                let value = if label.is_empty() { Value::Null } else { json!(label) };
                let args = json!({ "show": show.id, "graph": graph, "node": node, "label": value });
                self.refused = io.run("automation.set_node", args, None).err().map(|e| e.message);
            }
        } else if !response.has_focus() {
            shown.label = def.label.clone().unwrap_or_default();
        }
    }

    fn params(&mut self, ui: &mut Ui, io: &Io<'_>, show: &Show, graph: &GraphId, node: &NodeId) {
        let label = super::text::text(ui.ctx(), Word::EditNode).into_owned();
        let Some(shown) = self.shown.as_mut() else { return };
        let out = SchemaForm::new(&shown.form, &mut shown.value).id_salt(("node-params", node.as_str())).show(ui);
        let mut failed = None;
        for edit in out.edits {
            let params = Value::from(&shown.value);
            let args = json!({ "show": show.id, "graph": graph, "node": node, "params": params });
            let result = match (edit.phase, self.gesture) {
                (EditPhase::Preview, None) => {
                    let tx = io.runner.begin(&label, io.actor);
                    self.gesture = Some(tx);
                    io.run("automation.set_node", args, Some(tx)).map(drop)
                }
                (EditPhase::Preview, Some(tx)) => io.run("automation.set_node", args, Some(tx)).map(drop),
                (EditPhase::Commit, Some(tx)) => io
                    .run("automation.set_node", args, Some(tx))
                    .and_then(|_| io.runner.commit(tx, io.actor))
                    .inspect(|()| self.gesture = None),
                (EditPhase::Commit, None) => io.run("automation.set_node", args, None).map(drop),
                (EditPhase::Cancel, Some(tx)) => {
                    self.gesture = None;
                    io.runner.rollback(tx, io.actor)
                }
                (EditPhase::Cancel, None) => Ok(()),
            };
            if let Err(error) = result {
                failed = Some(error);
                break;
            }
        }
        if let Some(error) = failed {
            self.fail(io, error);
        }
    }

    /// A refused edit: its gesture is rolled back, and the reason shown.
    fn fail(&mut self, io: &Io<'_>, error: OpError) {
        if let Some(tx) = self.gesture.take() {
            // The rollback's own failure would only repeat the refusal; the edit is gone either way.
            let _ = io.runner.rollback(tx, io.actor);
        }
        self.refused = Some(error.message);
        self.shown = None;
    }

    #[allow(clippy::too_many_arguments)]
    fn constants(
        &mut self,
        ui: &mut Ui,
        io: &Io<'_>,
        show: &Show,
        graph: &GraphId,
        node: &NodeId,
        def: &NodeDef,
        info: &NodeInfo,
    ) {
        let ctx = ui.ctx().clone();
        let unwired: Vec<&PortInfo> =
            info.inputs.iter().filter(|p| !p.ty.is_event() && !def.inputs.contains_key(&p.name)).collect();
        if unwired.is_empty() {
            return;
        }
        ui.add_space(space::S8);
        ui.label(RichText::new(say(&ctx, Word::Constants, &[])).font(ds(&ctx).text.strong.clone()));
        for port in unwired {
            let current = def.set.get(&port.name).cloned().or_else(|| port.default.clone()).unwrap_or(Value::Null);
            let typing = self.typing.entry(port.name.clone()).or_default();
            if let Some(value) = constant_field(ui, port, &current, typing, ("node-const", node.as_str())) {
                let args =
                    json!({ "show": show.id, "graph": graph, "node": node, "set": { port.name.as_str(): value } });
                self.refused = io.run("automation.set_node", args, None).err().map(|e| e.message);
            }
        }
    }

    fn connect(
        &mut self,
        ui: &mut Ui,
        show: &Show,
        described: Option<&Described>,
        graph: &GraphId,
        node: &NodeId,
        info: &NodeInfo,
    ) -> Option<InspectorAction> {
        let ctx = ui.ctx().clone();
        if info.outputs.is_empty() {
            return None;
        }
        let described = described?;
        ui.add_space(space::S8);
        ui.label(RichText::new(say(&ctx, Word::ConnectTo, &[])).font(ds(&ctx).text.strong.clone()));
        self.connect_from = self.connect_from.min(info.outputs.len() - 1);
        let outputs: Vec<Choice<'_, usize>> =
            info.outputs.iter().enumerate().map(|(i, p)| Choice::new(i, p.name.as_str())).collect();
        let from_label = say(&ctx, Word::Output, &[]);
        ui.add(Dropdown::new(&from_label, &mut self.connect_from, &outputs).id_salt(("connect-from", node.as_str())));
        let from = &info.outputs[self.connect_from];
        // Every input of another node of this graph that takes this output's type.
        let mut targets: Vec<(PortRef, String)> = Vec::new();
        if let Some(nodes) = described.graphs.get(graph) {
            for (other, other_info) in &nodes.nodes {
                if other == node {
                    continue;
                }
                let other_name = show
                    .node(graph, other)
                    .map_or_else(|| other.to_string(), |d| d.label.clone().unwrap_or_else(|| other.to_string()));
                for input in &other_info.inputs {
                    if compatible(from.ty, input.ty).is_ok() {
                        targets.push((
                            PortRef::new(other.clone(), input.name.clone()),
                            format!("{other_name}.{}", input.name),
                        ));
                    }
                }
            }
        }
        if targets.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::NoTargets, &[])).color(ds(&ctx).theme.text2));
            return None;
        }
        self.connect_to = self.connect_to.min(targets.len() - 1);
        let choices: Vec<Choice<'_, usize>> =
            targets.iter().enumerate().map(|(i, (_, name))| Choice::new(i, name.as_str())).collect();
        let to_label = say(&ctx, Word::Input, &[]);
        ui.add(Dropdown::new(&to_label, &mut self.connect_to, &choices).id_salt(("connect-to", node.as_str())));
        let connect_label = say(&ctx, Word::Connect, &[]);
        let wire = Button::new(&connect_label).op("automation.connect");
        if ui.add(wire).clicked() {
            let to = targets[self.connect_to].0.clone();
            return Some(InspectorAction::Connect { from: PortRef::new(node.clone(), from.name.clone()), to });
        }
        None
    }
}

/// The form for `def`'s parameters, from its kind's schema and defaults.
fn build(graph: &GraphId, node: &NodeId, def: &NodeDef, kind: Option<&KindInfo>) -> Shown {
    let schema = kind.and_then(|k| Schema::try_from(k.params.clone()).ok()).unwrap_or_default();
    let form = Form::from_schema(&schema);
    let mut params: Map<String, Value> = kind.and_then(|k| k.defaults.as_object().cloned()).unwrap_or_default();
    params.extend(def.params.iter().map(|(k, v)| (k.clone(), v.clone())));
    let value = form.value_from_json(&Value::Object(params));
    Shown { graph: graph.clone(), node: node.clone(), form, value, label: def.label.clone().unwrap_or_default() }
}

/// Whether an output of type `from` may be wired into an input of type `to`: events only to events, values only where
/// the coercion table allows.
///
/// # Errors
///
/// The reason, in words, when the wire isn't allowed.
pub fn compatible(from: PortType, to: PortType) -> Result<(), Word> {
    if from.is_event() != to.is_event() {
        return Err(Word::EventToValue);
    }
    match PortType::coercion(from, to) {
        Coercion::Refused => Err(Word::TypesDontMatch),
        _ => Ok(()),
    }
}

/// Why a node won't run, in words.
pub fn inert_text(ctx: &egui::Context, why: &InertReason) -> String {
    let word = match why {
        InertReason::UnknownKind => Word::InertUnknownKind,
        InertReason::BadParams => Word::InertBadParams,
        InertReason::NotApproved { .. } => Word::NeedsApproval,
        InertReason::OpNotAllowed => Word::InertOpNotAllowed,
        InertReason::BadWiring => Word::InertBadWiring,
        InertReason::Undeclared => Word::InertUndeclared,
        InertReason::UnknownEventKind => Word::InertUnknownEvent,
        InertReason::Faulted => Word::InertFaulted,
        _ => Word::InertOther,
    };
    say(ctx, word, &[])
}

/// A field for the constant of the unwired value input `port`, showing `current`; `typing` holds text being typed.
/// The new value once the person changed it (a toggle or number at once, text when the field is left).
pub fn constant_field(
    ui: &mut Ui,
    port: &PortInfo,
    current: &Value,
    typing: &mut String,
    salt: (&str, &str),
) -> Option<Value> {
    let label = port.summary.clone().unwrap_or_else(|| port.name.to_string());
    let salt = (salt.0, salt.1, port.name.as_str());
    match port.ty {
        PortType::Bool => {
            let mut on = current.as_bool().unwrap_or(false);
            ui.add(Toggle::switch(&label, &mut on).id_salt(salt)).changed().then(|| json!(on))
        }
        PortType::Number => {
            let mut n = current.as_f64().unwrap_or(0.0);
            let out = NumberField::new(&label, &mut n, -1.0e9..=1.0e9).id_salt(salt).show(ui);
            (out.response.changed() || out.reset).then(|| json!(n))
        }
        _ => {
            let shown = match current {
                Value::String(s) => s.clone(),
                Value::Null => String::new(),
                other => other.to_string(),
            };
            let response = ui.add(TextField::new(&label, typing).id_salt(salt));
            if response.lost_focus() {
                let text = std::mem::take(typing);
                if text == shown {
                    return None;
                }
                return Some(if port.ty == PortType::Text {
                    json!(text)
                } else {
                    serde_json::from_str(&text).unwrap_or(json!(text))
                });
            }
            if !response.has_focus() {
                *typing = shown;
            }
            None
        }
    }
}
