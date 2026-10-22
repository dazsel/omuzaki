//! `connect.list`: every connector this app has, with what it makes and whether it can run now.

use omuzaki_connect_api::{Capability, ConnectorId, Status};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Lists every connector, sorted by id: what it makes, whether it costs money or sends anything off this computer,
/// and whether it can run now (disabled, needs a key, ready...).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectList {}

/// One row of `connect.list`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Listed {
    /// The id other `connect.*` ops take.
    pub id: ConnectorId,
    /// Its name.
    pub name: String,
    /// What it makes.
    pub capabilities: Vec<Capability>,
    /// Whether it costs money.
    pub paid: bool,
    /// Whether, as shipped, it sends what it is given off this computer (a person's endpoint can change that; the
    /// preview says for each call).
    pub leaves_machine: bool,
    /// Whether it can run now.
    pub status: Status,
}

impl Op for ConnectList {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.list",
            summary: "List the connectors: what each makes, whether it costs money or leaves this computer, and whether it can run now.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = super::connectors(ctx)?;
        let rows: Vec<Listed> = host
            .list()
            .into_iter()
            .map(|m| Listed {
                status: host.status(&m.id).unwrap_or(Status::Disabled),
                capabilities: m.capabilities.iter().map(|c| c.capability).collect(),
                paid: m.paid(),
                leaves_machine: m.privacy.leaves_machine,
                name: m.name,
                id: m.id,
            })
            .collect();
        let rows = serde_json::to_value(rows).map_err(|e| OpError::internal(format!("writing the list: {e}")))?;
        Ok(Plan::answer(rows))
    }
}

register_op!(ConnectList);
