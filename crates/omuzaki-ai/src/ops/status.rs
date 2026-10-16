//! `model.status`: whether one slot can run, or how far its model's download has got (`docs/design/first-run.md` §5).

use omuzaki_ai_api::ModelRegistry;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Says whether a slot (`face_tracker@1`, say) is ready, is downloading its model (with the percent done), or has no
/// model it can run. Reads only; poll it while a first run downloads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ModelStatus {
    /// The slot, as `id@version`.
    pub slot: String,
}

/// Where the slot stands.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Readiness {
    /// A model can run in the slot now.
    Ready,
    /// Nothing runs yet, and the slot's model is downloading.
    Downloading {
        /// How much is done, 0 to 100.
        percent: f32,
    },
    /// No model can run, and none is downloading: the slot's `without_model` behaviour applies.
    Unavailable {
        /// Why, when something went wrong (a download that failed its hash, a model that crashed).
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<String>,
    },
}

impl Op for ModelStatus {
    fn meta() -> OpMeta {
        OpMeta {
            name: "model.status",
            summary: "Say whether a model slot is ready, downloading (with the percent) or unavailable.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let registry = ctx.service::<dyn ModelRegistry>()?;
        let slot = super::slot("slot", &self.slot, registry)?;
        let status = registry.slots().into_iter().find(|s| s.info.slot == slot);
        let readiness = match (status, registry.downloading(&slot)) {
            (Some(s), _) if s.active.is_some() => Readiness::Ready,
            (_, Some(fraction)) => Readiness::Downloading { percent: (fraction * 100.0).clamp(0.0, 100.0) },
            (status, None) => Readiness::Unavailable { reason: status.and_then(|s| s.fallback) },
        };
        let answer = serde_json::to_value(&readiness)
            .map_err(|e| OpError::internal(format!("writing the slot status failed: {e}")))?;
        Ok(Plan::answer(answer))
    }
}

register_op!(ModelStatus);
