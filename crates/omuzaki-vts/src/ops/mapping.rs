//! `vts.map` (an undoable edit of the project's `connections/vts`), `vts.trigger` and `vts.release` (live actions) and
//! `vts.suggest` (a query): hotkeys first, expressions only by an explicit choice (#728).

use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{
    connected, connections, live_action, mapping_path, query, service, session_or_offline, stored_mapping,
    synced_mapping,
};
use crate::client::Connected;
use crate::hotkeys::{Row, Target, VtsMapping, suggest};
use crate::limits::FadeTime;

/// Set what Omuzaki's expressions and triggers run in a `VTube Studio` model: a hotkey of the model, or (only by
/// choice) an expression file. Kept in the project (`connections/vts`), so it travels with it and undoes like any
/// edit. `vts.suggest` proposes hotkey rows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {
        "model": "a-model-id",
        "rows": [{ "omuzaki": "smug", "target": { "kind": "hotkey", "id": "h-smug", "name": "Smug" } }],
        "merge": true,
    },
    {
        "model": "a-model-id",
        "rows": [{ "omuzaki": "cry", "target": { "kind": "expression", "file": "Cry.exp3.json", "fade_s": 0.5 } }],
    },
]))]
pub struct Map {
    /// The VTS model id. Default: the model VTS has loaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The rows.
    pub rows: Vec<Row>,
    /// Keep the model's other rows and replace only those for these ids. Default: replace them all.
    #[serde(default)]
    pub merge: bool,
}

/// Propose hotkey rows for Omuzaki ids from the loaded model's hotkey names. Applies nothing: `vts.map` does.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "ids": ["smug", "angry", "wave"] }]))]
pub struct Suggest {
    /// The Omuzaki expression or trigger ids.
    pub ids: Vec<String>,
}

/// Run what an Omuzaki id is mapped to in the loaded model: its hotkey, or its expression switched on.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "smug" }]))]
pub struct Trigger {
    /// The Omuzaki id, as `vts.status` lists the mapping.
    pub id: String,
}

/// Switch off the expression an Omuzaki id is mapped to, if the bridge switched it on. A hotkey ends by its own rules.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": "cry" }]))]
pub struct Release {
    /// The Omuzaki id.
    pub id: String,
}

/// The loaded model's id, or the error saying there is none.
fn loaded(session: &Connected) -> Result<(&str, &str), OpError> {
    session
        .model
        .as_ref()
        .map(|m| (m.id.as_str(), m.name.as_str()))
        .ok_or_else(|| OpError::conflict("VTube Studio has no model loaded").hint("load a model in VTube Studio"))
}

impl Map {
    /// The rows as they will be stored: fades clamped, ids checked.
    fn rows(&self, session: Option<&Connected>, model: &str) -> Result<Vec<Row>, OpError> {
        let mut rows = Vec::with_capacity(self.rows.len());
        for (i, row) in self.rows.iter().enumerate() {
            if row.omuzaki.trim().is_empty() {
                return Err(OpError::invalid(format!("rows[{i}].omuzaki"), "an Omuzaki id, like smug"));
            }
            if self.rows[..i].iter().any(|r| r.omuzaki == row.omuzaki) {
                return Err(OpError::invalid(format!("rows[{i}].omuzaki"), format!("{} is mapped twice", row.omuzaki)));
            }
            let target = match &row.target {
                Target::Hotkey { id, name } => {
                    // A row for the loaded model must name one of its hotkeys; another model's can't be checked now.
                    let hotkeys = session.filter(|s| s.model.as_ref().is_some_and(|m| m.id == model));
                    if let Some(s) = hotkeys
                        && !s.hotkeys.iter().any(|h| &h.hotkey_id == id)
                    {
                        let named = s.hotkeys.iter().find(|h| h.name.eq_ignore_ascii_case(name));
                        let error = OpError::not_found(format!("the loaded model has no hotkey with id {id}"));
                        return Err(match named {
                            Some(h) => error.hint(format!("the hotkey named {} has id {}", h.name, h.hotkey_id)),
                            None => error.hint("vts.status lists the model's hotkeys; vts.suggest proposes rows"),
                        });
                    }
                    row.target.clone()
                }
                Target::Expression { file, fade_s } => {
                    if !file.ends_with(".exp3.json") {
                        return Err(OpError::invalid(format!("rows[{i}].target.file"), "an .exp3.json file"));
                    }
                    let fade_s = FadeTime::new(*fade_s).seconds();
                    Target::Expression { file: file.clone(), fade_s }
                }
            };
            rows.push(Row { omuzaki: row.omuzaki.clone(), target });
        }
        Ok(rows)
    }
}

impl Op for Map {
    fn meta() -> OpMeta {
        OpMeta {
            name: "vts.map",
            summary: "Set what Omuzaki's expressions and triggers run in a VTube Studio model (kept in the project).",
            kind: OpKind::Edit,
            scope: Scope::EditScene,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let session = connected(service).ok();
        let model = match (&self.model, &session) {
            (Some(model), _) => model.clone(),
            (None, Some(s)) => loaded(s)?.0.to_owned(),
            (None, None) => {
                return Err(OpError::conflict("no model named and none loaded")
                    .hint("name the VTS model id, or connect with vts.connect and load a model"));
            }
        };
        let rows = self.rows(session.as_ref(), &model)?;
        let mut mapping = stored_mapping(ctx)?;
        let entry = mapping.models.entry(model.clone()).or_default();
        if self.merge {
            entry.retain(|old| !rows.iter().any(|new| new.omuzaki == old.omuzaki));
            entry.extend(rows);
        } else {
            *entry = rows;
        }
        let now = entry.clone();
        if now.is_empty() {
            mapping.models.remove(&model);
        }
        let mut plan = Plan::edit(patch(ctx, &mapping)?);
        plan.output = Some(json!({ "model": model, "rows": now }));
        Ok(plan)
    }
}

/// The patch that stores `mapping`: one change at `connections/vts`, or at `connections` when the project has none yet
/// (a change's parent must exist, which keeps the undo exact).
fn patch(ctx: &OpCtx<'_>, mapping: &VtsMapping) -> Result<Patch, OpError> {
    let after = serde_json::to_value(mapping).map_err(|e| OpError::internal(e.to_string()))?;
    let stored = ctx.doc.get(&mapping_path()).cloned();
    Ok(if ctx.doc.get(&connections()).is_some() {
        Patch::new().with(mapping_path(), stored, Some(after))
    } else {
        Patch::new().with(connections(), None, Some(json!({ "vts": after })))
    })
}

impl Op for Suggest {
    fn meta() -> OpMeta {
        query("vts.suggest", "Propose hotkey rows for Omuzaki ids from the loaded VTube Studio model's hotkeys.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let session = match session_or_offline(ctx, service)? {
            Ok(session) => session,
            Err(offline) => return Ok(offline),
        };
        let (model, _) = loaded(&session)?;
        let ids: Vec<&str> = self.ids.iter().map(String::as_str).collect();
        let rows = suggest(&ids, &session.hotkeys);
        let unmatched: Vec<&str> = ids.iter().copied().filter(|id| !rows.iter().any(|r| r.omuzaki == *id)).collect();
        Ok(Plan::answer(json!({ "model": model, "rows": rows, "unmatched": unmatched })))
    }
}

impl Op for Trigger {
    fn meta() -> OpMeta {
        live_action("vts.trigger", "Run what an Omuzaki id is mapped to in the loaded VTube Studio model.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let session = match session_or_offline(ctx, service)? {
            Ok(session) => session,
            Err(offline) => return Ok(offline),
        };
        let (model, name) = loaded(&session)?;
        let mapping = synced_mapping(ctx, service)?;
        let target = mapping.resolve(model, &self.id).cloned().ok_or_else(|| {
            OpError::not_found(format!("{} is not mapped in the model {name}", self.id))
                .hint("vts.suggest proposes rows from the model's hotkeys; vts.map sets them")
        })?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "id": self.id, "target": target })));
        }
        service.trigger(&self.id);
        Ok(Plan::answer(json!({ "triggered": self.id, "target": target })))
    }
}

impl Op for Release {
    fn meta() -> OpMeta {
        live_action("vts.release", "Switch off the VTube Studio expression an Omuzaki id switched on.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let session = match session_or_offline(ctx, service)? {
            Ok(session) => session,
            Err(offline) => return Ok(offline),
        };
        let (model, name) = loaded(&session)?;
        let mapping = synced_mapping(ctx, service)?;
        let target = mapping
            .resolve(model, &self.id)
            .cloned()
            .ok_or_else(|| OpError::not_found(format!("{} is not mapped in the model {name}", self.id)))?;
        let Target::Expression { file, .. } = &target else {
            return Ok(Plan::answer(json!({ "released": false, "why": "a hotkey ends by its own rules in VTS" })));
        };
        let on = session.expressions_on.contains(file);
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "id": self.id, "file": file, "on": on })));
        }
        service.release(&self.id);
        Ok(Plan::answer(json!({ "released": on, "file": file })))
    }
}

register_op!(Map);
register_op!(Suggest);
register_op!(Trigger);
register_op!(Release);
