//! `output.kinds`, `output.list`, `output.start` and `output.stop`: the live outputs as ops (spec v3 BR-01, BR-02).
//!
//! An output kind (a virtual camera, a Spout sender) registers itself with `register_output!`; these ops start one by
//! its kind's name at the program's size and rate. Starting and stopping are live actions in the `perform` scope
//! (they show the person to other apps), like the output window's ops; the two queries need `read` only.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stream_api::output::{OutputConfig, find_output_kind, output_kinds};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::record::op_error;
use crate::service::{OutputId, StreamService};

/// List the kinds of output this build can start, and whether each works on this machine now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Kinds {}

/// List the running outputs with their counters.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct List {}

/// Start an output of a registered kind at the program's size and frame rate.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "kind": "vcam" }, { "kind": "vcam", "name": "Omuzaki Camera" }]))]
pub struct Start {
    /// The kind's name, from `output.kinds`.
    pub kind: String,
    /// The name receivers see. Default `Omuzaki`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// Stop a running output.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "id": 1 }]))]
pub struct Stop {
    /// The output's id, from `output.list` or `output.start`.
    pub id: u64,
}

/// The name receivers see when none is asked for.
pub const DEFAULT_NAME: &str = "Omuzaki";

impl Op for Kinds {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.kinds",
            summary: "List the kinds of live output (virtual camera, Spout, NDI) and whether each works on this machine.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, _ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let kinds: Vec<_> = output_kinds()
            .into_iter()
            .map(|k| {
                let (available, hint) = match (k.available)() {
                    Ok(()) => (true, None),
                    Err(e) => (false, Some(e.to_string())),
                };
                json!({ "kind": k.name, "summary": k.summary, "available": available, "hint": hint })
            })
            .collect();
        Ok(Plan::answer(json!({ "kinds": kinds })))
    }
}

impl Op for List {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.list",
            summary: "List the running outputs and recordings with their frame counts and errors.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        Ok(Plan::answer(json!({ "outputs": service.outputs(), "counters": service.counters() })))
    }
}

impl Op for Start {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.start",
            summary: "Start a live output (such as the virtual camera) of the program picture.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        let kind = find_output_kind(&self.kind).ok_or_else(|| {
            let names: Vec<_> = output_kinds().into_iter().map(|k| k.name).collect();
            OpError::invalid("kind", format!("no output kind {:?}; this build has {names:?}", self.kind))
        })?;
        let program = service.program();
        let cfg =
            OutputConfig::new(self.name.clone().unwrap_or_else(|| DEFAULT_NAME.to_owned()), program.size, program.fps)
                .map_err(op_error)?;
        let available = (kind.available)();
        if ctx.dry_run {
            // A dry run reports whether the start would work here, rather than failing on this machine's state.
            return Ok(Plan::answer(json!({
                "started": false,
                "kind": kind.name,
                "name": cfg.name,
                "available": available.is_ok(),
                "hint": available.err().map(|e| e.to_string()),
            })));
        }
        available.map_err(op_error)?;
        let id = service.start((kind.make)(), cfg.clone()).map_err(op_error)?;
        Ok(Plan::answer(json!({
            "started": true,
            "id": id,
            "kind": kind.name,
            "name": cfg.name,
            "size": [cfg.size.x, cfg.size.y],
            "fps": [cfg.fps.0, cfg.fps.1],
        })))
    }
}

impl Op for Stop {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.stop",
            summary: "Stop a live output.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = ctx.service::<StreamService>()?;
        let id = OutputId(self.id);
        let running = service.outputs().iter().any(|o| o.id == id);
        if ctx.dry_run {
            // A dry run reports whether the output runs, rather than failing on this moment's state.
            return Ok(Plan::answer(json!({ "stopped": false, "id": id, "running": running })));
        }
        if !running {
            return Err(OpError::not_found(format!("no running output {}", self.id)));
        }
        if is_recording(service, id) {
            return Err(OpError::conflict("that output is the recording").hint("stop it with record.stop"));
        }
        let status = service.stop(id).map_err(op_error)?;
        Ok(Plan::answer(json!({ "stopped": true, "id": id, "frames": status.frames, "dropped": status.dropped })))
    }
}

fn is_recording(service: &StreamService, id: OutputId) -> bool {
    service.outputs().iter().any(|o| o.id == id && o.kind == crate::record::runtime::KIND)
}

register_op!(Kinds);
register_op!(List);
register_op!(Start);
register_op!(Stop);
