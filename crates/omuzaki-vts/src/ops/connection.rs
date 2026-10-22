//! `vts.connect` and `vts.disconnect` (live actions under the `broadcast` scope) and `vts.status` (a query).

use std::time::Duration;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{START_HINT, live_action, query, service, synced_mapping};
use crate::client::{BridgeStatus, ClientConfig};

/// How long `vts.connect` waits for the bridge to settle by default.
pub const DEFAULT_WAIT_S: f64 = 3.0;
/// The longest `vts.connect` waits.
pub const MAX_WAIT_S: f64 = 30.0;

/// Connect to `VTube Studio` on this computer and stay connected: Omuzaki's channels drive the model's parameters and
/// mapped expressions run its hotkeys. The first time, VTS asks the person to allow Omuzaki.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "port": 8001 }, { "instance": "default", "wait_s": 10.0 }]))]
pub struct Connect {
    /// VTS's API port (Settings, `VTube Studio` Plugins). Default: found from VTS's broadcast, else 8001.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Which VTS on this computer, when more than one runs; its token is stored under it. Default `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    /// How long to wait for the connection to settle, in seconds (0 to 30). Default 3. The answer says where it got
    /// to; `vts.status` follows it after.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_s: Option<f64>,
}

/// Switch off what the bridge switched on in `VTube Studio`, close the connection and stop reconnecting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Disconnect {}

/// Report the bridge to `VTube Studio`: its state, VTS's version and frame rate, the loaded model, its hotkeys, the
/// parameters another plugin owns, the expressions the bridge switched on, and the loaded model's mapping.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

impl Connect {
    fn config(&self) -> ClientConfig {
        let base = ClientConfig::default();
        ClientConfig { port: self.port, instance: self.instance.clone().unwrap_or(base.instance), ..base }
    }

    fn wait(&self) -> Result<Duration, OpError> {
        let wait = self.wait_s.unwrap_or(DEFAULT_WAIT_S);
        if !(0.0..=MAX_WAIT_S).contains(&wait) {
            return Err(OpError::invalid("wait_s", format!("{wait} is outside 0 to {MAX_WAIT_S} seconds")));
        }
        Ok(Duration::from_secs_f64(wait))
    }
}

/// The status as an answer: the bridge's state plus `connected`.
pub(crate) fn status_answer(status: &BridgeStatus) -> Result<Value, OpError> {
    let mut answer = serde_json::to_value(status).map_err(|e| OpError::internal(e.to_string()))?;
    answer["connected"] = json!(matches!(status, BridgeStatus::Connected(_)));
    Ok(answer)
}

impl Op for Connect {
    fn meta() -> OpMeta {
        live_action("vts.connect", "Connect to VTube Studio on this computer, and stay connected.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let (config, wait) = (self.config(), self.wait()?);
        if config.instance.trim().is_empty() {
            return Err(OpError::invalid("instance", "an instance name, like default"));
        }
        let stored = service.token_stored(&config.instance);
        // The first connect on an instance shows VTS's popup, which only the person at VTS can answer: an agent asks
        // the person first. The same answer in a dry run, so an agent's plan shows the question.
        if !stored {
            ctx.require_person("connect VTube Studio: it asks you once to allow Omuzaki")?;
        }
        // The bridge starts with the project's mapping.
        synced_mapping(ctx, service)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({
                "applied": false,
                "port": config.port,
                "instance": config.instance,
                "token_stored": stored,
                "state": status_answer(&service.status())?["state"],
            })));
        }
        let status = service.connect(config, wait).map_err(|e| OpError::unsupported(e.to_string()).hint(START_HINT))?;
        match &status {
            BridgeStatus::Denied => {
                return Err(OpError::denied("the person denied Omuzaki in VTube Studio's popup")
                    .hint("run vts.connect again and click Allow in VTube Studio"));
            }
            BridgeStatus::Failed { reason } => {
                return Err(
                    OpError::unsupported(format!("VTube Studio could not be reached: {reason}")).hint(START_HINT)
                );
            }
            _ => {}
        }
        let mut answer = status_answer(&status)?;
        match status {
            BridgeStatus::AwaitingApproval => {
                answer["hint"] = json!("the person clicks Allow in VTube Studio's popup; vts.status follows it");
            }
            BridgeStatus::Connecting { .. } => {
                answer["hint"] = json!(format!("still trying; vts.status follows it. If it stays here: {START_HINT}"));
            }
            _ => {}
        }
        Ok(Plan::answer(answer))
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        live_action("vts.disconnect", "Switch off what Omuzaki switched on in VTube Studio and close the connection.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            let state = status_answer(&service.status())?;
            return Ok(Plan::answer(json!({ "applied": false, "connected": state["connected"] })));
        }
        let (was, state) = service.disconnect(Duration::from_secs(2));
        Ok(Plan::answer(json!({ "disconnected": was, "state": status_answer(&state)?["state"] })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        query("vts.status", "Report the bridge to VTube Studio: state, model, hotkeys, conflicts and mapping.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let status = service.status();
        let mut answer = status_answer(&status)?;
        if let Some(config) = service.config() {
            answer["port"] = json!(config.port);
            answer["instance"] = json!(config.instance);
        }
        answer["parameters"] = json!(service.channel_map().params().count());
        if let BridgeStatus::Connected(c) = &status
            && let Some(model) = &c.model
        {
            let mapping = synced_mapping(ctx, service)?;
            answer["mapping"] = json!(mapping.models.get(&model.id).cloned().unwrap_or_default());
        }
        Ok(Plan::answer(answer))
    }
}

register_op!(Connect);
register_op!(Disconnect);
register_op!(Status);
