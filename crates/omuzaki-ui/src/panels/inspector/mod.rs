//! The inspector: the selected part's parameters as a form generated from its schema (`shell.md` section 7).
//!
//! The form comes from `part.describe` ([`params`]) through `ds::schema_form`, so every part kind has an inspector from
//! the day it is registered and the UI can't drift from what an agent sees (P8). It is read when the selection or the
//! document changes, never per frame, and not while a gesture is open (the gesture's own previews change the
//! document). Every edit is a `part.set_param`:
//!
//! - **One gesture is one transaction** ([`EditPhase`]): the first preview of a drag (or a colour being picked, or
//!   text being typed) opens one with `Runner::begin`, every preview runs in it, the gesture's end commits it (one
//!   undo step), and Esc rolls it back. A one-step edit (a click, a key, a reset) is its own transaction.
//! - **Several parts of one kind** selected are edited together: the edit runs on each, in one transaction. The form
//!   shows the primary part's values.
//!
//! Search keeps the fields whose path or description matches, with their sections; it rebuilds the form when the
//! query changes, not per frame.

pub mod params;

use egui::{RichText, ScrollArea, Ui};
use omuzaki_char_api::{CharId, PartId};
use omuzaki_ops_api::{OpError, TxId};
use schemars::Schema;
use serde_json::{Value, json};

use super::edit::EditFocus;
use super::io::{DocWatch, Io};
use super::text::{self, say};
use crate::ds::icons;
use crate::ds::schema_form::{EditPhase, FieldEdit, Form, FormValue, SchemaForm};
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{EmptyState, TextField};

/// The inspector's id, as the shell registers it.
pub const ID: &str = "inspector";
/// The search field's salt.
const SEARCH: &str = "panels.inspector.search";

/// What the inspector is showing.
#[derive(Debug)]
struct Shown {
    character: CharId,
    /// The primary part, whose values the form shows.
    part: PartId,
    /// Every selected part of the primary's kind: what an edit changes.
    targets: Vec<PartId>,
    /// The `part.describe` answer the form was made from.
    answer: Value,
    /// The header's two lines, made when the part was read.
    title: String,
    kind: String,
    form: Form,
    value: FormValue,
    /// How many fields the form has after the search.
    fields: usize,
    /// The query the form was made with.
    query: String,
}

/// The inspector panel.
#[derive(Debug)]
pub struct Inspector {
    focus: EditFocus,
    watch: DocWatch,
    /// The focus generation the shown part was read at.
    read_at: Option<u64>,
    shown: Option<Shown>,
    query: String,
    /// The transaction of the gesture in progress.
    gesture: Option<TxId>,
    problem: Option<String>,
}

impl Inspector {
    /// An inspector following `focus`.
    pub fn new(focus: EditFocus) -> Self {
        Self {
            focus,
            watch: DocWatch::default(),
            read_at: None,
            shown: None,
            query: String::new(),
            gesture: None,
            problem: None,
        }
    }

    /// Whether the panel can work: a character is open (with nothing selected it says how to select).
    pub fn available(&self) -> bool {
        self.focus.character().is_some()
    }

    /// The fields the form shows now (after the search).
    pub fn fields(&self) -> usize {
        self.shown.as_ref().map_or(0, |s| s.fields)
    }

    /// The form's current value, as JSON in the parameters' shape (lists keyed by index).
    pub fn value(&self) -> Option<Value> {
        self.shown.as_ref().map(|s| Value::from(&s.value))
    }

    /// Draws the inspector for one frame.
    pub fn ui(&mut self, ui: &mut Ui, io: Io<'_>) {
        let ds = ds(ui.ctx());
        self.refresh(io, &ds);
        let search_focus = self.focus.take_inspector_request();
        let Some(shown) = self.shown.as_mut() else {
            let (headline, line) = match self.focus.count() {
                0 => (text::SELECT_A_PART, text::SELECT_HOW),
                _ => (text::UNDESCRIBED, text::SELECT_HOW),
            };
            let reason = self.problem.as_deref().unwrap_or_default();
            EmptyState::new(
                icons::SLIDERS_HORIZONTAL,
                &say(&ds, headline, &[("reason", reason)]),
                &say(&ds, line, &[]),
            )
            .show(ui);
            return;
        };
        ui.label(RichText::new(shown.title.as_str()).font(ds.text.title.clone()));
        ui.label(RichText::new(shown.kind.as_str()).color(ds.theme.text2));
        let label = say(&ds, text::SEARCH_SETTINGS, &[]);
        let search = ui.add(TextField::new(&label, &mut self.query).search().id_salt(SEARCH));
        if search_focus {
            search.request_focus();
        }
        if shown.query != self.query && self.gesture.is_none() {
            rebuild(shown, &self.query);
        }
        if let Some(problem) = &self.problem {
            ui.label(RichText::new(problem.as_str()).color(ds.theme.danger));
        }
        if shown.fields == 0 && !shown.query.is_empty() {
            ui.label(say(&ds, text::NO_SETTING, &[("query", &shown.query)]).as_ref());
            return;
        }
        let out = ScrollArea::vertical()
            .id_salt("panels.inspector.form")
            .auto_shrink([false, false])
            .show(ui, |ui| SchemaForm::new(&shown.form, &mut shown.value).id_salt((ID, shown.part.as_str())).show(ui))
            .inner;
        if let Some(edit) = out.edit {
            self.edit(io, edit, &ds);
        }
    }

    /// Reads the selected part again when the selection or the document changed (never during a gesture).
    fn refresh(&mut self, io: Io<'_>, ds: &Ds) {
        let stale = self.watch.stale(io.events);
        let generation = self.focus.generation();
        if self.gesture.is_some() || (!stale && self.read_at == Some(generation)) {
            return;
        }
        self.read_at = Some(generation);
        self.watch.built();
        let (Some(character), Some(part)) = (self.focus.character(), self.focus.primary()) else {
            self.shown = None;
            return;
        };
        match describe(io, &character, &part) {
            Ok(answer) => {
                let targets = same_kind(io, &character, &part, &self.focus.selected(), &answer);
                let count = targets.len().to_string();
                let title = if targets.len() > 1 {
                    say(ds, text::MANY_SELECTED, &[("count", &count)]).into_owned()
                } else {
                    part.to_string()
                };
                let kind_id = answer.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned();
                let kind = say(ds, text::KIND, &[("kind", &kind_id)]).into_owned();
                let mut shown = Shown {
                    character,
                    part,
                    targets,
                    answer,
                    title,
                    kind,
                    form: Form::from_schema(&Schema::default()),
                    value: FormValue::Absent,
                    fields: 0,
                    query: String::new(),
                };
                rebuild(&mut shown, &self.query);
                self.shown = Some(shown);
            }
            Err(e) => {
                self.shown = None;
                self.problem = Some(e.message);
            }
        }
    }

    /// Runs an edit the form reported, as one gesture's part.
    fn edit(&mut self, io: Io<'_>, edit: FieldEdit, ds: &Ds) {
        let Some(shown) = self.shown.as_ref() else { return };
        let (Some(pointer), Some(value)) =
            (shown.form.pointer(edit.field), shown.form.field_value(&shown.value, edit.field))
        else {
            return;
        };
        let path = pointer.trim_start_matches('/').to_owned();
        let value = Value::from(value);
        let result = match edit.phase {
            EditPhase::Preview => {
                let tx = *self
                    .gesture
                    .get_or_insert_with(|| io.runner.begin(&say(ds, text::SET, &[("name", &path)]), io.actor));
                set(io, shown, &path, &value, Some(tx))
            }
            EditPhase::Commit => self.commit(io, &path, &value, ds),
            EditPhase::Cancel => self.gesture.take().map_or(Ok(()), |tx| io.runner.rollback(tx, io.actor)),
        };
        if edit.phase != EditPhase::Preview {
            self.watch.force();
        }
        match result {
            Ok(()) => self.problem = None,
            Err(e) => {
                if let Some(tx) = self.gesture.take() {
                    // The gesture can't go on: put the document back as it was before it. The edit's own error is
                    // the one worth reporting, so a failed rollback is not.
                    let _ = io.runner.rollback(tx, io.actor);
                }
                self.problem = Some(say(ds, text::REFUSED, &[("reason", &e.message)]).into_owned());
                self.watch.force();
            }
        }
    }

    /// Ends a gesture (or makes a one-step edit): the last value, then the commit. Several targets share one
    /// transaction; a value already in place is not sent again.
    fn commit(&mut self, io: Io<'_>, path: &str, value: &Value, ds: &Ds) -> Result<(), OpError> {
        let Some(shown) = self.shown.as_ref() else { return Ok(()) };
        let tx = match (self.gesture.take(), shown.targets.len()) {
            (Some(tx), _) => Some(tx),
            (None, n) if n > 1 => Some(io.runner.begin(&say(ds, text::SET, &[("name", path)]), io.actor)),
            (None, _) if current(&shown.answer, path) == Some(value) => return Ok(()),
            (None, _) => None,
        };
        let sent = set(io, shown, path, value, tx);
        match (tx, sent) {
            (Some(tx), Ok(())) => io.runner.commit(tx, io.actor),
            (Some(tx), Err(e)) => {
                let _ = io.runner.rollback(tx, io.actor);
                Err(e)
            }
            (None, sent) => sent,
        }
    }
}

/// Makes the form from the part's answer, keeping what matches `query`.
fn rebuild(shown: &mut Shown, query: &str) {
    let read = params::read(&shown.answer, query);
    shown.form =
        Schema::try_from(read.schema).map_or_else(|_| Form::from_schema(&Schema::default()), |s| Form::from_schema(&s));
    shown.value = shown.form.value_from_json(&read.value);
    shown.fields = read.fields;
    shown.query.clear();
    shown.query.push_str(query);
}

/// `part.describe` of `part`.
fn describe(io: Io<'_>, character: &CharId, part: &PartId) -> Result<Value, OpError> {
    io.ask("part.describe", json!({"character": character, "part": part}))
}

/// The selected parts that share `part`'s kind, `part` last; just `part` when the others can't be read.
fn same_kind(io: Io<'_>, character: &CharId, part: &PartId, selected: &[PartId], answer: &Value) -> Vec<PartId> {
    let kind = answer.get("kind").and_then(Value::as_str);
    let def = io.character(character).ok().flatten();
    let mut targets: Vec<PartId> = selected
        .iter()
        .filter(|p| *p != part)
        .filter(|p| def.as_ref().and_then(|d| d.parts.get(*p)).is_some_and(|d| Some(d.kind.as_str()) == kind))
        .cloned()
        .collect();
    targets.push(part.clone());
    targets
}

/// What `part.describe` said the parameter at `path` holds now.
fn current<'a>(answer: &'a Value, path: &str) -> Option<&'a Value> {
    answer
        .get("parameters")?
        .as_array()?
        .iter()
        .find(|p| p.get("path").and_then(Value::as_str) == Some(path))?
        .get("current")
}

/// Sets `path` to `value` on every target part.
fn set(io: Io<'_>, shown: &Shown, path: &str, value: &Value, tx: Option<TxId>) -> Result<(), OpError> {
    for part in &shown.targets {
        let args = json!({"character": shown.character, "part": part, "path": path, "value": value});
        io.run("part.set_param", args, tx)?;
    }
    Ok(())
}

/// The inspector as the shell shows it, over the shell's [`EditFocus`].
struct InspectorPanel(Inspector);

impl omuzaki_ui_api::panel::Panel for InspectorPanel {
    fn ui(&mut self, ui: &mut Ui, ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) {
        self.0.ui(ui, Io::from_ctx(ctx));
    }

    fn available(&self, _ctx: &omuzaki_ui_api::panel::PanelCtx<'_>) -> bool {
        self.0.available()
    }
}

omuzaki_ui_api::register_panel! {
    id: "inspector", title: "Inspector", icon: icons::SLIDERS_HORIZONTAL,
    region: omuzaki_ui_api::registry::Region::Right, min_size: [280.0, 200.0], order: 10,
    empty: omuzaki_ui_api::panel::EmptyState::new(text::NO_CHARACTER, text::OPEN_CHARACTER),
    build: |ctx| Box::new(InspectorPanel(Inspector::new(EditFocus::from_ctx(ctx)))),
}
