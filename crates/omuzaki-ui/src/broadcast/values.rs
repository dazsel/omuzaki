//! The values monitor (`docs/design/automation.md` D10, D12; #739): a centre tab of the Broadcast workspace that shows
//! the running show from the inside.
//!
//! - **Variables:** name, scope, type, value and when and by which node it last changed, with a filter. A value is
//!   edited in place through `automation.set_variable`; there is no other way to change one. Secrets are never
//!   variables, so none can show here.
//! - **Groups** and their runtime state, **graphs** (running, suspended with Resume through
//!   `automation.resume_graph`, inert and faulted nodes), the **last 200 firings** and **last 50 events**, what the
//!   engine **dropped**, and how many approvals are still **missing**.
//!
//! The engine refreshes the snapshot only while someone reads it (D10), so the monitor asks at most four times a
//! second and only while it is drawn: a hidden tab costs the engine nothing. The long lists draw only their visible
//! rows.

use std::collections::BTreeMap;
use std::time::Duration;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_live_api::ids::VarName;
use omuzaki_live_api::services::{Dropped, ValuesSnapshot, VarRow, shown};
use omuzaki_live_api::show::{VarScope, VarType};
use serde_json::{Value, json};

use super::model;
use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::Status as Tone;
use crate::ds::widgets::{Banner, Button, EmptyState, NumberField, Section, StatusChip, TextField, Toggle};
use crate::panels::io::Io;

/// The shortest wait between two reads: at most four a second.
pub const READ_EVERY: Duration = Duration::from_millis(250);
/// A row's height in the long lists, in points.
const ROW: f32 = 22.0;
/// The most rows a long list shows before it scrolls.
const LIST_ROWS: f32 = 10.0;

/// What the person asked for this frame.
enum Action {
    Set { name: VarName, value: Value },
    ResumeGraph(omuzaki_live_api::ids::GraphId),
}

/// The monitor's state.
#[derive(Default)]
pub struct ValuesPanel {
    snapshot: Option<ValuesSnapshot>,
    /// Whether a show runs, as last read; `None` before the first read.
    running: Option<bool>,
    read_at: Option<f64>,
    unreadable: Option<String>,
    refused: Option<String>,
    filter: String,
    /// Text being typed into a variable's value.
    typing: BTreeMap<VarName, String>,
    reads: u64,
}

impl ValuesPanel {
    /// A monitor that has read nothing yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// How many times it asked the engine for values: for tests of the read budget.
    pub fn reads(&self) -> u64 {
        self.reads
    }

    /// Draws the monitor.
    pub fn ui(&mut self, ui: &mut Ui, io: Io<'_>) {
        let now = ui.ctx().input(|i| i.time);
        if self.read_at.is_none_or(|at| now - at >= READ_EVERY.as_secs_f64()) {
            self.read(&io, now);
        }
        ui.ctx().request_repaint_after(READ_EVERY);
        let ctx = ui.ctx().clone();
        if let Some(reason) = &self.unreadable {
            let line = say(&ctx, Word::ValuesUnreadable, &[("reason", reason)]);
            EmptyState::new(icons::WARNING, &say(&ctx, Word::ValuesTitle, &[]), &line).show(ui);
            return;
        }
        let Some(snapshot) = self.snapshot.take() else {
            let headline = say(&ctx, Word::NotRunning, &[]);
            EmptyState::new(icons::ACTIVITY, &headline, &say(&ctx, Word::NotRunningLine, &[])).show(ui);
            return;
        };
        if let Some(reason) = &self.refused {
            Banner::new(Tone::Danger, &say(&ctx, Word::Refused, &[("reason", reason)])).show(ui);
        }
        let mut actions = Vec::new();
        ScrollArea::vertical().id_salt("broadcast.values.scroll").auto_shrink([false, false]).show(ui, |ui| {
            self.variables(ui, &snapshot, &mut actions);
            groups(ui, &snapshot);
            graphs(ui, &snapshot, &mut actions);
            firings(ui, &snapshot);
            events(ui, &snapshot);
            footer(ui, &snapshot.dropped, snapshot.missing.len());
        });
        self.snapshot = Some(snapshot);
        for action in actions {
            let ran = match action {
                Action::Set { name, value } => {
                    io.run("automation.set_variable", json!({ "name": name, "value": value }), None)
                }
                Action::ResumeGraph(graph) => io.run("automation.resume_graph", json!({ "graph": graph }), None),
            };
            self.refused = ran.err().map(|e| e.message);
            self.read_at = None;
        }
    }

    fn read(&mut self, io: &Io<'_>, now: f64) {
        self.read_at = Some(now);
        self.reads += 1;
        match model::values(io) {
            Ok(snapshot) => {
                self.running = Some(snapshot.is_some());
                self.snapshot = snapshot;
                self.unreadable = None;
            }
            Err(error) => {
                self.snapshot = None;
                self.unreadable = Some(error.message);
            }
        }
    }

    fn variables(&mut self, ui: &mut Ui, snapshot: &ValuesSnapshot, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let count = snapshot.variables.len().to_string();
        let title = say(&ctx, Word::Variables, &[]);
        Section::new(&title).badge(&count).default_open(true).id_salt("broadcast.values.variables").show(ui, |ui| {
            let filter_label = say(&ctx, Word::FilterVariables, &[]);
            ui.add(TextField::new(&filter_label, &mut self.filter).search().id_salt("broadcast.values.filter"));
            let words = self.filter.to_lowercase();
            let rows: Vec<&VarRow> = snapshot
                .variables
                .iter()
                .filter(|v| words.is_empty() || v.name.as_str().to_lowercase().contains(&words))
                .collect();
            if rows.is_empty() {
                ui.label(RichText::new(say(&ctx, Word::NoVariables, &[])).color(ds(&ctx).theme.text2));
                return;
            }
            let height = ROW * 1.6;
            ScrollArea::vertical()
                .id_salt("broadcast.values.variables.rows")
                .max_height(height * LIST_ROWS)
                .auto_shrink([false, true])
                .show_rows(ui, height, rows.len(), |ui, range| {
                    for row in &rows[range] {
                        self.variable(ui, row, snapshot, actions);
                    }
                });
        });
    }

    fn variable(&mut self, ui: &mut Ui, row: &VarRow, snapshot: &ValuesSnapshot, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let theme = ds(&ctx).theme;
        ui.horizontal(|ui| {
            ui.set_height(ROW * 1.6);
            ui.label(RichText::new(row.name.as_str()).font(ds(&ctx).text.mono.clone()));
            let scope = match row.scope {
                VarScope::Session => Word::ScopeSession,
                VarScope::Show => Word::ScopeShow,
                _ => Word::ScopeGlobal,
            };
            ui.add(StatusChip::new(Tone::Info, &say(&ctx, scope, &[])));
            let salt = ("var", row.name.as_str());
            let label = say(&ctx, Word::ValueOf, &[("name", row.name.as_str())]);
            match row.ty {
                VarType::Bool => {
                    let mut on = row.value.as_bool().unwrap_or(false);
                    if ui.add(Toggle::switch(&label, &mut on).id_salt(salt)).changed() {
                        actions.push(Action::Set { name: row.name.clone(), value: json!(on) });
                    }
                }
                VarType::Number => {
                    let mut n = row.value.as_f64().unwrap_or(0.0);
                    let out = NumberField::new(&label, &mut n, -1.0e12..=1.0e12).width(96.0).id_salt(salt).show(ui);
                    if out.response.changed() || out.reset {
                        actions.push(Action::Set { name: row.name.clone(), value: json!(n) });
                    }
                }
                VarType::Text => {
                    let current = row.value.as_str().unwrap_or_default().to_owned();
                    let typed = self.typing.entry(row.name.clone()).or_insert_with(|| current.clone());
                    let response = ui.add(TextField::new(&label, typed).width(160.0).id_salt(salt));
                    if response.lost_focus() {
                        let text = self.typing.remove(&row.name).unwrap_or_default();
                        if text != current {
                            actions.push(Action::Set { name: row.name.clone(), value: json!(text) });
                        }
                    } else if !response.has_focus() {
                        *typed = current;
                    }
                }
                _ => {
                    ui.label(RichText::new(shown(&row.value)).font(ds(&ctx).text.mono.clone()));
                }
            }
            let changed = match (&row.changed_at, &row.changed_by) {
                (Some(at), by) => {
                    let ago = format!("{:.0}", (snapshot.at.0 - at.0).max(0.0));
                    match by {
                        Some(by) => say(
                            &ctx,
                            Word::ChangedBy,
                            &[("seconds", &ago), ("node", &format!("{}/{}", by.graph, by.node))],
                        ),
                        None => say(&ctx, Word::Changed, &[("seconds", &ago)]),
                    }
                }
                (None, _) => say(&ctx, Word::Unchanged, &[]),
            };
            ui.label(RichText::new(changed).color(theme.text3));
        });
    }
}

fn groups(ui: &mut Ui, snapshot: &ValuesSnapshot) {
    if snapshot.groups.is_empty() {
        return;
    }
    let ctx = ui.ctx().clone();
    let title = say(&ctx, Word::Groups, &[]);
    Section::new(&title).default_open(true).id_salt("broadcast.values.groups").show(ui, |ui| {
        for group in &snapshot.groups {
            ui.horizontal(|ui| {
                ui.label(group.group.as_str());
                let (tone, word) = if group.on { (Tone::Ok, Word::On) } else { (Tone::Info, Word::Off) };
                ui.add(StatusChip::new(tone, &say(&ctx, word, &[])));
            });
        }
    });
}

fn graphs(ui: &mut Ui, snapshot: &ValuesSnapshot, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let title = say(&ctx, Word::Graphs, &[]);
    Section::new(&title).default_open(true).id_salt("broadcast.values.graphs").show(ui, |ui| {
        for graph in &snapshot.graphs {
            ui.horizontal_wrapped(|ui| {
                ui.label(graph.graph.as_str());
                if graph.suspended.is_some() {
                    ui.add(StatusChip::new(Tone::Warn, &say(&ctx, Word::Suspended, &[])));
                    let resume_label = say(&ctx, Word::ResumeGraph, &[]);
                    let resume = Button::new(&resume_label).op("automation.resume_graph");
                    if ui.add(resume).clicked() {
                        actions.push(Action::ResumeGraph(graph.graph.clone()));
                    }
                } else if graph.active {
                    ui.add(StatusChip::new(Tone::Ok, &say(&ctx, Word::Running, &[])));
                } else {
                    ui.add(StatusChip::new(Tone::Info, &say(&ctx, Word::Off, &[])));
                }
                if !graph.inert.is_empty() {
                    let count = graph.inert.len().to_string();
                    ui.add(StatusChip::new(Tone::Warn, &say(&ctx, Word::InertNodes, &[("count", &count)])));
                }
                if !graph.faulted.is_empty() {
                    let count = graph.faulted.len().to_string();
                    ui.add(StatusChip::new(Tone::Danger, &say(&ctx, Word::Faulted, &[("count", &count)])));
                }
            });
        }
    });
}

fn firings(ui: &mut Ui, snapshot: &ValuesSnapshot) {
    let ctx = ui.ctx().clone();
    let title = say(&ctx, Word::Firings, &[]);
    let count = snapshot.firings.len().to_string();
    Section::new(&title).badge(&count).default_open(true).id_salt("broadcast.values.firings").show(ui, |ui| {
        if snapshot.firings.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::NoFirings, &[])).color(ds(&ctx).theme.text2));
            return;
        }
        let mono = ds(&ctx).text.mono.clone();
        ScrollArea::vertical()
            .id_salt("broadcast.values.firings.rows")
            .max_height(ROW * LIST_ROWS)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show_rows(ui, ROW, snapshot.firings.len(), |ui, range| {
                for firing in &snapshot.firings[range] {
                    let values: Vec<String> = firing.values.iter().map(|(port, v)| format!("{port}={v}")).collect();
                    let line = format!(
                        "{:>8.2}  {}/{}.{}  {}",
                        firing.at.0,
                        firing.node.graph,
                        firing.node.node,
                        firing.port,
                        values.join(" ")
                    );
                    ui.label(RichText::new(line).font(mono.clone()));
                }
            });
    });
}

fn events(ui: &mut Ui, snapshot: &ValuesSnapshot) {
    let ctx = ui.ctx().clone();
    let title = say(&ctx, Word::Events, &[]);
    let count = snapshot.events.len().to_string();
    Section::new(&title).badge(&count).default_open(true).id_salt("broadcast.values.events").show(ui, |ui| {
        if snapshot.events.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::NoEvents, &[])).color(ds(&ctx).theme.text2));
            return;
        }
        let mono = ds(&ctx).text.mono.clone();
        ScrollArea::vertical()
            .id_salt("broadcast.values.events.rows")
            .max_height(ROW * LIST_ROWS)
            .auto_shrink([false, true])
            .stick_to_bottom(true)
            .show_rows(ui, ROW, snapshot.events.len(), |ui, range| {
                for event in &snapshot.events[range] {
                    let source = source_name(&serde_json::to_value(&event.source).unwrap_or(Value::Null));
                    let line = format!("{:>8.2}  {}  {}  {}", event.at.0, event.kind.as_str(), source, event.summary);
                    ui.label(RichText::new(line).font(mono.clone()));
                }
            });
    });
}

/// A source's short name: its variant, or a platform's name.
fn source_name(source: &Value) -> String {
    match source {
        Value::String(name) => name.clone(),
        Value::Object(map) => match map.iter().next() {
            Some((variant, inner)) => {
                inner.get("platform").and_then(Value::as_str).map_or_else(|| variant.clone(), str::to_owned)
            }
            None => String::new(),
        },
        _ => String::new(),
    }
}

fn footer(ui: &mut Ui, dropped: &Dropped, missing: usize) {
    let ctx = ui.ctx().clone();
    let counts = [
        (Word::DropRate, dropped.rate),
        (Word::DropInbox, dropped.inbox + dropped.inbox_full),
        (Word::DropProducer, dropped.producer),
        (Word::DropInert, dropped.inert),
        (Word::DropInactive, dropped.inactive),
        (Word::DropReload, dropped.reload),
        (Word::DropPaused, dropped.paused),
        (Word::DropDuplicate, dropped.duplicate),
    ];
    let title = say(&ctx, Word::Dropped, &[]);
    let total: u64 = counts.iter().map(|(_, n)| n).sum();
    Section::new(&title).badge(&total.to_string()).default_open(false).id_salt("broadcast.values.dropped").show(
        ui,
        |ui| {
            for (word, n) in counts {
                ui.label(say(&ctx, word, &[("count", &n.to_string())]));
            }
        },
    );
    if missing > 0 {
        let text = say(&ctx, Word::MissingApprovals, &[("count", &missing.to_string())]);
        ui.add(StatusChip::new(Tone::Warn, &text));
    }
}
