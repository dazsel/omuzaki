//! `settings.get`, `settings.list` and `settings.changes`: what a setting holds, every setting, and who changed what.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_settings_api::event::CHANGE_LOG_CAPACITY;
use omuzaki_settings_api::registry;
use omuzaki_settings_api::service::ListFilter;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{describe, op_error, query, service};

/// How many changes `settings.changes` returns when no limit is given.
pub const DEFAULT_CHANGES: usize = 20;

/// Read one setting: its value, its default, where the value came from, who may change it, and its schema.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "key": "ui.scale" }]))]
pub struct Get {
    /// The setting's key, such as `ui.scale`.
    pub key: String,
}

/// List the settings, sorted by key: every one, one area's, or only those changed from their defaults.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "area": "ui" }, { "changed": true }]))]
pub struct List {
    /// Only this area: the key's first segment, such as `ui` or `obs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub area: Option<String>,
    /// Only settings whose value differs from the default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub changed: bool,
    /// Also list the stored values that could not be used (a bad value, a key no linked crate registers).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub unknown: bool,
}

/// The latest setting changes, newest first, each with who made it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "limit": 5 }]))]
pub struct Changes {
    /// How many, at most 200. Default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
}

impl Op for Get {
    fn meta() -> OpMeta {
        query("settings.get", "Read one setting: its value, default, source, who may change it, and its schema.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let settings = service(ctx)?;
        let resolved = settings.resolve(&self.key).map_err(|e| op_error(settings, &e))?;
        let mut answer = describe(&resolved);
        if let Some(reg) = registry::find(&self.key) {
            answer["schema"] = serde_json::to_value(reg.schema()).unwrap_or_default();
        }
        Ok(Plan::answer(answer))
    }
}

impl Op for List {
    fn meta() -> OpMeta {
        query("settings.list", "List the settings with their values, by area or only those changed from default.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let settings = service(ctx)?;
        let mut filter = ListFilter::all();
        if let Some(area) = &self.area {
            filter = filter.with_area(area.clone());
        }
        if self.changed {
            filter = filter.only_changed();
        }
        let listed: Vec<_> = settings.list(&filter).iter().map(describe).collect();
        let mut answer = json!({ "settings": listed });
        if self.unknown {
            answer["unknown"] = serde_json::to_value(settings.findings()).unwrap_or_default();
        }
        Ok(Plan::answer(answer))
    }
}

impl Op for Changes {
    fn meta() -> OpMeta {
        query("settings.changes", "List the latest setting changes, newest first, with who made each one.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let limit = self.limit.unwrap_or(DEFAULT_CHANGES);
        if limit == 0 || limit > CHANGE_LOG_CAPACITY {
            return Err(OpError::invalid("limit", format!("a limit from 1 to {CHANGE_LOG_CAPACITY}, not {limit}")));
        }
        let changes = service(ctx)?.changes(limit);
        let changes = serde_json::to_value(changes).map_err(|e| OpError::internal(e.to_string()))?;
        Ok(Plan::answer(json!({ "changes": changes })))
    }
}

register_op!(Get);
register_op!(List);
register_op!(Changes);
