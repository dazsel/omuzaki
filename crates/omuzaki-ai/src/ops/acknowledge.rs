//! `model.acknowledge`: the person accepts an added model's licence notice (ADR 0016 rule 2).

use omuzaki_ai_api::{DISCLAIMER, ModelRegistry};
use omuzaki_ops_api::{Actor, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Records that the person read and accepted the notice for a model they added, once per exact file contents. Only a
/// person can do this, in the Omuzaki window: an agent's request is refused, and no approval can stand in for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelAcknowledge {
    /// The model, as `model.list` keys it (`id@version#digest`).
    pub model: String,
}

impl Op for ModelAcknowledge {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.acknowledge",
            summary: "Accept the licence notice of a model you added (people only).",
            kind: OpKind::Edit,
            scope: Scope::EditLibrary,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let Actor::Person { name } = ctx.actor else {
            return Err(OpError::person_only("accepting a model's licence notice"));
        };
        let registry = ctx.service::<dyn ModelRegistry>()?;
        let model = super::model("model", &self.model, registry)?;
        if !ctx.dry_run {
            registry.acknowledge(&model, name).map_err(|e| super::error(&e))?;
        }
        Ok(Plan::answer(json!({
            "acknowledged": super::full_name(&model),
            "notice": DISCLAIMER,
            "dry_run": ctx.dry_run,
        })))
    }
}

register_op!(ModelAcknowledge);
