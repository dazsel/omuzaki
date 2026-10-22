//! `connect.describe`: one connector's manifest, with the knobs (`params`) a request may set.

use omuzaki_connect_api::ConnectorId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Describes a connector: its manifest (what it makes, what it sends and where, its price, its licence terms and the
/// `params` a request may set). Never a person's settings, a command line or a key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectDescribe {
    /// The connector, as `connect.list` names it.
    pub connector: ConnectorId,
}

impl Op for ConnectDescribe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.describe",
            summary: "Describe a connector: what it makes and sends, its price, its licence terms and the params a request may set.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = super::connectors(ctx)?;
        let manifest = host.describe(&self.connector).ok_or_else(|| super::unknown(&self.connector, host))?;
        let json = serde_json::to_value(manifest).map_err(|e| OpError::internal(format!("writing the manifest: {e}")))?;
        Ok(Plan::answer(json))
    }
}

register_op!(ConnectDescribe);
