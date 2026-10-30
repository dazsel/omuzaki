//! `overlay.*`: the on-stream widgets as ops (spec v3 BR-13, BR-20).
//!
//! - **Edits** (`overlay.add`, `set`, `remove`, `show`, `hide`) change the document under `overlay/widgets/<id>`:
//!   saved with the project, undoable, and the same for a person, a script and an agent.
//! - **Live actions** (`overlay.alert`, `overlay.timer`, `overlay.goal`) move a widget on air now. Like `twitch.connect`
//!   they are `broadcast` scope with no undo: they change what viewers see, not the document. An automation fires
//!   them with its `op.run` node, so a redeem can start a timer or a raid can put up a custom alert.
//! - **Queries** (`overlay.kinds`, `overlay.list`) need `read` only.

use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{merge, put, service};
use crate::live::Alert;
use crate::model::{AlertKind, Content, Kind, MAX_TEXT, MAX_WIDGETS, WidgetDef, WidgetId, paths};
use crate::service::TimerAction;

/// List the kinds of on-stream widget and what each does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Kinds {}

/// List the overlay's widgets with what each shows now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct List {}

/// Add an on-stream widget.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "id": "alerts", "kind": "alerts" },
    { "id": "chat", "kind": "chat", "settings": { "lines": 6 }, "layout": { "anchor": "bottom_left", "width": 0.25 } },
    { "id": "subathon", "kind": "timer", "settings": { "mode": "subathon", "seconds": 3600, "label": "Subathon" } },
    { "id": "goal", "kind": "goal", "settings": { "title": "Road to 100 subs", "metric": "subs", "target": 100 } },
    { "id": "brb", "kind": "text", "settings": { "text": "Be right back" }, "scenes": ["brb"] }
]))]
pub struct Add {
    /// The new widget's id: lowercase letters, digits, `-` and `_`.
    pub id: WidgetId,
    /// What it is: `alerts`, `chat`, `goal`, `timer` or `text` (`overlay.kinds`).
    pub kind: Kind,
    /// The kind's own settings (alerts: `events`, `seconds`, `templates`; chat: `lines`, `seconds`, `hide_commands`;
    /// goal: `title`, `metric`, `target`, `start`; timer: `mode`, `seconds`, `label`, `add`; text: `text`,
    /// `subtitle`). Anything left out takes its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<Value>,
    /// Where it sits: `anchor` (`top_left` .. `bottom_right`), `x`, `y`, `width`, `scale`. Default: where its kind
    /// usually goes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout: Option<Value>,
    /// Its colours: `accent`, `text`, `panel` (`#rrggbb`) and `panel_opacity`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<Value>,
    /// The program scenes it shows in; leave out for every scene.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scenes: Vec<String>,
}

/// Change a widget: any of its fields, merged into what it has (`null` puts a field back to its default).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "id": "goal", "changes": { "target": 150, "title": "Road to 150" } },
    { "id": "chat", "changes": { "layout": { "anchor": "bottom_right" }, "style": { "accent": "#7ad7ff" } } }
]))]
pub struct Set {
    /// The widget.
    pub id: WidgetId,
    /// The fields to change, as they appear in `overlay.list`'s widget definitions.
    pub changes: Value,
}

/// Remove a widget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "brb" }]))]
pub struct Remove {
    /// The widget.
    pub id: WidgetId,
}

/// Show a hidden widget.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "brb" }]))]
pub struct Show {
    /// The widget.
    pub id: WidgetId,
}

/// Hide a widget without removing it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "brb" }]))]
pub struct Hide {
    /// The widget.
    pub id: WidgetId,
}

/// Put an alert on stream now: a test, or one an automation makes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "title": "Thanks for 1,000 followers!" },
    { "title": "Hydrate!", "message": "chat says drink some water", "widget": "alerts" }
]))]
pub struct AlertNow {
    /// The alert's title.
    pub title: String,
    /// A line under it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub message: String,
    /// The alerts widget to show it on; default the first one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub widget: Option<WidgetId>,
}

/// Start, pause, reset or change a timer on stream.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    { "id": "subathon", "action": "start" },
    { "id": "subathon", "action": "add", "seconds": 300 }
]))]
pub struct Timer {
    /// The timer widget.
    pub id: WidgetId,
    /// `start`, `pause`, `reset`, `add` or `set`.
    pub action: TimerAction,
    /// For `add` and `set`: seconds.
    #[serde(default)]
    pub seconds: f64,
}

/// Move a goal bar by hand.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "goal", "value": 42 }, { "id": "goal", "value": 1, "add": true }]))]
pub struct Goal {
    /// The goal widget.
    pub id: WidgetId,
    /// The new value, or the amount to add.
    pub value: f64,
    /// Add `value` instead of setting it.
    #[serde(default)]
    pub add: bool,
}

fn edit(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::EditScene, undo: Undo::Auto }
}

fn live(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Broadcast, undo: Undo::None }
}

fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

/// The widget `id` as the document holds it.
fn current(ctx: &OpCtx<'_>, id: &WidgetId) -> Result<Value, OpError> {
    ctx.doc.get(&paths::widget(id)).cloned().ok_or_else(|| {
        OpError::not_found(format!("there is no widget '{id}'")).hint("overlay.list lists the overlay's widgets")
    })
}

/// Parses and checks a widget definition, as an op error naming the field.
fn checked(value: Value) -> Result<WidgetDef, OpError> {
    let def: WidgetDef = serde_json::from_value(value).map_err(|e| OpError::invalid("settings", e.to_string()))?;
    def.check().map_err(|(field, why)| OpError::invalid(field, why))?;
    Ok(def)
}

/// The patch that stores `def` as widget `id`.
fn store(ctx: &OpCtx<'_>, id: &WidgetId, def: &WidgetDef) -> Result<Patch, OpError> {
    let value = serde_json::to_value(def).map_err(|e| OpError::internal(e.to_string()))?;
    Ok(put(ctx.doc, &paths::widget(id), value))
}

impl Op for Kinds {
    fn meta() -> OpMeta {
        query(
            "overlay.kinds",
            "List the kinds of on-stream widget (alerts, chat, goal, timer, text) and their settings.",
        )
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kinds: Vec<Value> = Kind::ALL
            .iter()
            .map(|k| {
                let defaults = serde_json::to_value(WidgetDef::new(Content::of(*k))).unwrap_or(Value::Null);
                json!({ "kind": k.name(), "summary": k.summary(), "defaults": defaults })
            })
            .collect();
        Ok(Plan::answer(json!({ "kinds": kinds, "alert_events": AlertKind::names() })))
    }
}

impl Op for List {
    fn meta() -> OpMeta {
        query("overlay.list", "List the overlay's widgets: each one's definition and what it shows now.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let defs = ctx.doc.get(&paths::widgets()).cloned().unwrap_or_else(|| json!({}));
        let live = ctx.service::<crate::service::ProgramService>().ok().map(crate::service::ProgramService::widgets);
        Ok(Plan::answer(json!({ "definitions": defs, "live": live })))
    }
}

impl Op for Add {
    fn meta() -> OpMeta {
        edit("overlay.add", "Add an on-stream widget: alerts, a chat box, a goal bar, a timer or a line of text.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&paths::widget(&self.id)).is_some() {
            return Err(OpError::conflict(format!("there is already a widget '{}'", self.id))
                .hint("overlay.set changes it; overlay.remove removes it"));
        }
        let count = ctx.doc.get(&paths::widgets()).and_then(Value::as_object).map_or(0, serde_json::Map::len);
        if count >= MAX_WIDGETS {
            return Err(OpError::invalid("id", format!("the overlay holds at most {MAX_WIDGETS} widgets")));
        }
        let mut value = serde_json::to_value(WidgetDef::new(Content::of(self.kind)))
            .map_err(|e| OpError::internal(e.to_string()))?;
        if let Some(settings) = &self.settings {
            if !settings.is_object() || settings.get("kind").is_some() {
                return Err(OpError::invalid("settings", "an object of the kind's own settings, without `kind`"));
            }
            merge(&mut value, settings);
        }
        for (field, patch) in [("layout", &self.layout), ("style", &self.style)] {
            if let Some(patch) = patch {
                merge(&mut value[field], patch);
            }
        }
        if !self.scenes.is_empty() {
            value["scenes"] = json!(self.scenes);
        }
        let def = checked(value)?;
        let patch = store(ctx, &self.id, &def)?;
        Ok(Plan {
            patch,
            output: Some(json!({ "added": self.id.as_str(), "kind": self.kind.name() })),
            ..Plan::default()
        })
    }
}

impl Op for Set {
    fn meta() -> OpMeta {
        edit("overlay.set", "Change an on-stream widget's settings, layout, style or scenes.")
    }

    fn coalesces() -> bool {
        true
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !self.changes.is_object() {
            return Err(OpError::invalid("changes", "an object of the fields to change"));
        }
        let mut value = current(ctx, &self.id)?;
        let kind = value.get("kind").cloned();
        if self.changes.get("kind").is_some_and(|k| Some(k) != kind.as_ref()) {
            return Err(OpError::invalid("changes.kind", "a widget keeps its kind: remove it and add another"));
        }
        merge(&mut value, &self.changes);
        let def = checked(value)?;
        let patch = store(ctx, &self.id, &def)?;
        Ok(Plan { patch, output: Some(json!({ "changed": self.id.as_str() })), ..Plan::default() })
    }
}

impl Op for Remove {
    fn meta() -> OpMeta {
        edit("overlay.remove", "Remove an on-stream widget.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let old = current(ctx, &self.id)?;
        let patch = Patch::new().with(paths::widget(&self.id), Some(old), None);
        Ok(Plan { patch, output: Some(json!({ "removed": self.id.as_str() })), ..Plan::default() })
    }
}

fn visibility(ctx: &OpCtx<'_>, id: &WidgetId, visible: bool) -> Result<Plan, OpError> {
    let mut value = current(ctx, id)?;
    value["visible"] = json!(visible);
    let def = checked(value)?;
    let patch = store(ctx, id, &def)?;
    Ok(Plan { patch, output: Some(json!({ "id": id.as_str(), "visible": visible })), ..Plan::default() })
}

impl Op for Show {
    fn meta() -> OpMeta {
        edit("overlay.show", "Show a hidden on-stream widget.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        visibility(ctx, &self.id, true)
    }
}

impl Op for Hide {
    fn meta() -> OpMeta {
        edit("overlay.hide", "Hide an on-stream widget without removing it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        visibility(ctx, &self.id, false)
    }
}

impl Op for AlertNow {
    fn meta() -> OpMeta {
        live("overlay.alert", "Put an alert on stream now, with a title and an optional line under it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.title.trim().is_empty() || self.title.len() > MAX_TEXT || self.message.len() > MAX_TEXT {
            return Err(OpError::invalid(
                "title",
                format!("a title of 1 to {MAX_TEXT} bytes, a message of at most {MAX_TEXT}"),
            ));
        }
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "title": self.title })));
        }
        let mut alert = Alert::new(AlertKind::Custom, "");
        alert.title.clone_from(&self.title);
        alert.message.clone_from(&self.message);
        let on = service
            .alert(self.widget.as_ref(), alert)
            .map_err(|why| OpError::not_found(why).hint("overlay.add with kind alerts makes one"))?;
        Ok(Plan::answer(json!({ "queued_on": on.as_str() })))
    }
}

impl Op for Timer {
    fn meta() -> OpMeta {
        live("overlay.timer", "Start, pause, reset, add time to or set a timer on stream.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !(self.seconds.is_finite() && self.seconds.abs() <= 864_000.0) {
            return Err(OpError::invalid("seconds", "at most ten days either way"));
        }
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false })));
        }
        let state = service.timer(&self.id, self.action, self.seconds).map_err(OpError::not_found)?;
        Ok(Plan::answer(json!({ "id": self.id.as_str(), "timer": state })))
    }
}

impl Op for Goal {
    fn meta() -> OpMeta {
        live("overlay.goal", "Set a goal bar's value on stream, or add to it.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if !self.value.is_finite() {
            return Err(OpError::invalid("value", "a number"));
        }
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false })));
        }
        let value = service.goal(&self.id, self.value, self.add).map_err(OpError::not_found)?;
        Ok(Plan::answer(json!({ "id": self.id.as_str(), "value": value })))
    }
}

register_op!(Kinds);
register_op!(List);
register_op!(Add);
register_op!(Set);
register_op!(Remove);
register_op!(Show);
register_op!(Hide);
register_op!(AlertNow);
register_op!(Timer);
register_op!(Goal);
