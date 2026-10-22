//! `input.doctor`: what is hurting tracking now, and how to fix it (PF-28).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use omuzaki_track_api::doctor::Diagnosis;

use super::host;

/// Read the tracking doctor: each problem the running tracker measures (too dark, a light behind the person, glare on
/// glasses, the face at the edge, lost frames, an overloaded machine, a phone's uneven Wi-Fi), with the reading, the
/// line it crossed and the fix in plain words. Empty when tracking is healthy or no capture runs.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("examples" = [{}]))]
pub struct Doctor {}

impl Op for Doctor {
    fn meta() -> OpMeta {
        OpMeta {
            name: "input.doctor",
            summary: "Read what is hurting tracking now (light, glare, framing, lost frames, load, jitter) and the fix.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let findings: Vec<Value> = host(ctx)?.diagnoses().iter().map(finding).collect();
        Ok(Plan::answer(json!({ "healthy": findings.is_empty(), "findings": findings })))
    }
}

/// One diagnosis as the ops answer it: the finding's id, the reading, the threshold and the fix.
pub(super) fn finding(diagnosis: &Diagnosis) -> Value {
    json!({
        "finding": diagnosis.finding.id(),
        "measured": diagnosis.measured,
        "threshold": diagnosis.threshold,
        "fix": diagnosis.fix(),
    })
}

omuzaki_ops_api::register_op!(Doctor);
