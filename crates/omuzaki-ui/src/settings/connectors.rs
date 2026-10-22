//! The Connectors page of the Settings window (#1446, CN-13): `VTube Studio`. Where the bridge stands and what to do
//! next, the setup steps while it isn't connected, the `Omuzaki*` parameters to map in VTS (a list to copy), the VTS
//! inputs Omuzaki feeds and the ones another plugin owns, and the table that maps Omuzaki's expressions to the loaded
//! model's hotkeys, with suggestions marked until the person confirms them. An expression file as a target is behind
//! an "advanced" disclosure with its warning. The footer says Omuzaki is not affiliated with VTS's makers, and the page
//! draws no logo or image of theirs: `VTube Studio` is plain text (research `2026-10-vts-plugin-api-terms.md` §2).
//!
//! Like the other pages it holds nothing of its own: it reads `vts.status` (when it opens, after each of its own
//! changes, and once a second, so an agent's connect or mapping shows) and every change is a `vts.*` op run as the
//! person. Connecting never waits on the page: `vts.connect` is asked not to wait, and the status read follows it.
//! Nothing reconnects by itself after access was revoked or denied; the person connects again.

use std::sync::Arc;

use egui::{RichText, ScrollArea, Ui, vec2};
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use serde_json::{Value, json};

use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{Status, space};
use crate::ds::widgets::{Button, EmptyState, Section, StatusChip, TextField};

/// Seconds between reads while the page is open.
const REFRESH_S: f64 = 1.0;

/// One mapping row as the page shows it.
#[derive(Clone, Debug, PartialEq)]
struct MapRow {
    omuzaki: String,
    /// The hotkey's name, or the expression file.
    target: String,
    /// The row as `vts.map` takes it.
    raw: Value,
    /// Proposed by `vts.suggest` and not confirmed yet.
    suggested: bool,
}

/// What the person asked for this frame.
#[derive(Clone, Copy)]
enum Action {
    Connect,
    Disconnect,
    Suggest,
    Confirm(usize),
    ConfirmAll,
    Remove(usize),
    AddExpression,
    Copy,
}

/// The page's state, kept while the Settings window is open.
#[derive(Default)]
pub struct ConnectorsPage {
    status: Option<Value>,
    read_at: Option<f64>,
    /// Why `vts.status` failed (no bridge in this build, for one).
    unreadable: Option<String>,
    /// Why the last change was refused.
    refused: Option<String>,
    /// The port the person typed; empty finds it.
    port: String,
    /// Rows `vts.suggest` proposed that the person hasn't confirmed.
    suggestions: Vec<MapRow>,
    /// The advanced row being written: an Omuzaki id and an expression file.
    expression_id: String,
    expression_file: String,
}

impl ConnectorsPage {
    /// The state as `vts.status` last answered (`state` is `disconnected`, `connecting`, `awaiting_approval`,
    /// `connected`, `revoked`, `denied` or `failed`).
    pub fn state(&self) -> Option<&str> {
        self.status.as_ref().and_then(|s| s["state"].as_str())
    }

    /// Draws the page; `now` is egui's clock.
    pub fn show(&mut self, ui: &mut Ui, runner: &Arc<dyn Runner>, actor: &Actor, now: f64) {
        let ctx = ui.ctx().clone();
        if self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(runner.as_ref(), actor, now);
        }
        let mut actions = Vec::new();
        ScrollArea::vertical().id_salt("settings.connectors.scroll").auto_shrink([false, false]).show(ui, |ui| {
            ui.label(RichText::new(say(&ctx, Word::VtsTitle, &[])).font(ds(&ctx).text.section.clone()));
            ui.add_space(space::S4);
            if let Some(reason) = &self.refused {
                ui.label(RichText::new(say(&ctx, Word::NotSaved, &[("reason", reason)])).color(ds(&ctx).theme.danger));
            }
            match self.status.clone() {
                None => {
                    let reason = self.unreadable.clone().unwrap_or_default();
                    let headline = say(&ctx, Word::VtsUnavailable, &[]);
                    let line = say(&ctx, Word::UnavailableLine, &[("reason", &reason)]);
                    EmptyState::new(icons::GEAR, &headline, &line).show(ui);
                }
                Some(status) => self.draw(ui, &status, &mut actions),
            }
            ui.add_space(space::S12);
            ui.label(
                RichText::new(say(&ctx, Word::VtsNotAffiliated, &[]))
                    .font(ds(&ctx).text.small.clone())
                    .color(ds(&ctx).theme.text2),
            );
        });
        for action in actions {
            self.act(ui, runner.as_ref(), actor, action, now);
        }
    }

    fn draw(&mut self, ui: &mut Ui, status: &Value, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let state = status["state"].as_str().unwrap_or("disconnected");
        let (chip, line) = match state {
            "connected" => {
                let version = status["vts_version"].as_str().unwrap_or_default();
                let chip = say(&ctx, Word::VtsConnected, &[("version", version)]);
                let line = status["model"]["name"].as_str().map_or_else(
                    || say(&ctx, Word::VtsNoModel, &[]),
                    |name| say(&ctx, Word::VtsModel, &[("name", name)]),
                );
                ((Status::Ok, chip), Some(line))
            }
            "awaiting_approval" => ((Status::Warn, say(&ctx, Word::VtsAwaiting, &[])), None),
            "connecting" => ((Status::Info, say(&ctx, Word::VtsConnecting, &[])), None),
            "revoked" => {
                ((Status::Danger, say(&ctx, Word::VtsRevoked, &[])), Some(say(&ctx, Word::VtsAllowAgain, &[])))
            }
            "denied" => ((Status::Danger, say(&ctx, Word::VtsDenied, &[])), Some(say(&ctx, Word::VtsAllowAgain, &[]))),
            "failed" => {
                let reason = status["reason"].as_str().unwrap_or_default();
                ((Status::Danger, say(&ctx, Word::VtsFailed, &[("reason", reason)])), None)
            }
            _ => ((Status::Info, say(&ctx, Word::VtsDisconnected, &[])), None),
        };
        ui.add(StatusChip::new(chip.0, &chip.1));
        if let Some(line) = line {
            ui.label(RichText::new(line).font(ds(&ctx).text.small.clone()).color(ds(&ctx).theme.text2));
        }
        ui.add_space(space::S6);
        let connected = state == "connected";
        ui.horizontal(|ui| {
            if connected {
                if ui.add(Button::new(&say(&ctx, Word::VtsDisconnect, &[])).op("vts.disconnect")).clicked() {
                    actions.push(Action::Disconnect);
                }
            } else if state != "connecting" && state != "awaiting_approval" {
                let port = say(&ctx, Word::VtsPort, &[]);
                let hint = say(&ctx, Word::VtsPortHint, &[]);
                ui.add(
                    TextField::new(&port, &mut self.port)
                        .hint(&hint)
                        .width(160.0)
                        .clearable(false)
                        .id_salt("settings.vts.port"),
                );
                if ui.add(Button::new(&say(&ctx, Word::VtsConnect, &[])).primary().op("vts.connect")).clicked() {
                    actions.push(Action::Connect);
                }
            }
        });
        if !connected {
            ui.add_space(space::S8);
            for (n, step) in [Word::VtsStep1, Word::VtsStep2, Word::VtsStep3].into_iter().enumerate() {
                let number = (n + 1).to_string();
                ui.label(say(&ctx, Word::VtsStep, &[("n", &number), ("step", &say(&ctx, step, &[]))]));
            }
        }
        if connected && status["model"].is_object() {
            ui.add_space(space::S12);
            self.draw_mapping(ui, status, actions);
        }
        ui.add_space(space::S12);
        draw_parameters(ui, status, actions);
    }

    fn draw_mapping(&mut self, ui: &mut Ui, status: &Value, actions: &mut Vec<Action>) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        ui.label(RichText::new(say(&ctx, Word::VtsMapping, &[])).font(ds.text.strong.clone()));
        let rows = self.rows(status);
        if rows.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::VtsMappingNone, &[])).color(ds.theme.text2));
        }
        let (suggested, remove, confirm) =
            (say(&ctx, Word::VtsSuggested, &[]), say(&ctx, Word::VtsRemove, &[]), say(&ctx, Word::VtsConfirm, &[]));
        let suggested = RichText::new(suggested).font(ds.text.small.clone()).color(ds.theme.text2);
        egui::Grid::new("settings.vts.mapping").num_columns(4).spacing(vec2(space::S8, space::S4)).show(ui, |ui| {
            for (i, row) in rows.iter().enumerate() {
                ui.label(RichText::new(row.omuzaki.as_str()).font(ds.text.mono.clone()));
                ui.label(say(&ctx, Word::VtsRuns, &[("target", &row.target)]));
                if row.suggested {
                    ui.label(suggested.clone());
                    let name = say(&ctx, Word::VtsConfirmRow, &[("id", &row.omuzaki)]);
                    if ui.add(Button::new(&confirm).accessible_label(&name).op("vts.map")).clicked() {
                        actions.push(Action::Confirm(i));
                    }
                } else {
                    ui.label("");
                    let name = say(&ctx, Word::VtsRemoveRow, &[("id", &row.omuzaki)]);
                    if ui.add(Button::new(&remove).ghost().accessible_label(&name).op("vts.map")).clicked() {
                        actions.push(Action::Remove(i));
                    }
                }
                ui.end_row();
            }
        });
        ui.horizontal(|ui| {
            if ui.add(Button::new(&say(&ctx, Word::VtsSuggest, &[])).op("vts.suggest")).clicked() {
                actions.push(Action::Suggest);
            }
            if !self.suggestions.is_empty()
                && ui.add(Button::new(&say(&ctx, Word::VtsConfirmAll, &[])).op("vts.map")).clicked()
            {
                actions.push(Action::ConfirmAll);
            }
        });
        ui.add_space(space::S8);
        Section::new(&say(&ctx, Word::VtsAdvanced, &[])).id_salt("settings.vts.advanced").show(ui, |ui| {
            ui.label(RichText::new(say(&ctx, Word::VtsExpressionWarning, &[])).color(ds.theme.warn));
            let (id, file) = (say(&ctx, Word::VtsExpressionId, &[]), say(&ctx, Word::VtsExpressionFile, &[]));
            let file_hint = say(&ctx, Word::VtsExpressionFileHint, &[]);
            let id_hint = say(&ctx, Word::VtsExpressionIdHint, &[]);
            ui.add(TextField::new(&id, &mut self.expression_id).hint(&id_hint).id_salt("settings.vts.expression-id"));
            ui.add(TextField::new(&file, &mut self.expression_file).hint(&file_hint).id_salt("settings.vts.file"));
            if ui.add(Button::new(&say(&ctx, Word::VtsAddExpression, &[])).op("vts.map")).clicked() {
                actions.push(Action::AddExpression);
            }
        });
    }

    /// The stored rows, then the suggestions for ids that have none.
    fn rows(&self, status: &Value) -> Vec<MapRow> {
        let mut rows: Vec<MapRow> = status["mapping"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|r| map_row(r, false)).collect())
            .unwrap_or_default();
        for suggestion in &self.suggestions {
            if !rows.iter().any(|r| r.omuzaki == suggestion.omuzaki) {
                rows.push(suggestion.clone());
            }
        }
        rows
    }

    fn act(&mut self, ui: &Ui, runner: &dyn Runner, actor: &Actor, action: Action, now: f64) {
        let status = self.status.clone().unwrap_or(Value::Null);
        let rows = self.rows(&status);
        let stored: Vec<Value> = rows.iter().filter(|r| !r.suggested).map(|r| r.raw.clone()).collect();
        let result = match action {
            Action::Connect => {
                let mut args = json!({ "wait_s": 0.0 });
                if let Ok(port) = self.port.trim().parse::<u16>() {
                    args["port"] = json!(port);
                }
                run(runner, actor, "vts.connect", args)
            }
            Action::Disconnect => run(runner, actor, "vts.disconnect", json!({})),
            Action::Suggest => self.suggest(runner, actor, &rows),
            Action::Confirm(i) => match rows.get(i) {
                Some(row) => {
                    let answer = run(runner, actor, "vts.map", json!({ "rows": [row.raw], "merge": true }));
                    self.suggestions.retain(|s| s.omuzaki != row.omuzaki);
                    answer
                }
                None => Ok(Value::Null),
            },
            Action::ConfirmAll => {
                let rows: Vec<Value> = self.suggestions.drain(..).map(|r| r.raw).collect();
                run(runner, actor, "vts.map", json!({ "rows": rows, "merge": true }))
            }
            Action::Remove(i) => {
                let keep: Vec<Value> =
                    stored.iter().enumerate().filter(|(n, _)| *n != i).map(|(_, r)| r.clone()).collect();
                run(runner, actor, "vts.map", json!({ "rows": keep }))
            }
            Action::AddExpression => {
                let target = json!({ "kind": "expression", "file": self.expression_file.trim(), "fade_s": 0.5 });
                let row = json!({ "omuzaki": self.expression_id.trim(), "target": target });
                let answer = run(runner, actor, "vts.map", json!({ "rows": [row], "merge": true }));
                if answer.is_ok() {
                    (self.expression_id, self.expression_file) = (String::new(), String::new());
                }
                answer
            }
            Action::Copy => {
                ui.ctx().copy_text(parameter_names(&status).join("\n"));
                Ok(Value::Null)
            }
        };
        self.refused = result.err().map(|error| error.message);
        self.read(runner, actor, now);
    }

    /// Runs `vts.suggest` for the character's expressions (`trigger.list` lists them and the bound keys' ones) and the
    /// mapped ids.
    fn suggest(&mut self, runner: &dyn Runner, actor: &Actor, rows: &[MapRow]) -> Result<Value, OpError> {
        let listed = run(runner, actor, "trigger.list", json!({})).unwrap_or(Value::Null);
        let library = listed["expressions"].as_array().into_iter().flatten().filter_map(Value::as_str);
        let bound =
            listed["triggers"].as_array().into_iter().flatten().filter_map(|t| t["binding"]["expression"].as_str());
        let mut ids: Vec<String> = library.chain(bound).map(str::to_owned).collect();
        ids.extend(rows.iter().map(|r| r.omuzaki.clone()));
        ids.sort();
        ids.dedup();
        let answer = run(runner, actor, "vts.suggest", json!({ "ids": ids }))?;
        self.suggestions = answer["rows"]
            .as_array()
            .map(|rows| rows.iter().filter_map(|r| map_row(r, true)).collect())
            .unwrap_or_default();
        Ok(answer)
    }

    /// Reads `vts.status`.
    fn read(&mut self, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        match run(runner, actor, "vts.status", json!({})) {
            Ok(status) => {
                self.status = Some(status);
                self.unreadable = None;
            }
            Err(error) => {
                self.status = None;
                self.unreadable = Some(error.message);
            }
        }
    }
}

/// The parameters to map in VTS, the VTS inputs Omuzaki feeds, and the ones another plugin owns.
fn draw_parameters(ui: &mut Ui, status: &Value, actions: &mut Vec<Action>) {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    ui.label(RichText::new(say(&ctx, Word::VtsParameters, &[])).font(ds.text.strong.clone()));
    ui.label(RichText::new(say(&ctx, Word::VtsParametersLine, &[])).font(ds.text.small.clone()).color(ds.theme.text2));
    let names = parameter_names(status);
    if !names.is_empty() {
        ui.add_space(space::S4);
        ui.horizontal(|ui| {
            let count = names.len().to_string();
            ui.label(say(&ctx, Word::VtsParameterCount, &[("count", &count)]));
            if ui.add(Button::new(&say(&ctx, Word::VtsCopy, &[]))).clicked() {
                actions.push(Action::Copy);
            }
        });
    }
    let inputs: Vec<(&str, bool)> = status["default_inputs"]
        .as_array()
        .map(|d| d.iter().filter_map(|d| Some((d["name"].as_str()?, d["on"].as_bool().unwrap_or(false)))).collect())
        .unwrap_or_default();
    // The names themselves (some eighty) and the inputs are there to check, not to read every time: folded away.
    Section::new(&say(&ctx, Word::VtsShowNames, &[])).id_salt("settings.vts.names").show(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            for name in &names {
                ui.label(RichText::new(name.as_str()).font(ds.text.mono.clone()));
            }
        });
        if !inputs.is_empty() {
            ui.add_space(space::S6);
            ui.label(RichText::new(say(&ctx, Word::VtsInputs, &[])).font(ds.text.small.clone()).color(ds.theme.text2));
            ui.horizontal_wrapped(|ui| {
                for (name, on) in inputs {
                    let word = if on { Word::VtsInputOn } else { Word::VtsInputOff };
                    ui.label(say(&ctx, word, &[("name", name)]));
                }
            });
        }
    });
    for conflict in status["conflicts"].as_array().into_iter().flatten().filter_map(Value::as_str) {
        ui.add(StatusChip::new(Status::Warn, &say(&ctx, Word::VtsConflict, &[("name", conflict)])));
    }
}

/// The custom parameter names `vts.status` lists.
fn parameter_names(status: &Value) -> Vec<String> {
    status["custom_parameters"]
        .as_array()
        .map(|names| names.iter().filter_map(|n| n.as_str().map(str::to_owned)).collect())
        .unwrap_or_default()
}

/// A mapping row from `vts.status` or `vts.suggest`.
fn map_row(raw: &Value, suggested: bool) -> Option<MapRow> {
    let omuzaki = raw["omuzaki"].as_str()?.to_owned();
    let target = &raw["target"];
    let target = match target["kind"].as_str()? {
        "hotkey" => target["name"].as_str()?.to_owned(),
        "expression" => target["file"].as_str()?.to_owned(),
        _ => return None,
    };
    Some(MapRow { omuzaki, target, raw: raw.clone(), suggested })
}

/// Runs `op` as the person and returns its answer.
fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
    let outcome = runner.run(Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    })?;
    Ok(outcome.plan.output.unwrap_or(Value::Null))
}
