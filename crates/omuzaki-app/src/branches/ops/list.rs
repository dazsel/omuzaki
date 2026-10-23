//! `branch.list` and `branch.status` (`docs/design/branches.md` 6.2, 6.4 and 7.3): the branches with what each changed
//! since its base with the folder branch, and one branch's standing against the folder ("3 changed here, 5 on
//! main").

use std::collections::BTreeSet;
use std::sync::Arc;

use omuzaki_doc_api::diff::diff;
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_history_api::branch::{BranchId, BranchInfo, BranchName, LaneState, Point, PointId};
use omuzaki_history_api::merge_base::merge_base;
use omuzaki_history_api::state::BranchStore;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::branch_error;
use crate::history::units::{is_bookkeeping, unit_of};

/// The folder branch among `rows`: the one whose lane holds the folder, else `main`.
fn folder_of(rows: &[BranchInfo]) -> Option<&BranchInfo> {
    rows.iter()
        .find(|b| matches!(b.lane, LaneState::Open { folder: true, .. }))
        .or_else(|| rows.iter().find(|b| b.id.is_main()))
}

/// The base of `point` with the folder branch's head (design 7.3, with the folder as ours).
fn base_with(store: &dyn BranchStore, folder: &BranchInfo, point: &Point) -> Option<PointId> {
    let head = store.point(&folder.head).ok()?;
    let read = |id: &PointId| store.point(id).ok();
    merge_base(&folder.id, &head.contains.join(&folder.inherits), point, &read)
}

/// The titles of the units that differ from `from` to `to`, bookkeeping left out, in key order.
fn changed_units(from: &Value, to: &Value) -> Vec<String> {
    let patch = diff(from, to);
    let units: BTreeSet<(String, String)> = patch
        .changes
        .iter()
        .filter(|c| !is_bookkeeping(&c.path))
        .map(|c| unit_of(&c.path, &[from, to]))
        .map(|u| (u.key, u.title))
        .collect();
    units.into_iter().map(|(_, title)| title).collect()
}

fn tree(store: &dyn BranchStore, id: &PointId) -> Result<Arc<Value>, OpError> {
    store.tree(id).map_err(branch_error)
}

fn row(info: &BranchInfo, at: Option<u64>, changed: &[String]) -> Value {
    json!({
        "id": info.id,
        "name": info.name,
        "title": info.title,
        "head": info.head.to_string(),
        "seq": info.seq,
        "auto": info.auto,
        "deleted": info.deleted.map(|t| t.0),
        "lane": info.lane,
        "review": info.review,
        "owner_session": info.owner_session,
        "focus": info.focus,
        "last_change": at,
        "changed": changed,
    })
}

/// Lists the project's branches: the folder branch first, then by last change, each with the parts, face, palette or
/// scenes it changed since its base with the folder branch. Automatic branches (kept by restores) and deleted ones
/// are left out unless asked for.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "include_auto": true, "include_bin": true }]))]
pub struct BranchList {
    /// Include the automatic branches restores keep.
    #[serde(default)]
    pub include_auto: bool,
    /// Include branches in the bin.
    #[serde(default)]
    pub include_bin: bool,
}

impl Op for BranchList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "branch.list",
            summary: "List the project's branches, with what each changed since it left the folder branch.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let store = ctx.service::<dyn BranchStore>()?;
        let all = store.list().map_err(branch_error)?;
        let Some(folder) = folder_of(&all).cloned() else {
            return Ok(Plan::answer(json!({
                "branches": [],
                "folder": "main",
                "note": "the project is one branch, main, until something branches",
            })));
        };
        let folder_tree = tree(store, &folder.head)?;
        let mut rows: Vec<(bool, Option<u64>, Value)> = Vec::new();
        for info in all.iter().filter(|b| (self.include_auto || !b.auto) && (self.include_bin || b.deleted.is_none())) {
            let point = store.point(&info.head).ok();
            let at = point.as_ref().map(|p| p.at.0);
            let changed = match (&point, info.id == folder.id) {
                (Some(point), false) => {
                    let base = match base_with(store, &folder, point) {
                        Some(base) => tree(store, &base)?,
                        None => Arc::clone(&folder_tree),
                    };
                    changed_units(&base, &*tree(store, &info.head)?)
                }
                _ => Vec::new(),
            };
            rows.push((info.id == folder.id, at, row(info, at, &changed)));
        }
        rows.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
        Ok(Plan::answer(json!({
            "branches": rows.into_iter().map(|(_, _, row)| row).collect::<Vec<_>>(),
            "folder": folder.name,
        })))
    }
}
register_op!(BranchList);

/// How a branch stands against the folder branch: its head, their base, and the units changed on each side since
/// ("3 changed here, 5 on main"), with its review. This lane's branch by default.
#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "branch": "aria/oval" }]))]
pub struct BranchStatus {
    /// The branch; this lane's own by default.
    #[serde(default)]
    pub branch: Option<BranchName>,
}

impl Op for BranchStatus {
    fn meta() -> OpMeta {
        OpMeta {
            name: "branch.status",
            summary: "Show how a branch stands against the folder branch: what changed on each side since they parted.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let store = ctx.service::<dyn BranchStore>()?;
        let all = store.list().map_err(branch_error)?;
        let Some(folder) = folder_of(&all).cloned() else {
            return Ok(Plan::answer(json!({
                "branch": "main",
                "folder": true,
                "summary": "the project is one branch, main, until something branches",
            })));
        };
        let this = store.this().ok();
        let target = match &self.branch {
            Some(name) => store.branch(name).map_err(branch_error)?,
            None => this.clone().unwrap_or_else(|| folder.clone()),
        };
        let live = ctx.doc.get(&DocPath::root()).cloned().unwrap_or(Value::Null);
        let is_this = |id: &BranchId| this.as_ref().is_some_and(|t| t.id == *id);
        let state_of = |info: &BranchInfo| -> Result<Arc<Value>, OpError> {
            if is_this(&info.id) { Ok(Arc::new(live.clone())) } else { tree(store, &info.head) }
        };
        let review = target.review.clone();
        if target.id == folder.id {
            return Ok(Plan::answer(json!({
                "branch": target.name,
                "head": target.head.to_string(),
                "folder": true,
                "review": review,
                "summary": format!("{} is the folder branch", target.name),
            })));
        }
        let point = store.point(&target.head).map_err(branch_error)?;
        let base = base_with(store, &folder, &point);
        let base_tree = match base {
            Some(base) => tree(store, &base)?,
            None => Arc::new(Value::Null),
        };
        let here = changed_units(&base_tree, &*state_of(&target)?);
        let there = changed_units(&base_tree, &*state_of(&folder)?);
        let summary = match base {
            Some(_) => format!("{} changed here, {} on {}", here.len(), there.len(), folder.name),
            None => format!("no common version with {}: every difference is a choice", folder.name),
        };
        Ok(Plan::answer(json!({
            "branch": target.name,
            "head": target.head.to_string(),
            "seq": target.seq,
            "folder": false,
            "base": base.map(|b| b.to_string()),
            "changed_here": here,
            "changed_on_folder": there,
            "review": review,
            "lane": target.lane,
            "summary": summary,
        })))
    }
}
register_op!(BranchStatus);
