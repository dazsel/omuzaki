//! The `obs.*` actions: scene, source visibility, stream, record and OBS's own replay buffer. All are live actions
//! (`Undo::None`) under the `broadcast` scope, so a hotkey, the automation graph (`op.run`) and the AI performer call
//! the same ops a person does. OBS's replay buffer is not Omuzaki's (`record.save_replay`, BR-17): the names differ on
//! purpose (`obs.replay.*`).
//!
//! An action that finds OBS already in the asked-for state (OBS answers "output running" or "output not running")
//! succeeds with `already: true`, after one request.

use std::time::Duration;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::query::no_scene;
use super::{live_action, nearest, op_error, service, would};
use crate::client::ObsError;
use crate::limits::{STATUS_OUTPUT_NOT_RUNNING, STATUS_OUTPUT_RUNNING, STATUS_RESOURCE_NOT_FOUND};
use crate::protocol::{
    GetLastReplayBufferReplay, GetSceneItemId, GetSceneItemList, ObsEvent, RequestType, SaveReplayBuffer,
    SetCurrentProgramScene, SetSceneItemEnabled, StartRecord, StartReplayBuffer, StartStream, StopRecord,
    StopReplayBuffer, StopStream,
};
use crate::service::ObsService;

/// How long `obs.replay.save` waits for OBS to say where it saved the replay.
pub const REPLAY_SAVE_WAIT: Duration = Duration::from_secs(2);

/// Switch OBS's program to a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "scene": "Main" }]))]
pub struct SceneSet {
    /// The scene, as `obs.scenes` lists it.
    pub scene: String,
}

/// Show or hide a source in a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "scene": "Main", "source": "Alerts", "visible": false }]))]
pub struct SourceSetVisible {
    /// The scene, as `obs.scenes` lists it.
    pub scene: String,
    /// The source, as `obs.sources` lists it.
    pub source: String,
    /// Show it (`true`) or hide it.
    pub visible: bool,
}

macro_rules! no_args {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
        #[serde(deny_unknown_fields)]
        #[schemars(extend("examples" = [{}]))]
        pub struct $name {}
    };
}

no_args!(
    /// Start OBS's stream: goes live with OBS's own stream settings.
    StreamStart
);
no_args!(
    /// Stop OBS's stream: ends the broadcast.
    StreamStop
);
no_args!(
    /// Start OBS's own recording.
    RecordStart
);
no_args!(
    /// Stop OBS's own recording; answers the file it wrote.
    RecordStop
);
no_args!(
    /// Start OBS's replay buffer.
    ReplayStart
);
no_args!(
    /// Stop OBS's replay buffer.
    ReplayStop
);
no_args!(
    /// Save OBS's replay buffer to a file; answers its path.
    ReplaySave
);

/// Sends a start or stop request; OBS's "already running" or "not running" is success with `already: true`.
fn toggle<R: RequestType>(service: &ObsService, request: &R, already: u32) -> Result<(bool, Option<R::Response>), OpError> {
    match service.request(request) {
        Ok(response) => Ok((false, Some(response))),
        Err(ObsError::Refused { code, .. }) if code == already => Ok((true, None)),
        Err(e) => Err(op_error(e)),
    }
}

/// Starts or stops an output (the dry run is answered by the caller).
fn output<R: RequestType>(
    ctx: &OpCtx<'_>,
    request: &R,
    output: &str,
    start: bool,
) -> Result<(Value, Option<R::Response>), OpError> {
    let service = service(ctx)?;
    let state = if start { "started" } else { "stopped" };
    let code = if start { STATUS_OUTPUT_RUNNING } else { STATUS_OUTPUT_NOT_RUNNING };
    let (already, response) = toggle(service, request, code)?;
    Ok((json!({ "applied": true, "output": output, "output_active": start, "state": state, "already": already }), response))
}

fn answer(value: Value) -> Plan {
    Plan::answer(value)
}

impl Op for SceneSet {
    fn meta() -> OpMeta {
        live_action("obs.scene.set", "Switch OBS's program to a scene.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if self.scene.trim().is_empty() {
            return Err(OpError::invalid("scene", "name a scene from obs.scenes"));
        }
        if ctx.dry_run {
            return Ok(would(service, &json!({ "scene": self.scene })));
        }
        match service.request(&SetCurrentProgramScene { scene_name: self.scene.clone() }) {
            Ok(_) => Ok(answer(json!({ "applied": true, "scene": self.scene }))),
            Err(ObsError::Refused { code: STATUS_RESOURCE_NOT_FOUND, .. }) => Err(no_scene(service, &self.scene)),
            Err(e) => Err(op_error(e)),
        }
    }
}

impl Op for SourceSetVisible {
    fn meta() -> OpMeta {
        live_action("obs.source.set_visible", "Show or hide a source in an OBS scene.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if self.scene.trim().is_empty() || self.source.trim().is_empty() {
            return Err(OpError::invalid("source", "name a scene from obs.scenes and a source from obs.sources"));
        }
        let what = json!({ "scene": self.scene, "source": self.source, "visible": self.visible });
        if ctx.dry_run {
            return Ok(would(service, &what));
        }
        let id = match service
            .request(&GetSceneItemId { scene_name: self.scene.clone(), source_name: self.source.clone() })
        {
            Ok(found) => found.scene_item_id,
            Err(ObsError::Refused { code: STATUS_RESOURCE_NOT_FOUND, .. }) => return Err(self.not_found(service)),
            Err(e) => return Err(op_error(e)),
        };
        service
            .request(&SetSceneItemEnabled {
                scene_name: self.scene.clone(),
                scene_item_id: id,
                scene_item_enabled: self.visible,
            })
            .map_err(op_error)?;
        let mut done = what;
        done["applied"] = json!(true);
        Ok(answer(done))
    }
}

impl SourceSetVisible {
    /// The scene, or the source in it, doesn't exist: name the nearest.
    fn not_found(&self, service: &ObsService) -> OpError {
        match service.request(&GetSceneItemList { scene_name: self.scene.clone() }) {
            Ok(items) => {
                let names: Vec<&str> =
                    items.scene_items.iter().filter_map(|i| i.get("sourceName").and_then(Value::as_str)).collect();
                let error = OpError::not_found(format!("the scene {} has no source named {}", self.scene, self.source));
                match nearest(&self.source, names) {
                    Some(near) => error.hint(format!("did you mean {near}? obs.sources lists them")),
                    None => error.hint("the scene has no sources"),
                }
            }
            Err(_) => no_scene(service, &self.scene),
        }
    }
}

macro_rules! output_op {
    ($ty:ident, $name:literal, $summary:literal, $request:expr, $output:literal, $start:literal) => {
        impl Op for $ty {
            fn meta() -> OpMeta {
                live_action($name, $summary)
            }

            fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
                if ctx.dry_run {
                    let state = if $start { "started" } else { "stopped" };
                    return Ok(would(service(ctx)?, &json!({ "output": $output, "state": state })));
                }
                output(ctx, &$request, $output, $start).map(|(value, _)| answer(value))
            }
        }
    };
}

output_op!(StreamStart, "obs.stream.start", "Start OBS's stream (goes live).", StartStream {}, "stream", true);
output_op!(StreamStop, "obs.stream.stop", "Stop OBS's stream (ends the broadcast).", StopStream {}, "stream", false);
output_op!(RecordStart, "obs.record.start", "Start OBS's own recording.", StartRecord {}, "record", true);
output_op!(ReplayStart, "obs.replay.start", "Start OBS's replay buffer.", StartReplayBuffer {}, "replay", true);
output_op!(ReplayStop, "obs.replay.stop", "Stop OBS's replay buffer.", StopReplayBuffer {}, "replay", false);

impl Op for RecordStop {
    fn meta() -> OpMeta {
        live_action("obs.record.stop", "Stop OBS's own recording; answers the file it wrote.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.dry_run {
            return Ok(would(service(ctx)?, &json!({ "output": "record", "state": "stopped" })));
        }
        let (mut value, response) = output(ctx, &StopRecord {}, "record", false)?;
        value["path"] = json!(response.map(|r| r.output_path));
        Ok(answer(value))
    }
}

impl Op for ReplaySave {
    fn meta() -> OpMeta {
        live_action("obs.replay.save", "Save OBS's replay buffer to a file; answers its path.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(would(service, &json!({ "output": "replay" })));
        }
        // Listen first, so the saved event can't slip past between the request and the subscription.
        let events = service.subscribe();
        match service.request(&SaveReplayBuffer {}) {
            Ok(_) => {}
            Err(ObsError::Refused { code: STATUS_OUTPUT_NOT_RUNNING, .. }) => {
                return Err(OpError::conflict("OBS's replay buffer isn't running").hint("start it with obs.replay.start"));
            }
            Err(e) => return Err(op_error(e)),
        }
        let deadline = std::time::Instant::now() + REPLAY_SAVE_WAIT;
        let mut path = None;
        while path.is_none() {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            if left.is_zero() {
                break;
            }
            if let Some(ObsEvent::ReplayBufferSaved { saved_replay_path }) = events.recv_timeout(left) {
                path = Some(saved_replay_path);
            }
        }
        if path.is_none() {
            path = service.request(&GetLastReplayBufferReplay {}).ok().map(|r| r.saved_replay_path);
        }
        Ok(answer(json!({ "applied": true, "output": "replay", "path": path })))
    }
}

register_op!(SceneSet);
register_op!(SourceSetVisible);
register_op!(StreamStart);
register_op!(StreamStop);
register_op!(RecordStart);
register_op!(RecordStop);
register_op!(ReplayStart);
register_op!(ReplayStop);
register_op!(ReplaySave);
