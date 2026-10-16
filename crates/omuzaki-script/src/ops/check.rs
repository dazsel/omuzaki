//! `script.check`: what a script names and what is wrong with it, without running it.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{Limits, registered_host};

/// Parses and resolves a script without running it ([`ScriptHost::check`](crate::ScriptHost::check)). The answer is
/// the [`CheckReport`](crate::CheckReport) as JSON: `ops` (every op named, with line and column, including calls inside
/// functions and closures), `unknown_ops`, `field_errors` (unknown and missing fields of argument maps written as
/// literals), `refused` (calls a script may not make), `scopes`, `ceiling` and `parse_errors`. The console runs it as you
/// type; an agent runs it to see what a script needs before asking for the `script` scope.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScriptCheck {
    /// The Rhai source to check (at most 64 KiB).
    pub source: String,
}

impl Op for ScriptCheck {
    fn meta() -> OpMeta {
        OpMeta {
            name: "script.check",
            summary: "Check a script without running it: the ops it names, unknown ops, bad fields, the scopes it needs.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let report = registered_host(Limits::console()).check(&self.source);
        let report = serde_json::to_value(report)
            .map_err(|e| OpError::internal(format!("serialising the check report failed: {e}")))?;
        Ok(Plan::answer(report))
    }
}

register_op!(ScriptCheck);
