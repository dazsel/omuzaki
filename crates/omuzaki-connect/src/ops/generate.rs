//! `connect.generate`: make assets or answers through a connector, as a job.

use omuzaki_connect_api::{ConnectorId, Request, RunId};
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::call::{Limits, finish_call, plan_call, work_call};

/// Makes images, textures, models, voice, music, motion or answers through a connector. The plan (and a dry-run) is
/// the preview: what leaves this computer and where, the price and the outputs' licence; nothing is sent until it
/// runs. Generated files land in the tray as pending candidates, in one undo step; text and JSON answers come back
/// in the output. Money already billed is not refunded by an undo.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectGenerate {
    /// What to make: tagged by `capability` (`image`, `texture`, `model3d`, `voice`, `music`, `motion`, `llm`,
    /// `vision`, `dcc`).
    pub request: Request,
    /// The connector; omitted when exactly one enabled connector makes this.
    #[serde(default)]
    pub connector: Option<ConnectorId>,
    /// The price the person was shown, micro-dollars; a dearer estimate at run time fails and nothing is sent.
    #[serde(default)]
    pub expect_usd_micros: Option<u64>,
    /// The most this call may bill, micro-dollars. Omitted means the estimate.
    #[serde(default)]
    pub max_usd_micros: Option<u64>,
    /// An earlier run to reuse when its candidates still match this request (a replay): nothing is sent or billed.
    #[serde(default)]
    pub reuse: Option<RunId>,
}

impl ConnectGenerate {
    fn limits(&self) -> Limits {
        Limits {
            connector: self.connector.clone(),
            expect_usd_micros: self.expect_usd_micros,
            max_usd_micros: self.max_usd_micros,
            reuse: self.reuse,
            regenerated_from: None,
        }
    }
}

impl Op for ConnectGenerate {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.generate",
            summary: "Make images, textures, models, voice, music, motion or answers through a connector; files land in the tray for a person to accept.",
            kind: OpKind::Job,
            scope: Scope::Net,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        plan_call(ctx, &self.request, &self.limits())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        work_call(job, &self.limits())
    }

    fn finish(&self, ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
        finish_call(ctx, result, &self.limits())
    }
}

register_op!(ConnectGenerate);
