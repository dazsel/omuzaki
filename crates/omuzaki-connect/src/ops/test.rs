//! `connect.test`: a free reachability check.

use omuzaki_connect_api::ConnectorId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Checks that a connector's service or program can be reached. Free: nothing is billed or made.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectTest {
    /// The connector, as `connect.list` names it.
    pub connector: ConnectorId,
}

impl Op for ConnectTest {
    fn meta() -> OpMeta {
        OpMeta {
            name: "connect.test",
            summary: "Check that a connector's service or program can be reached. Free: nothing is billed or made.",
            kind: OpKind::Job,
            scope: Scope::Net,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = super::connectors(ctx)?;
        if host.describe(&self.connector).is_none() {
            return Err(super::unknown(&self.connector, host));
        }
        Ok(Plan::default())
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let host = job.service::<dyn omuzaki_connect_api::Connectors>()?;
        Ok(match host.ping(&self.connector, &super::JobProgress(job)) {
            Ok(()) => json!({ "ok": true }),
            Err(e) => json!({ "ok": false, "error": e.to_string() }),
        })
    }
}

register_op!(ConnectTest);
