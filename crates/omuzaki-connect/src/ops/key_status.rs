//! `connect.key_status`: whether a connector's key is in the keychain. It asks the keychain whether it holds the label;
//! it never reads the key.

use omuzaki_connect_api::ConnectorId;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::host::Host;

/// Whether a connector needs a key and whether the keychain holds it. Never the key: a person enters keys on the
/// Connectors page.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectKeyStatus {
    /// The connector, as `connect.list` names it.
    pub connector: ConnectorId,
}

impl Op for ConnectKeyStatus {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.key_status",
            summary: "Say whether a connector needs a key and whether it is in the keychain (never the key itself).",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = ctx.service::<Host>()?;
        let status = host
            .key_status(&self.connector)
            .ok_or_else(|| super::unknown(&self.connector, host as &dyn omuzaki_connect_api::Connectors))?;
        let json = serde_json::to_value(status).map_err(|e| OpError::internal(format!("writing the status: {e}")))?;
        Ok(Plan::answer(json))
    }
}

register_op!(ConnectKeyStatus);
