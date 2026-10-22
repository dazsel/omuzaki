//! The two Twitch HTTP calls the adapter makes, through the app's one HTTP client (`HttpTransport`): validating the
//! token (which says whose it is, which app issued it and its scopes) and creating an `EventSub` subscription on a
//! WebSocket session.
//!
//! The token only ever goes into an `Authorization` header; no error, log or status line carries it.

use std::sync::Arc;

use omuzaki_http_api::{HttpBody, HttpRequest, HttpTransport, Method, NeverCancel};
use serde::Deserialize;
use serde_json::json;

use crate::error::TwitchError;
use crate::eventsub::Topic;

/// The seconds a call may take.
const TIMEOUT_S: u32 = 10;
/// The most bytes of an answer read.
const MAX_ANSWER: u64 = 64 * 1024;

/// What the token validation says about a token.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct TokenInfo {
    /// The app that issued the token: the `Client-Id` Helix calls send.
    pub client_id: String,
    /// The account's login.
    #[serde(default)]
    pub login: String,
    /// The account's user id: the broadcaster the subscriptions name.
    #[serde(default)]
    pub user_id: String,
    /// The scopes the token holds.
    #[serde(default)]
    pub scopes: Vec<String>,
    /// Seconds until it expires.
    #[serde(default)]
    pub expires_in: u64,
}

/// Helix and the OAuth service, at their addresses (Twitch's own, or a test server's).
#[derive(Clone)]
pub struct Helix {
    http: Arc<dyn HttpTransport>,
    api: String,
    oauth: String,
}

impl std::fmt::Debug for Helix {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Helix").field("api", &self.api).field("oauth", &self.oauth).finish_non_exhaustive()
    }
}

impl Helix {
    /// Helix at `api` and the OAuth service at `oauth`, called through `http`.
    pub fn new(http: Arc<dyn HttpTransport>, api: impl Into<String>, oauth: impl Into<String>) -> Self {
        Self { http, api: api.into(), oauth: oauth.into() }
    }

    fn call(
        &self,
        method: Method,
        url: String,
        headers: Vec<(String, String)>,
        body: HttpBody,
    ) -> Result<(u16, Vec<u8>), TwitchError> {
        let request = HttpRequest { method, url, headers, body, timeout_s: TIMEOUT_S, max_response: MAX_ANSWER };
        let response = self.http.send(&request, &NeverCancel).map_err(|e| TwitchError::Network(e.to_string()))?;
        Ok((response.status, response.body))
    }

    /// Validates `token`: whose it is, its app and its scopes.
    ///
    /// # Errors
    /// [`TwitchError::Token`] when Twitch refuses it (expired, revoked); [`TwitchError::Network`] or
    /// [`TwitchError::Protocol`] otherwise.
    pub fn validate(&self, token: &str) -> Result<TokenInfo, TwitchError> {
        let headers = vec![("Authorization".to_owned(), format!("OAuth {token}"))];
        let (status, body) = self.call(Method::Get, format!("{}/validate", self.oauth), headers, HttpBody::Empty)?;
        match status {
            200 => serde_json::from_slice(&body)
                .map_err(|e| TwitchError::Protocol(format!("the token check answered oddly: {e}"))),
            401 => Err(TwitchError::Token("Twitch refused the token (expired or revoked)".to_owned())),
            other => Err(TwitchError::Protocol(format!("the token check answered HTTP {other}"))),
        }
    }

    /// Subscribes the WebSocket `session` to `topic` for the broadcaster `user`.
    ///
    /// # Errors
    /// [`TwitchError::Refused`] with Twitch's own reason when it refuses (a missing scope, a duplicate);
    /// [`TwitchError::Network`] otherwise.
    pub fn subscribe(&self, token: &str, info: &TokenInfo, topic: &Topic, session: &str) -> Result<(), TwitchError> {
        let headers = vec![
            ("Authorization".to_owned(), format!("Bearer {token}")),
            ("Client-Id".to_owned(), info.client_id.clone()),
        ];
        let body = json!({
            "type": topic.kind,
            "version": topic.version,
            "condition": topic.condition(&info.user_id),
            "transport": { "method": "websocket", "session_id": session },
        });
        let url = format!("{}/eventsub/subscriptions", self.api);
        let (status, answer) = self.call(Method::Post, url, headers, HttpBody::Json(body))?;
        if (200..300).contains(&status) {
            return Ok(());
        }
        let reason = serde_json::from_slice::<serde_json::Value>(&answer)
            .ok()
            .and_then(|v| v["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| format!("HTTP {status}"));
        Err(TwitchError::Refused { kind: topic.kind.to_owned(), reason })
    }
}
