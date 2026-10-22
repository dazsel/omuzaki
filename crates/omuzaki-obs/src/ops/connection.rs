//! `obs.connect` and `obs.disconnect`: live actions under the `broadcast` scope.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpMeta, Plan, register_op};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use omuzaki_settings_api::service::{Settings, SettingsExt as _};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{live_action, op_error, service};
use crate::client::ClientConfig;
use crate::settings;

/// The secret label `obs.connect` reads the password from when none is named.
pub const DEFAULT_PASSWORD_LABEL: &str = "obs/websocket";

/// Connect to OBS Studio's remote-control server (Tools → WebSocket Server Settings in OBS), and stay connected.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "port": 4455 }, { "host": "127.0.0.1", "password": "obs/websocket" }]))]
pub struct Connect {
    /// The machine OBS runs on. Default: the `obs.host` setting (`127.0.0.1`). Another computer needs a person to
    /// allow remote links (`obs.allow_remote`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// The server's port. Default: the `obs.port` setting (4455).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The label of the stored secret holding the server password. Default `obs/websocket`; ignored when OBS needs no
    /// password.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// Close the connection to OBS and stop reconnecting.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Disconnect {}

impl Connect {
    /// Where to connect: the arguments, then the settings, then the defaults.
    fn config(&self, ctx: &OpCtx<'_>) -> ClientConfig {
        let settings = ctx.service::<dyn Settings>().ok();
        let host = self.host.clone().unwrap_or_else(|| {
            settings.and_then(|s| s.get::<settings::Host>().ok()).unwrap_or_else(<settings::Host as omuzaki_settings_api::setting::Setting>::default)
        });
        let port = self.port.unwrap_or_else(|| {
            settings.and_then(|s| s.get::<settings::Port>().ok()).unwrap_or(crate::limits::DEFAULT_PORT)
        });
        let allow_remote = settings.and_then(|s| s.get::<settings::AllowRemote>().ok()).unwrap_or(false);
        ClientConfig { allow_remote, ..ClientConfig::at(&host, port) }
    }
}

impl Op for Connect {
    fn meta() -> OpMeta {
        live_action("obs.connect", "Connect to OBS Studio's WebSocket server, with the password from the secret store.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let config = self.config(ctx);
        let label = SecretLabel::new(self.password.as_deref().unwrap_or(DEFAULT_PASSWORD_LABEL))
            .map_err(|e| OpError::invalid("password", e.to_string()))?;
        let store = ctx.service::<dyn SecretStore>().ok();
        if ctx.dry_run {
            let stored = store.and_then(|s| s.contains(&label).ok()).unwrap_or(false);
            return Ok(Plan::answer(json!({
                "applied": false,
                "host": config.host,
                "port": config.port,
                "remote": !config.is_loopback(),
                "remote_allowed": config.allow_remote,
                "password_label": label.to_string(),
                "password_stored": stored,
            })));
        }
        let secret = match store {
            Some(s) => s.get(&label).map_err(|e| OpError::internal(e.to_string()))?,
            None => None,
        };
        let client = service.connect(config.clone(), secret).map_err(op_error)?;
        Ok(Plan::answer(json!({
            "connected": true,
            "host": config.host,
            "port": config.port,
            "unencrypted": client.unencrypted(),
            "websocket_version": client.info().websocket_version,
            "rpc_version": client.info().rpc_version,
        })))
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        live_action("obs.disconnect", "Close the connection to OBS Studio and stop reconnecting.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "connected": service.client().is_some() })));
        }
        Ok(Plan::answer(json!({ "disconnected": service.disconnect() })))
    }
}

register_op!(Connect);
register_op!(Disconnect);
