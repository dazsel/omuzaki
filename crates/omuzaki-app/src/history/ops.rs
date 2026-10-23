//! The history ops: list the project's transactions, compare any two states, restore one as an undoable step, and
//! name one as a version.

use std::sync::Arc;

use omuzaki_doc_api::diff::diff;
use omuzaki_doc_api::{DocPath, Patch, Value};
use omuzaki_history_api::state::{HistoryError, HistoryView, StateRef};
use omuzaki_history_api::{HistoryLog, HistoryQuery, SnapshotError, Snapshots, Trigger, TxSummary};
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, TxId, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::units::{is_bookkeeping, unit_of};

fn history_error(error: HistoryError) -> OpError {
    match error {
        HistoryError::UnknownTx(tx) => OpError::not_found(format!("no committed transaction {} in this project", tx.0))
            .hint("list them with history.list; a rolled back or still open transaction has no state of its own"),
        other => OpError::conflict(other.to_string()),
    }
}

fn snapshot_error(error: SnapshotError) -> OpError {
    match error {
        SnapshotError::Invalid(reason) => OpError::invalid("at", reason),
        other => OpError::conflict(other.to_string()),
    }
}

fn actor_label(actor: &Actor) -> String {
    match actor {
        Actor::Person { name } => format!("person {name}"),
        Actor::Agent { id, .. } => format!("agent {id}"),
        Actor::Plugin { id } => format!("plugin {id}"),
        Actor::System => "Omuzaki".to_owned(),
    }
}

fn summary_json(item: &TxSummary) -> Value {
    json!({
        "tx": item.tx.0,
        "label": item.label,
        "actor": actor_label(&item.actor),
        "by": item.actor,
        "cause": item.cause,
        "time": item.time.map(|t| t.0),
        "ops": item.ops,
        "op_names": item.op_names,
        "outcome": item.outcome,
        "names": item.names,
        "session": item.session,
    })
}

/// The document at `at`: the op's own for `now`, the history view's otherwise.
fn tree_at(ctx: &OpCtx<'_>, at: &StateRef) -> Result<Arc<Value>, OpError> {
    if *at == StateRef::Now {
        return Ok(Arc::new(ctx.doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null)));
    }
    ctx.service::<dyn HistoryView>()?.tree(at).map_err(history_error)
}

/// The changes from `from` to `to` that a person would read (bookkeeping left out), limited to `scope`.
fn changes(from: &Value, to: &Value, scope: &[DocPath]) -> Patch {
    let mut patch = diff(from, to);
    patch.changes.retain(|change| !is_bookkeeping(&change.path));
    if !scope.is_empty() {
        patch.changes.retain(|change| scope.iter().any(|root| change.path.starts_with(root)));
    }
    patch
}

/// `patch` grouped by unit, units in key order and changes in path order inside each.
fn grouped(patch: &Patch, trees: &[&Value]) -> Value {
    let mut units: std::collections::BTreeMap<String, (super::units::Unit, Vec<Value>)> =
        std::collections::BTreeMap::new();
    for change in &patch.changes {
        let unit = unit_of(&change.path, trees);
        let entry = units.entry(unit.key.clone()).or_insert_with(|| (unit, Vec::new()));
        entry.1.push(json!({"path": change.path.to_string(), "before": change.before, "after": change.after}));
    }
    Value::Array(
        units
            .into_values()
            .map(
                |(unit, changes)| json!({"unit": unit.key, "title": unit.title, "kind": unit.kind, "changes": changes}),
            )
            .collect(),
    )
}

fn parse_scope(scope: &[String]) -> Result<Vec<DocPath>, OpError> {
    scope
        .iter()
        .map(|p| DocPath::parse(p).map_err(|e| OpError::invalid("scope", format!("{}: {}", e.path, e.reason))))
        .collect()
}

/// Lists the project's transactions across every session, newest first: who made each, when, its ops, whether it is
/// in effect, undone or rolled back, and the versions named after it. Page with `before`.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "limit": 20 }, { "limit": 20, "before": 41 }]))]
pub struct HistoryList {
    /// At most this many (1 to 500; default 50).
    #[serde(default)]
    pub limit: Option<usize>,
    /// Only transactions older than this one: the `next` of the page before.
    #[serde(default)]
    pub before: Option<u64>,
    /// Only those this actor made, e.g. `{"kind": "person", "name": "You"}`.
    #[serde(default)]
    pub actor: Option<Actor>,
    /// Include rolled back transactions, which never took effect.
    #[serde(default)]
    pub rolled_back: bool,
}

impl Op for HistoryList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "history.list",
            summary: "List the project's changes across every session, newest first, with who made each and when.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut query = HistoryQuery::default();
        query.limit = self.limit.unwrap_or(0);
        query.before = self.before.map(TxId);
        query.actor.clone_from(&self.actor);
        query.rolled_back = self.rolled_back;
        let page = ctx.service::<dyn HistoryLog>()?.list(&query).map_err(history_error)?;
        let items: Vec<Value> = page.items.iter().map(summary_json).collect();
        Ok(Plan::answer(json!({ "transactions": items, "next": page.next.map(|t| t.0), "total": page.total })))
    }
}
register_op!(HistoryList);

/// Compares two states of the project (a transaction `{"tx": 7}`, a snapshot, or `"now"`) and lists what differs,
/// grouped by unit (a part, the face, the palette, a scene) with each value before and after. Changes nothing.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "from": { "tx": 7 } }, { "from": { "tx": 7 }, "to": { "tx": 9 }, "scope": ["characters/kit/face"] }]))]
pub struct HistoryDiff {
    /// The earlier state.
    pub from: StateRef,
    /// The later state; `"now"` by default.
    #[serde(default = "now")]
    pub to: StateRef,
    /// Only changes under these paths.
    #[serde(default)]
    pub scope: Vec<String>,
}

fn now() -> StateRef {
    StateRef::Now
}

impl Op for HistoryDiff {
    fn meta() -> OpMeta {
        OpMeta {
            name: "history.diff",
            summary: "Compare two states of the project and list what differs, grouped by part, face, palette or scene.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let scope = parse_scope(&self.scope)?;
        let (from, to) = (tree_at(ctx, &self.from)?, tree_at(ctx, &self.to)?);
        let patch = changes(&from, &to, &scope);
        Ok(Plan::answer(json!({
            "from": self.from,
            "to": self.to,
            "changes": patch.changes.len(),
            "units": grouped(&patch, &[&from, &to]),
        })))
    }
}
register_op!(HistoryDiff);

/// Puts the project back the way it was just after a transaction (`{"tx": 7}`) or in a snapshot, as one new step:
/// nothing in between is lost, and undo returns to now.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "to": { "tx": 7 } }]))]
pub struct HistoryRestore {
    /// The state to go back to.
    pub to: StateRef,
}

impl Op for HistoryRestore {
    fn meta() -> OpMeta {
        OpMeta {
            name: "history.restore",
            summary: "Put the project back as it was after an earlier change, as one undoable step.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let now = tree_at(ctx, &StateRef::Now)?;
        let target = tree_at(ctx, &self.to)?;
        let patch = changes(&now, &target, &[]);
        let outside = patch.changes.iter().any(|c| c.path.segments().first().is_none_or(|s| s != "characters"));
        if outside {
            ctx.require_scope(Scope::EditScene)?;
        }
        let label = match &self.to {
            StateRef::Tx { tx, .. } => {
                ctx.service::<dyn HistoryLog>().ok().and_then(|log| log.get(*tx).ok()).map_or_else(
                    || format!("transaction {}", tx.0),
                    |item| format!("transaction {} '{}'", tx.0, item.label),
                )
            }
            other => serde_json::to_string(other).unwrap_or_default(),
        };
        let output = json!({
            "restored_to": label,
            "changes": patch.changes.len(),
            "units": grouped(&patch, &[&now, &target]),
        });
        Ok(Plan { output: Some(output), ..Plan::edit(patch) })
    }
}
register_op!(HistoryRestore);

/// Names a state of the project as a version (`"now"` by default, or a transaction `{"tx": 7}`), kept until it is
/// deleted: retention never removes a named version.
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "name": "before the hair pass" }, { "name": "first look", "at": { "tx": 7 } }]))]
pub struct HistoryName {
    /// The version's name, unique in the project.
    pub name: String,
    /// The state to name; `"now"` by default.
    #[serde(default = "now")]
    pub at: StateRef,
}

impl Op for HistoryName {
    fn meta() -> OpMeta {
        OpMeta {
            name: "history.name",
            summary: "Name the project's current state, or an earlier one, as a version kept until you delete it.",
            kind: OpKind::Edit,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let name = self.name.trim();
        if name.is_empty() || name.chars().count() > 120 {
            return Err(OpError::invalid("name", "a version's name is 1 to 120 characters"));
        }
        let snapshots = ctx.service::<dyn Snapshots>()?;
        let taken = snapshots.list().map_err(snapshot_error)?;
        if taken.iter().any(|info| info.trigger == Trigger::Named && info.name.as_deref() == Some(name)) {
            return Err(OpError::conflict(format!("a version is already named '{name}'")).hint("pick another name"));
        }
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "name": name, "at": self.at, "named": false })));
        }
        let id = snapshots.take_at(&self.at, Trigger::Named, Some(name)).map_err(snapshot_error)?;
        Ok(Plan::answer(json!({ "name": name, "at": self.at, "named": true, "snapshot": id.to_string() })))
    }
}
register_op!(HistoryName);
