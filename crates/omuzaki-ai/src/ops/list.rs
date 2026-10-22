//! `model.list`: every slot and every model, shipped and added, as data.

use omuzaki_ai_api::{ModelEntry, ModelRegistry, SlotStatus};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::IMPERSONATION_WARNING;

/// Lists the model slots and the models for them: shipped and added by the person, with each model's licence, status
/// (with the reason when it can't be used), whether the person acknowledged it (a status of `needs_acknowledgement`
/// says not yet) and its latest smoke-test result. No local path appears. Pass `slot` (`id@version`, as listed) for
/// one slot. A model's `key` (`id@version#digest`) is what `model.select`, `model.remove` and `model.acknowledge`
/// take.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelList {
    /// One slot, as `id@version`; every slot when left out.
    #[serde(default)]
    pub slot: Option<String>,
}

/// One model in the answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Listed {
    /// How the other ops name it: `id@version#digest`.
    pub key: String,
    /// Everything the registry says about it.
    #[serde(flatten)]
    pub entry: ModelEntry,
    /// The impersonation warning (SE-05), for a model that imitates a real person.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// What `model.list` answers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Listing {
    /// The slots, with what is chosen, what runs and why that differs.
    pub slots: Vec<SlotStatus>,
    /// The models, by slot, id and version.
    pub models: Vec<Listed>,
}

impl Op for ModelList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.list",
            summary: "List the model slots and every shipped and added model, with licences, statuses and smoke-test \
                      results.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let registry = ctx.service::<dyn ModelRegistry>()?;
        let slot = self.slot.as_deref().map(|s| super::slot("slot", s, registry)).transpose()?;
        let slots = registry.slots().into_iter().filter(|s| slot.as_ref().is_none_or(|w| &s.info.slot == w)).collect();
        let models = registry
            .models(slot.as_ref())
            .into_iter()
            .map(|entry| Listed {
                key: super::full_name(&entry.model),
                warning: entry.imitates.as_ref().map(|_| IMPERSONATION_WARNING.to_owned()),
                entry,
            })
            .collect();
        let listing = serde_json::to_value(Listing { slots, models })
            .map_err(|e| OpError::internal(format!("listing the models failed: {e}")))?;
        Ok(Plan::answer(listing))
    }
}

register_op!(ModelList);
