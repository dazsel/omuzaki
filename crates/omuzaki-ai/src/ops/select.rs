//! `model.select`: choose the model a slot runs, or go back to the shipped default.

use omuzaki_ai_api::ModelRegistry;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Chooses the model a slot runs; leave `model` out to go back to the shipped default. The switch happens between
/// runs, without a gap. An agent may ask for a model the person added only once the person has accepted its licence
/// notice: otherwise the answer is `needs_person`, and nothing switches. A model that is disabled or waits for a real
/// person's consent can't be chosen by anyone; the error says why.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelSelect {
    /// The slot, as `id@version`.
    pub slot: String,
    /// The model, as `model.list` keys it (`id@version#digest`; the id alone when only one version exists). Left out:
    /// the shipped default.
    #[serde(default)]
    pub model: Option<String>,
}

impl Op for ModelSelect {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.select",
            summary: "Choose the model a slot runs, or go back to the shipped default.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let registry = ctx.service::<dyn ModelRegistry>()?;
        let slot = super::slot("slot", &self.slot, registry)?;
        let model = self.model.as_deref().map(|m| super::model("model", m, registry)).transpose()?;
        if !ctx.dry_run {
            registry.select(&slot, model.as_ref(), super::requester(ctx.actor)).map_err(|e| super::error(&e))?;
        }
        let selected = model.as_ref().map(super::full_name);
        Ok(Plan::answer(json!({ "slot": slot.to_string(), "selected": selected, "dry_run": ctx.dry_run })))
    }
}

register_op!(ModelSelect);
