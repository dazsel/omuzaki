//! `youtube.connect`, `youtube.disconnect` and `youtube.status`: the YouTube connection as ops (spec v3 BR-10), so a
//! person, a script or an agent can bring a live stream's chat, Super Chats, memberships and new subscribers into the
//! show.
//!
//! Connecting is a live action (kind `Edit`, `Undo::None`, scope `broadcast`): it changes what reaches the show, not
//! the document. The token is never an argument: `youtube.connect` names a secret label (default `youtube/token`) and
//! reads the value inside its run, so no token reaches an op record, a log or MCP output. The token is an OAuth access
//! token for the channel's Google account with the `youtube.readonly` scope.

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_secrets_api::prelude::{SecretLabel, SecretStore};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::service::YouTubeService;

/// The secret label `youtube.connect` reads the token from when none is named.
pub const DEFAULT_TOKEN_LABEL: &str = "youtube/token";
/// The events the adapter brings in.
pub const EVENTS: [&str; 5] = ["chat.message", "tip", "sub", "sub_gift", "follow"];

/// Connect to the channel's live stream on YouTube with the stored token, and bring its chat and events into the show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "video": "dQw4w9WgXcQ" }, { "token": "youtube/token" }]))]
pub struct Connect {
    /// The label of the stored secret holding the OAuth access token. Default `youtube/token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// A live video's id, to read its chat; default the channel's own live broadcast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub video: Option<String>,
}

/// Stop reading YouTube.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Disconnect {}

/// Report the YouTube connection: the broadcast, the events coming in and the quota spent.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

fn service<'a>(ctx: &OpCtx<'a>) -> Result<&'a YouTubeService, OpError> {
    ctx.service::<YouTubeService>()
}

fn live_action(name: &'static str, summary: &'static str) -> OpMeta {
    OpMeta { name, summary, kind: OpKind::Edit, scope: Scope::Broadcast, undo: Undo::None }
}

impl Op for Connect {
    fn meta() -> OpMeta {
        live_action(
            "youtube.connect",
            "Connect to the channel's live stream on YouTube and bring its chat, Super Chats and members into the show.",
        )
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let service = service(ctx)?;
        let label = SecretLabel::new(self.token.as_deref().unwrap_or(DEFAULT_TOKEN_LABEL))
            .map_err(|e| OpError::invalid("token", e.to_string()))?;
        // Only YouTube's own secrets: the value goes to Google in a header, so another service's key never does.
        if !label.as_str().starts_with("youtube/") {
            return Err(OpError::invalid(
                "token",
                format!("{label} is not a YouTube secret; YouTube's labels start youtube/"),
            ));
        }
        if let Some(video) = &self.video
            && (video.is_empty()
                || video.len() > 64
                || !video.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
        {
            return Err(OpError::invalid("video", "a YouTube video id: letters, digits, - and _"));
        }
        let store = ctx.service::<dyn SecretStore>().ok();
        if ctx.dry_run {
            let stored = store.and_then(|s| s.contains(&label).ok()).unwrap_or(false);
            return Ok(Plan::answer(json!({
                "applied": false,
                "token_label": label.to_string(),
                "token_stored": stored,
                "events": EVENTS,
            })));
        }
        let token = match store {
            Some(s) => s.get(&label).map_err(|e| OpError::internal(e.to_string()))?,
            None => None,
        }
        .ok_or_else(|| {
            OpError::not_found(format!("no YouTube token is stored under {label}")).hint(
                "store an OAuth access token with the youtube.readonly scope under the secret label youtube/token",
            )
        })?;
        service.connect(token, self.video.clone()).map_err(|e| OpError::unsupported(e.to_string()))?;
        Ok(Plan::answer(json!({ "connecting": true, "token_label": label.to_string() })))
    }
}

impl Op for Disconnect {
    fn meta() -> OpMeta {
        live_action("youtube.disconnect", "Stop reading YouTube's live chat.")
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
            name: "youtube.status",
            summary: "Report the YouTube connection: the broadcast, the events coming in and the quota spent today.",
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
