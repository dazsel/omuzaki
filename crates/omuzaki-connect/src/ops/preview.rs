//! `connect.preview`: what a call would send, where, at what price and under which rights. Calls nothing.

use omuzaki_connect_api::{ConnectorId, Request, tray};
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Previews a connector call without making it: whether anything leaves this computer and where, what is sent, the
/// price, what the vendor keeps and the licence the outputs would carry, with one English sentence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectPreview {
    /// What to make: tagged by `capability` (`image`, `texture`, `model3d`, `voice`, `music`, `motion`, `llm`,
    /// `vision`, `dcc`).
    pub request: Request,
    /// The connector; omitted when exactly one enabled connector makes this.
    #[serde(default)]
    pub connector: Option<ConnectorId>,
}

impl Op for ConnectPreview {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.preview",
            summary: "Preview a connector call without making it: what leaves this computer and where, the price and the outputs' licence.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = super::connectors(ctx)?;
        let store = ctx.service::<dyn Store>()?;
        let rights = tray::input_rights(ctx.doc, store, &self.request).map_err(super::tray_error)?;
        let preview = host
            .preview(&self.request, self.connector.as_ref(), super::requester(ctx.actor), &rights)
            .map_err(super::preview_error)?;
        let cost = CostHint {
            usd: Some(super::dollars(preview.estimate.usd_micros)),
            seconds: preview.estimate.seconds,
            gpu_mib: None,
        };
        let output = super::call::with_english(&preview, None)?;
        Ok(Plan { output: Some(output), cost: Some(cost), ..Plan::default() })
    }
}

register_op!(ConnectPreview);
