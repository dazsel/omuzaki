//! `model.remove`: free a model's files.

use omuzaki_ai_api::ModelRegistry;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Removes a model's files from the model cache. A model the person added is forgotten, with their acceptance of its
/// notice (their own copy stays where they keep it); a shipped one is uninstalled and can be installed again. A slot
/// that runs it falls back to its shipped default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelRemove {
    /// The model, as `model.list` keys it (`id@version#digest`).
    pub model: String,
}

impl Op for ModelRemove {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.remove",
            summary: "Remove a model's files: an added model is forgotten, a shipped one uninstalled.",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let registry = ctx.service::<dyn ModelRegistry>()?;
        let model = super::model("model", &self.model, registry)?;
        if !ctx.dry_run {
            registry.remove(&model).map_err(|e| super::error(&e))?;
        }
        Ok(Plan::answer(json!({ "removed": super::full_name(&model), "dry_run": ctx.dry_run })))
    }
}

register_op!(ModelRemove);
