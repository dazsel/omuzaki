//! `twitch.connect`, `twitch.disconnect` and `twitch.status`: the Twitch connection as ops (spec v3 BR-10), so a
//! person, a script or an agent can bring a channel's redeems, chat, follows, raids, cheers and subs into the show.
//!
//! Connecting is a live action (kind `Edit`, `Undo::None`, scope `broadcast`): it changes what reaches the show, not
//! the document. The token is never an argument: `twitch.connect` names a secret label (default `twitch/token`) and
//! reads the value inside its run, so no token reaches an op record, a log or MCP output. The token is a user access
//! token for the broadcaster's account, with the scopes for the events wanted (`twitch.status` lists any it lacks).

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::eventsub::TOPICS;
use crate::service::TwitchService;

/// The secret label `twitch.connect` reads the token from when none is named.
pub const DEFAULT_TOKEN_LABEL: &str = "twitch/token";

/// Connect to Twitch with the token in the secret store, and bring the channel's events into the show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "token": "twitch/token" }]))]
pub struct Connect {
    /// The label of the stored secret holding the user access token. Default `twitch/token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
}

/// Close the connection to Twitch.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Disconnect {}

/// Report the connection to Twitch: the account, the events coming in, the ones the token's scopes don't allow.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a TwitchService, OpError> {
    ctx.service::<TwitchService>()
}

fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Broadcast, undo: Undo::None }
}

impl Op for Connect {
    fn meta() -> OpMeta {
        live_action(
            "twitch.connect",
            "Connect to Twitch with the stored token and bring the channel's events into the show.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let label = SecretLabel::new(self.token.as_deref().unwrap_or(DEFAULT_TOKEN_LABEL))
            .map_err(|e| OpError::invalid("token", e.to_string()))?;
        let store = ctx.service::<dyn SecretStore>().ok();
        if ctx.dry_run {
            let stored = store.and_then(|s| s.contains(&label).ok()).unwrap_or(false);
            let events: Vec<&str> = TOPICS.iter().map(|t| t.event).collect();
            return Ok(Plan::answer(json!({
                "applied": false,
                "token_label": label.to_string(),
                "token_stored": stored,
                "events": events,
            })));
        }
        let token = match store {
            Some(s) => s.get(&label).map_err(|e| OpError::internal(e.to_string()))?,
            None => None,
        }
        .ok_or_else(|| {
            OpError::not_found(format!("no Twitch token is stored under {label}"))
                .hint("store a user access token for your channel under the secret label twitch/token")
        })?;
        service.connect(token).map_err(|e| OpError::unsupported(e.to_string()))?;
        Ok(Plan::answer(json!({ "connecting": true, "token_label": label.to_string() })))
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        live_action("twitch.disconnect", "Close the connection to Twitch.")
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        if ctx.dry_run {
            return Ok(Plan::answer(json!({ "applied": false, "connected": service.status().is_some() })));
        }
        Ok(Plan::answer(json!({ "disconnected": service.disconnect() })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "twitch.status",
            summary: "Report the Twitch connection: the account, the events coming in and what the token lacks.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let answer = match service(ctx)?.status() {
            Some(status) => serde_json::to_value(status).map_err(|e| OpError::internal(e.to_string()))?,
            None => json!({ "connected": false }),
        };
        Ok(Plan::answer(answer))
    }
}

register_op!(Connect);
register_op!(Disconnect);
register_op!(Status);
