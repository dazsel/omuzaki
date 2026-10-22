//! `obs.connect`, `obs.disconnect`, `obs.status`, `obs.scenes`, `obs.set_scene`, `obs.stream` and `obs.record`: OBS
//! Studio controlled as ops (spec v3 BR-19), so a person, a script or an agent can switch scenes and go live in OBS.
//!
//! Everything that changes OBS is a **live action** (kind `Edit`, `Undo::None`, scope `perform`), like the output and
//! record ops: it acts on another app and on the audience, never on the document. The password is never an argument:
//! `obs.connect` names a secret label (default `obs/websocket`) and reads the value from the secret store inside its
//! run, so no value reaches an op record, a log or MCP output. A dry run says whether the action would work now.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::client::ObsError;
use crate::limits::DEFAULT_PORT;
use crate::protocol::{
    GetCurrentProgramScene, GetRecordStatus, GetSceneList, GetStreamStatus, SetCurrentProgramScene, StartRecord,
    StartStream, StopRecord, StopStream,
};
use crate::service::{DEFAULT_HOST, ObsService};

/// The secret label `obs.connect` reads the password from when none is named.
pub const DEFAULT_PASSWORD_LABEL: &str = "obs/websocket";

/// Connect to OBS Studio's remote-control server (Tools → WebSocket Server Settings in OBS).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "port": 4455 }, { "host": "127.0.0.1", "password": "obs/websocket" }]))]
pub struct Connect {
    /// The machine OBS runs on. Default `127.0.0.1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The server's port. Default 4455.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The label of the stored secret holding the server password. Default `obs/websocket`; ignored when OBS needs no
    /// password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// Close the connection to OBS.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Disconnect {}

/// Report the connection to OBS and, when connected, its program scene, stream and recording.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// List OBS's scenes and the one on program.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Scenes {}

/// Switch OBS's program to a scene.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "scene": "Main" }]))]
pub struct SetScene {
    /// The scene's name, as `obs.scenes` lists it.
    pub scene: String,
}

/// Start or stop.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Start it.
    #[default]
    Start,
    /// Stop it.
    Stop,
}

/// Start or stop OBS's stream (going live with OBS's own stream settings).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "action": "start" }, { "action": "stop" }]))]
pub struct Stream {
    /// `start` or `stop`.
    pub action: Action,
}

/// Start or stop OBS's own recording.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "action": "start" }, { "action": "stop" }]))]
pub struct Record {
    /// `start` or `stop`.
    pub action: Action,
}

/// Maps a connection error to the op error a person or an agent can act on.
#[allow(clippy::needless_pass_by_value)] // Used as `map_err(op_error)`.
fn op_error(e: ObsError) -> OpError {
    match e {
        ObsError::Unreachable { .. } => OpError::unsupported(e.to_string())
            .hint("start OBS and turn on Tools > WebSocket Server Settings > Enable WebSocket server"),
        ObsError::Auth(_) => OpError::denied(e.to_string())
            .hint("store the server password (OBS shows it in WebSocket Server Settings) under the secret label obs/websocket"),
        ObsError::Closed(_) => OpError::conflict(e.to_string()).hint("connect with obs.connect"),
        ObsError::Refused { .. } => OpError::conflict(e.to_string()),
        ObsError::Timeout(_) | ObsError::Protocol(_) => OpError::internal(e.to_string()),
    }
}

fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a ObsService, OpError> {
    ctx.service::<ObsService>()
}

/// The answer of a live action's dry run: whether it would reach OBS now.
fn would(service: &ObsService, what: &Value) -> Plan {
    let connected = service.client().is_some();
    let mut answer = json!({ "applied": false, "connected": connected });
    if let (Some(a), Some(w)) = (answer.as_object_mut(), what.as_object()) {
        a.extend(w.clone());
    }
    if !connected {
        answer["hint"] = json!("connect with obs.connect first");
    }
    Plan::answer(answer)
}

fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Perform, undo: Undo::None }
}

fn query(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Query, scope: Scope::Read, undo: Undo::None }
}

impl Op for Connect {
    fn meta() -> OpMeta {
        live_action("obs.connect", "Connect to OBS Studio's WebSocket server, with the password from the secret store.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let host = self.host.clone().unwrap_or_else(|| DEFAULT_HOST.to_owned());
        let port = self.port.unwrap_or(DEFAULT_PORT);
        let label = SecretLabel::new(self.password.as_deref().unwrap_or(DEFAULT_PASSWORD_LABEL))
            .map_err(|e| OpError::invalid("password", e.to_string()))?;
        let store = ctx.service::<dyn SecretStore>().ok();
        if ctx.dry_run {
            let stored = store.and_then(|s| s.contains(&label).ok()).unwrap_or(false);
            return Ok(Plan::answer(json!({
                "connected": false,
                "host": host,
                "port": port,
                "password_label": label.to_string(),
                "password_stored": stored,
            })));
        }
        let secret = match store {
            Some(s) => s.get(&label).map_err(|e| OpError::internal(e.to_string()))?,
            None => None,
        };
        let client = service
            .connect(&host, port, secret.as_ref().map(omuzaki_secrets_api::value::SecretValue::expose))
            .map_err(op_error)?;
        Ok(Plan::answer(json!({
            "connected": true,
            "host": host,
            "port": port,
            "websocket_version": client.info().websocket_version,
            "rpc_version": client.info().rpc_version,
        })))
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        live_action("obs.disconnect", "Close the connection to OBS Studio.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "disconnected": false, "connected": service.client().is_some() })));
        }
        Ok(Plan::answer(json!({ "disconnected": service.disconnect() })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        query("obs.status", "Report the connection to OBS and its program scene, stream and recording.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let Some(client) = service.client() else {
            let reason = service.last().and_then(|c| c.close_reason());
            return Ok(Plan::answer(json!({ "connected": false, "closed_because": reason })));
        };
        let scene = client.request(&GetCurrentProgramScene {}).map_err(op_error)?;
        let stream = client.request(&GetStreamStatus {}).map_err(op_error)?;
        let record = client.request(&GetRecordStatus {}).map_err(op_error)?;
        Ok(Plan::answer(json!({
            "connected": true,
            "addr": client.addr(),
            "websocket_version": client.info().websocket_version,
            "program_scene": scene.scene_name,
            "streaming": stream.output_active,
            "stream_reconnecting": stream.output_reconnecting,
            "stream_duration_ms": stream.output_duration,
            "stream_skipped_frames": stream.output_skipped_frames,
            "recording": record.output_active,
            "record_duration_ms": record.output_duration,
        })))
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
        let list = service.request(&GetSceneList {}).map_err(op_error)?;
        // OBS lists scenes bottom-up; the person sees them top-down, so the answer reverses them.
        let names: Vec<&str> =
            list.scenes.iter().rev().filter_map(|s| s.get("sceneName").and_then(Value::as_str)).collect();
        Ok(Plan::answer(json!({ "connected": true, "program": list.current_program_scene_name, "scenes": names })))
    }
}

impl Op for SetScene {
    fn meta() -> OpMeta {
        live_action("obs.set_scene", "Switch OBS's program to a scene.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if self.scene.trim().is_empty() {
            return Err(OpError::invalid("scene", "name a scene from obs.scenes"));
        }
        if ctx.dry_run {
            return Ok(would(service, &json!({ "scene": self.scene })));
        }
        service.request(&SetCurrentProgramScene { scene_name: self.scene.clone() }).map_err(op_error)?;
        Ok(Plan::answer(json!({ "applied": true, "scene": self.scene })))
    }
}

impl Op for Stream {
    fn meta() -> OpMeta {
        live_action("obs.stream", "Start or stop OBS's stream (go live or end the stream).")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(would(service, &json!({ "action": self.action })));
        }
        match self.action {
            Action::Start => service.request(&StartStream {}),
            Action::Stop => service.request(&StopStream {}),
        }
        .map_err(op_error)?;
        Ok(Plan::answer(json!({ "applied": true, "action": self.action })))
    }
}

impl Op for Record {
    fn meta() -> OpMeta {
        live_action("obs.record", "Start or stop OBS's own recording.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(would(service, &json!({ "action": self.action })));
        }
        let path = match self.action {
            Action::Start => service.request(&StartRecord {}).map(|_| None),
            Action::Stop => service.request(&StopRecord {}).map(|r| Some(r.output_path)),
        }
        .map_err(op_error)?;
        Ok(Plan::answer(json!({ "applied": true, "action": self.action, "file": path })))
    }
}

register_op!(Connect);
register_op!(Disconnect);
register_op!(Status);
register_op!(Scenes);
register_op!(SetScene);
register_op!(Stream);
register_op!(Record);
