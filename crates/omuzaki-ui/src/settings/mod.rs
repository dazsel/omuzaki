//! The Settings window (`docs/design/settings.md` D2, D4, D7; `shell.md` section 7; #948): every registered setting,
//! generated from the registry and each key's JSON Schema, in one section per area, with a search.
//!
//! Each row is the shell's generated control for the key's schema (a segmented control for a short enum, a dropdown for
//! a long one, a toggle, a slider, a text field), with the "modified from default" dot and a reset. Under it: the key's
//! summary, "Applies after restart" for a key that waits for the next start, "This computer" for a key that stays on
//! this machine, and the agent that changed it last, when an agent did.
//!
//! The window holds no settings of its own. It reads them through `settings.list` and `settings.changes` (when it opens,
//! after each of its own edits, and once a second while it is open, so an agent's change shows), and every change is a
//! `settings.set` or `settings.reset` run as the person: one op in history, undone like any other. The schemas come from
//! the registry, the same ones `settings.get` gives an agent, so the window and MCP cannot drift.

pub mod connectors;
pub mod models;
pub mod text;

use std::sync::Arc;

use egui::{Context, Id, Key, RichText, ScrollArea, Ui, Window, vec2};
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_settings_api::registry;
use schemars::Schema;
use serde_json::{Map, Value, json};

use self::connectors::{ConnectorsPage, Keychain};
use self::models::ModelsPage;
use self::text::{Word, area_title, humanise, say, title};
use crate::ds::icons;
use crate::ds::schema_form::{EditPhase, Form, FormValue, SchemaForm};
use crate::ds::theme::ds;
use crate::ds::tokens::space;
use crate::ds::widgets::{ActorChip, ActorKind, EmptyState, Segment, Segmented, TextField};

/// Seconds between reads while the window is open.
const REFRESH_S: f64 = 1.0;
/// How many of the newest changes are read to find the agent that changed each key last.
const CHANGES: usize = 200;
/// The property each row's one-field form holds its value under.
const FIELD: &str = "value";
/// Settings a page of their own edits in words: the list leaves them out rather than show their raw value (the
/// Connectors page writes `connectors.entries`, a map a person would otherwise read as `{}`).
const OWN_PAGE: &[&str] = &[omuzaki_connect::settings::KEY];

/// One setting's row.
struct Row {
    key: String,
    area: String,
    /// The label, as the person reads it.
    label: String,
    /// The registration's one line.
    summary: String,
    machine: bool,
    next_start: bool,
    /// The agent that changed it last, when the newest change was an agent's.
    agent: Option<String>,
    default: Value,
    form: Form,
    value: FormValue,
}

impl Row {
    /// Whether the search `needle` (lowercase) is in the key, the label or the summary.
    fn matches(&self, needle: &str) -> bool {
        needle.is_empty()
            || [&self.key, &self.label, &self.summary].iter().any(|text| text.to_lowercase().contains(needle))
    }
}

/// The window's pages.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Page {
    /// Every registered setting.
    #[default]
    Settings,
    /// The model slots and their models.
    Models,
    /// The connections to other apps: OBS Studio and `VTube Studio`.
    Connectors,
}

/// The Settings window's state. Make it when the window opens and drop it when it closes.
#[derive(Default)]
pub struct SettingsWindow {
    page: Page,
    models: ModelsPage,
    connectors: ConnectorsPage,
    query: String,
    rows: Vec<Row>,
    /// When the settings were last read, on egui's clock.
    read_at: Option<f64>,
    /// Why the settings could not be read.
    unreadable: Option<String>,
    /// Why the last change was refused.
    refused: Option<String>,
    /// The language the rows' words were made in.
    locale: String,
}

impl std::fmt::Debug for SettingsWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SettingsWindow")
            .field("query", &self.query)
            .field("rows", &self.rows.len())
            .finish_non_exhaustive()
    }
}

impl SettingsWindow {
    /// A window that reads the settings on its first frame.
    pub fn new() -> Self {
        Self::default()
    }

    /// A window open on `page`.
    pub fn on(page: Page) -> Self {
        Self { page, ..Self::default() }
    }

    /// The same window, keeping the OBS password in `keychain` (the app's; see [`Keychain`]).
    #[must_use]
    pub fn with_keychain(mut self, keychain: Option<Keychain>) -> Self {
        self.connectors = std::mem::take(&mut self.connectors).with_keychain(keychain);
        self
    }

    /// The page on show.
    pub const fn page(&self) -> Page {
        self.page
    }

    /// The Models page.
    pub fn models(&mut self) -> &mut ModelsPage {
        &mut self.models
    }

    /// The Connectors page.
    pub fn connectors(&mut self) -> &mut ConnectorsPage {
        &mut self.connectors
    }

    /// The keys on show, for the search, in the order drawn.
    pub fn listed(&self) -> Vec<&str> {
        let needle = self.query.trim().to_lowercase();
        self.rows.iter().filter(|row| row.matches(&needle)).map(|row| row.key.as_str()).collect()
    }

    /// Draws the window; false once the person closed it (its close button or Esc).
    pub fn show(&mut self, ctx: &Context, runner: &Arc<dyn Runner>, actor: &Actor) -> bool {
        let now = ctx.input(|i| i.time);
        let locale = ds(ctx).strings.locale().to_owned();
        if locale != self.locale {
            // The rows' labels, summaries and option names are words: make them again in the new language.
            self.rows.clear();
            self.read_at = None;
            self.locale = locale;
        }
        if self.page == Page::Settings && self.read_at.is_none_or(|at| now - at >= REFRESH_S) {
            self.read(ctx, runner.as_ref(), actor, now);
        }
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(REFRESH_S));
        let mut open = true;
        let heading = say(ctx, Word::Title, &[]);
        Window::new(&heading)
            .id(Id::new("settings_window"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(vec2(720.0, 640.0))
            .min_size(vec2(360.0, 280.0))
            .show(ctx, |ui| {
                let label = say(ctx, Word::Pages, &[]);
                let (settings, models) = (say(ctx, Word::PageSettings, &[]), say(ctx, Word::PageModels, &[]));
                let connectors = say(ctx, Word::PageConnectors, &[]);
                let pages = [
                    Segment::new(Page::Settings, &settings),
                    Segment::new(Page::Models, &models),
                    Segment::new(Page::Connectors, &connectors),
                ];
                ui.add(Segmented::new(&label, &mut self.page, &pages).id_salt("settings.page"));
                ui.add_space(space::S8);
                match self.page {
                    Page::Settings => self.body(ui, runner.as_ref(), actor, now),
                    Page::Models => self.models.show(ui, runner, actor, now),
                    Page::Connectors => self.connectors.show(ui, runner, actor, now),
                }
            });
        let escaped = !ctx.text_edit_focused() && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape));
        open && !escaped
    }

    fn body(&mut self, ui: &mut Ui, runner: &dyn Runner, actor: &Actor, now: f64) {
        let ctx = ui.ctx().clone();
        let ds = ds(&ctx);
        let search = say(&ctx, Word::Search, &[]);
        ui.add(TextField::new(&search, &mut self.query).search().hint(&search).id_salt("settings.search"));
        ui.add_space(space::S8);
        if let Some(reason) = &self.refused {
            ui.label(RichText::new(say(&ctx, Word::NotSaved, &[("reason", reason)])).color(ds.theme.danger));
        }
        if self.rows.is_empty() {
            if let Some(reason) = &self.unreadable {
                let headline = say(&ctx, Word::Unavailable, &[]);
                let line = say(&ctx, Word::UnavailableLine, &[("reason", reason)]);
                EmptyState::new(icons::GEAR, &headline, &line).show(ui);
            }
            return;
        }
        let needle = self.query.trim().to_lowercase();
        if !self.rows.iter().any(|row| row.matches(&needle)) {
            let query = self.query.trim().to_owned();
            let headline = say(&ctx, Word::NoMatch, &[("query", &query)]);
            EmptyState::new(icons::MAGNIFYING_GLASS, &headline, &say(&ctx, Word::NoMatchLine, &[])).show(ui);
            return;
        }
        let mut edits = Vec::new();
        ScrollArea::vertical().id_salt("settings.scroll").auto_shrink([false, false]).show(ui, |ui| {
            let mut area: Option<String> = None;
            for (index, row) in self.rows.iter_mut().enumerate() {
                if !row.matches(&needle) {
                    continue;
                }
                if area.as_deref() != Some(row.area.as_str()) {
                    if area.is_some() {
                        ui.add_space(space::S12);
                    }
                    ui.label(RichText::new(area_title(&ctx, &row.area)).font(ds.text.section.clone()));
                    ui.add_space(space::S4);
                    area = Some(row.area.clone());
                }
                if draw_row(ui, row) {
                    edits.push(index);
                }
            }
        });
        for index in edits {
            self.commit(&ctx, runner, actor, index, now);
        }
    }

    /// Sends row `index`'s value: `settings.reset` when it is the default again, else `settings.set`; then reads the
    /// settings back, so the row shows what the store holds.
    fn commit(&mut self, ctx: &Context, runner: &dyn Runner, actor: &Actor, index: usize, now: f64) {
        let Some(row) = self.rows.get(index) else { return };
        let value = Value::from(&row.value).get(FIELD).cloned().unwrap_or(Value::Null);
        let result = if value == row.default {
            run(runner, actor, "settings.reset", json!({ "key": row.key }))
        } else {
            run(runner, actor, "settings.set", json!({ "key": row.key, "value": value }))
        };
        self.read(ctx, runner, actor, now);
        self.refused = result.err().map(|error| error.message);
    }

    /// Reads every setting and the newest changes, keeping each row's form when its key was already shown.
    fn read(&mut self, ctx: &Context, runner: &dyn Runner, actor: &Actor, now: f64) {
        self.read_at = Some(now);
        let listed = match run(runner, actor, "settings.list", json!({})) {
            Ok(answer) => answer,
            Err(error) => {
                self.unreadable = Some(error.message);
                self.rows.clear();
                return;
            }
        };
        self.unreadable = None;
        let changes = run(runner, actor, "settings.changes", json!({ "limit": CHANGES })).unwrap_or_default();
        let mut old: Vec<Row> = std::mem::take(&mut self.rows);
        let entries = listed.get("settings").and_then(Value::as_array).cloned().unwrap_or_default();
        for entry in &entries {
            let Some(key) = entry.get("key").and_then(Value::as_str).filter(|key| !OWN_PAGE.contains(key)) else {
                continue;
            };
            let current = entry.get("value").cloned().unwrap_or(Value::Null);
            let agent = last_agent(&changes, key);
            if let Some(at) = old.iter().position(|row| row.key == key) {
                let mut row = old.swap_remove(at);
                row.value = row.form.value_from_json(&json!({ FIELD: current }));
                row.agent = agent;
                self.rows.push(row);
            } else if let Some(row) = new_row(ctx, entry, key, &current, agent) {
                self.rows.push(row);
            }
        }
        self.rows.sort_by(|a, b| (a.area.as_str(), a.key.as_str()).cmp(&(b.area.as_str(), b.key.as_str())));
    }
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

/// The agent whose change to `key` is the newest in `changes` (`settings.changes`), when that change was an agent's.
fn last_agent(changes: &Value, key: &str) -> Option<String> {
    let newest = changes
        .get("changes")?
        .as_array()?
        .iter()
        .find(|change| change.pointer("/change/key").and_then(Value::as_str) == Some(key))?;
    let actor: Actor = serde_json::from_value(newest.get("actor")?.clone()).ok()?;
    match actor {
        Actor::Agent { id, .. } => Some(id),
        _ => None,
    }
}

/// The row of `key` from its `settings.list` entry and its registered schema; `None` for a key this build did not
/// register (the store lists only registered keys, so that is a key registered since the list was made).
fn new_row(ctx: &Context, entry: &Value, key: &str, current: &Value, agent: Option<String>) -> Option<Row> {
    let reg = registry::find(key)?;
    let area = entry.get("area").and_then(Value::as_str).unwrap_or(reg.meta.area()).to_owned();
    let segment = key.rsplit('.').next().unwrap_or(key);
    let label = title(ctx, &humanise(segment)).into_owned();
    let default = entry.get("default").cloned().unwrap_or_else(|| reg.default_json());
    let schema = row_schema(ctx, reg.schema(), &label, &default);
    let form = Form::from_schema(&schema);
    let value = form.value_from_json(&json!({ FIELD: current }));
    Some(Row {
        key: key.to_owned(),
        area,
        label,
        summary: title(ctx, reg.meta.summary).into_owned(),
        machine: entry.get("scope").and_then(Value::as_str) == Some("machine"),
        next_start: entry.get("apply").and_then(Value::as_str) == Some("next_start"),
        agent,
        default,
        form,
        value,
    })
}

/// A one-field object schema around a setting's value schema: the field titled `label`, defaulting to `default`, its
/// enum options named in words, and the value schema's definitions lifted to the root where `$ref`s find them.
fn row_schema(ctx: &Context, value: Schema, label: &str, default: &Value) -> Schema {
    let mut field = value.to_value();
    let mut defs = field.as_object_mut().and_then(|f| {
        f.remove("$schema");
        f.remove("$defs")
    });
    if let Some(object) = field.as_object_mut() {
        object.insert("title".to_owned(), json!(label));
        object.insert("default".to_owned(), default.clone());
    }
    name_in_words(ctx, &mut field);
    if let Some(defs) = defs.as_mut().and_then(Value::as_object_mut) {
        for def in defs.values_mut() {
            name_in_words(ctx, def);
        }
    }
    let mut root = Map::new();
    root.insert("type".to_owned(), json!("object"));
    root.insert("properties".to_owned(), json!({ FIELD: field }));
    if let Some(defs) = defs {
        root.insert("$defs".to_owned(), defs);
    }
    Schema::try_from(Value::Object(root)).unwrap_or_default()
}

/// Gives every field of `schema` (an object's properties, all the way down) a title in words where it has none, and
/// every option of an enum written as `const`s its value in words: `vmc_port` reads "Vmc port", `high-contrast` "High
/// contrast".
fn name_in_words(ctx: &Context, schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else { return };
    for key in ["oneOf", "anyOf"] {
        for arm in object.get_mut(key).and_then(Value::as_array_mut).into_iter().flatten() {
            let text = arm.get("const").and_then(Value::as_str).map(str::to_owned);
            match (text, arm.as_object_mut()) {
                (Some(text), Some(arm)) => {
                    arm.entry("title").or_insert_with(|| json!(title(ctx, &humanise(&text))));
                }
                _ => name_in_words(ctx, arm),
            }
        }
    }
    if let Some(properties) = object.get_mut("properties").and_then(Value::as_object_mut) {
        for (name, property) in properties.iter_mut() {
            if let Some(field) = property.as_object_mut() {
                field.entry("title").or_insert_with(|| json!(title(ctx, &humanise(name))));
            }
            name_in_words(ctx, property);
        }
    }
    if let Some(items) = object.get_mut("items") {
        name_in_words(ctx, items);
    }
}

/// Draws `row`; true when its value was committed this frame.
fn draw_row(ui: &mut Ui, row: &mut Row) -> bool {
    let ctx = ui.ctx().clone();
    let ds = ds(&ctx);
    let out = SchemaForm::new(&row.form, &mut row.value).id_salt(("settings.row", row.key.as_str())).show(ui);
    let committed = out.edits.iter().any(|edit| edit.phase == EditPhase::Commit);
    ui.label(RichText::new(row.summary.as_str()).font(ds.text.small.clone()).color(ds.theme.text2));
    if !row.next_start && !row.machine && row.agent.is_none() {
        ui.add_space(space::S8);
        return committed;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = space::S6;
        if row.next_start {
            chip(ui, &say(&ctx, Word::NextStart, &[]));
        }
        if row.machine {
            chip(ui, &say(&ctx, Word::Machine, &[]));
        }
        if let Some(agent) = &row.agent {
            ui.label(RichText::new(say(&ctx, Word::ChangedBy, &[])).font(ds.text.small.clone()).color(ds.theme.text2));
            ui.add(ActorChip::new(agent, ActorKind::Agent));
        }
    });
    ui.add_space(space::S8);
    committed
}

/// A quiet fact about a row, as a small framed word.
fn chip(ui: &mut Ui, text: &str) {
    let ds = ds(ui.ctx());
    egui::Frame::new()
        .stroke(egui::Stroke::new(1.0, ds.theme.line))
        .corner_radius(egui::CornerRadius::same(8))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| ui.label(RichText::new(text).font(ds.text.small.clone()).color(ds.theme.text2)));
}
