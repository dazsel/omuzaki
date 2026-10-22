//! `obs.status`, `obs.scenes` and `obs.sources`: queries under the `read` scope.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{nearest, op_error, query, service};
use crate::client::ObsError;
use crate::limits::STATUS_RESOURCE_NOT_FOUND;
use crate::protocol::{
    GetCurrentProgramScene, GetRecordStatus, GetReplayBufferStatus, GetSceneItemList, GetSceneList, GetStreamStatus,
};
use crate::service::{ObsService, ObsStatus};

/// Report the connection to OBS and, when connected, its program scene, stream, recording and replay buffer.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// List OBS's scenes, top first, and the one on program.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Scenes {}

/// List a scene's sources, top first, with whether each is visible.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "scene": "Main" }]))]
pub struct Sources {
    /// The scene, as `obs.scenes` lists it.
    pub scene: String,
}

/// The scene names, top first (OBS lists them bottom-up).
pub(crate) fn scene_names(service: &ObsService) -> Result<(Option<String>, Vec<String>), ObsError> {
    let list = service.request(&GetSceneList {})?;
    let names = list
        .scenes
        .iter()
        .rev()
        .filter_map(|s| s.get("sceneName").and_then(Value::as_str).map(str::to_owned))
        .collect();
    Ok((list.current_program_scene_name, names))
}

/// The error for a scene that doesn't exist, naming the nearest one.
pub(crate) fn no_scene(service: &ObsService, scene: &str) -> OpError {
    let names = scene_names(service).map(|(_, n)| n).unwrap_or_default();
    let error = OpError::not_found(format!("OBS has no scene named {scene}"));
    match nearest(scene, names.iter().map(String::as_str)) {
        Some(near) => error.hint(format!("did you mean {near}? obs.scenes lists them")),
        None => error.hint("obs.scenes lists the scenes"),
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        query("obs.status", "Report the connection to OBS and its program scene, stream, recording and replay buffer.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let status = service.status();
        let mut answer = serde_json::to_value(&status).map_err(|e| OpError::internal(e.to_string()))?;
        answer["connected"] = json!(matches!(status, ObsStatus::Connected { .. }) && service.client().is_some());
        if service.client().is_some() {
            let scene = service.request(&GetCurrentProgramScene {}).map_err(op_error)?;
            let stream = service.request(&GetStreamStatus {}).map_err(op_error)?;
            let record = service.request(&GetRecordStatus {}).map_err(op_error)?;
            // An OBS with the replay buffer turned off in its settings refuses the query: that's "not active".
            let replay = service.request(&GetReplayBufferStatus {}).is_ok_and(|r| r.output_active);
            answer["program_scene"] = json!(scene.scene_name);
            answer["stream_active"] = json!(stream.output_active);
            answer["stream_reconnecting"] = json!(stream.output_reconnecting);
            answer["stream_duration_ms"] = json!(stream.output_duration);
            answer["stream_skipped_frames"] = json!(stream.output_skipped_frames);
            answer["record_active"] = json!(record.output_active);
            answer["record_duration_ms"] = json!(record.output_duration);
            answer["replay_active"] = json!(replay);
        }
        Ok(Plan::answer(answer))
    }
}

impl Op for Scenes {
    fn meta() -> OpMeta {
        query("obs.scenes", "List OBS's scenes and the one on program.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if service.client().is_none() {
            return Ok(Plan::answer(
                json!({ "connected": false, "program": null, "scenes": [], "hint": "connect with obs.connect first" }),
            ));
        }
        let (program, names) = scene_names(service).map_err(op_error)?;
        Ok(Plan::answer(json!({ "connected": true, "program": program, "scenes": names })))
    }
}

impl Op for Sources {
    fn meta() -> OpMeta {
        query("obs.sources", "List a scene's sources, top first, with whether each is visible.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if self.scene.trim().is_empty() {
            return Err(OpError::invalid("scene", "name a scene from obs.scenes"));
        }
        if service.client().is_none() {
            return Ok(Plan::answer(json!({ "connected": false, "sources": [], "hint": "connect with obs.connect first" })));
        }
        let items = match service.request(&GetSceneItemList { scene_name: self.scene.clone() }) {
            Ok(items) => items,
            Err(ObsError::Refused { code: STATUS_RESOURCE_NOT_FOUND, .. }) => return Err(no_scene(service, &self.scene)),
            Err(e) => return Err(op_error(e)),
        };
        // OBS lists items bottom-up by index; a person reads top-down.
        let mut items: Vec<&Value> = items.scene_items.iter().collect();
        items.sort_by_key(|i| std::cmp::Reverse(i.get("sceneItemIndex").and_then(Value::as_i64).unwrap_or_default()));
        let sources: Vec<Value> = items
            .iter()
            .map(|i| {
                json!({
                    "id": i.get("sceneItemId").cloned().unwrap_or(Value::Null),
                    "name": i.get("sourceName").cloned().unwrap_or(Value::Null),
                    "visible": i.get("sceneItemEnabled").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        Ok(Plan::answer(json!({ "connected": true, "scene": self.scene, "sources": sources })))
    }
}

register_op!(Status);
register_op!(Scenes);
register_op!(Sources);
