//! The YouTube Data API calls the adapter makes, through the app's one HTTP client, and the quota each costs.
//!
//! The token only ever goes into an `Authorization` header; no error, log or status line carries it.

use std::sync::Arc;

use omuzaki_http_api::{HttpBody, HttpRequest, HttpTransport, Method, NeverCancel};
use serde_json::Value;

use crate::chat::{Chat, Page};
use crate::error::YouTubeError;

/// YouTube's own API address.
pub const API_URL: &str = "https://www.googleapis.com/youtube/v3";
/// The seconds a call may take.
const TIMEOUT_S: u32 = 15;
/// The most bytes of an answer read (a page of 2,000 messages fits).
const MAX_ANSWER: u64 = 4 * 1024 * 1024;

/// Quota units per call, from YouTube's published costs: a read of a list is 1 unit; a live chat page is 5.
pub mod cost {
    /// `liveBroadcasts.list`, `videos.list`, `subscriptions.list`.
    pub const LIST: u64 = 1;
    /// `liveChatMessages.list`.
    pub const CHAT_PAGE: u64 = 5;
    /// A project's default daily quota.
    pub const DAILY_DEFAULT: u64 = 10_000;
}

/// The YouTube Data API at an address (YouTube's own, or a test server's).
#[derive(Clone)]
pub struct Api {
    http: Arc<dyn HttpTransport>,
    base: String,
}

impl std::fmt::Debug for Api {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Api").field("base", &self.base).finish_non_exhaustive()
    }
}

/// Percent-encodes a query value (ids and page tokens are URL-safe already, but a person types the video id).
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

impl Api {
    /// The API at `base`, called through `http`.
    pub fn new(http: Arc<dyn HttpTransport>, base: impl Into<String>) -> Self {
        Self { http, base: base.into() }
    }

    fn get(&self, token: &str, path_and_query: &str) -> Result<Value, YouTubeError> {
        let request = HttpRequest {
            method: Method::Get,
            url: format!("{}/{path_and_query}", self.base),
            headers: vec![("Authorization".to_owned(), format!("Bearer {token}"))],
            body: HttpBody::Empty,
            timeout_s: TIMEOUT_S,
            max_response: MAX_ANSWER,
        };
        let response = self.http.send(&request, &NeverCancel).map_err(|e| YouTubeError::Network(e.to_string()))?;
        let body: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        match response.status {
            200..=299 => Ok(body),
            401 => Err(YouTubeError::Token("YouTube refused the token (expired or revoked)".to_owned())),
            403 | 429 => {
                let reason = body.pointer("/error/errors/0/reason").and_then(Value::as_str).unwrap_or_default();
                if reason.contains("quota") || reason == "rateLimitExceeded" {
                    Err(YouTubeError::Quota)
                } else if reason == "liveChatEnded" || reason == "liveChatDisabled" {
                    Err(YouTubeError::Ended)
                } else {
                    let message = body.pointer("/error/message").and_then(Value::as_str).unwrap_or("forbidden");
                    Err(YouTubeError::Refused(message.to_owned()))
                }
            }
            404 => Err(YouTubeError::Ended),
            other => Err(YouTubeError::Protocol(format!("YouTube answered HTTP {other}"))),
        }
    }

    /// The live chat of the token's channel's active broadcast, or of `video` when one is named.
    ///
    /// # Errors
    /// [`YouTubeError::NotLive`] when nothing is live; the call's own errors otherwise.
    pub fn find_chat(&self, token: &str, video: Option<&str>) -> Result<Chat, YouTubeError> {
        if let Some(video) = video {
            let answer =
                self.get(token, &format!("videos?part=snippet%2CliveStreamingDetails&id={}", encode(video)))?;
            let item = answer.pointer("/items/0").ok_or(YouTubeError::NotLive)?;
            let id = item
                .pointer("/liveStreamingDetails/activeLiveChatId")
                .and_then(Value::as_str)
                .ok_or(YouTubeError::NotLive)?;
            return Ok(Chat {
                id: id.to_owned(),
                channel_id: item.pointer("/snippet/channelId").and_then(Value::as_str).unwrap_or_default().to_owned(),
                title: item.pointer("/snippet/title").and_then(Value::as_str).unwrap_or_default().to_owned(),
            });
        }
        let answer =
            self.get(token, "liveBroadcasts?part=snippet&broadcastStatus=active&broadcastType=all&maxResults=5")?;
        let items = answer.get("items").and_then(Value::as_array).cloned().unwrap_or_default();
        items
            .iter()
            .find_map(|item| {
                let id = item.pointer("/snippet/liveChatId").and_then(Value::as_str)?;
                Some(Chat {
                    id: id.to_owned(),
                    channel_id: item
                        .pointer("/snippet/channelId")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_owned(),
                    title: item.pointer("/snippet/title").and_then(Value::as_str).unwrap_or_default().to_owned(),
                })
            })
            .ok_or(YouTubeError::NotLive)
    }

    /// A page of `chat`'s messages after `page` (from the start of what YouTube keeps when `None`).
    ///
    /// # Errors
    /// The call's errors; [`YouTubeError::Protocol`] for a page that doesn't parse.
    pub fn messages(&self, token: &str, chat: &str, page: Option<&str>) -> Result<Page, YouTubeError> {
        let mut query =
            format!("liveChat/messages?liveChatId={}&part=snippet%2CauthorDetails&maxResults=2000", encode(chat));
        if let Some(page) = page {
            query.push_str("&pageToken=");
            query.push_str(&encode(page));
        }
        let answer = self.get(token, &query)?;
        serde_json::from_value(answer).map_err(|e| YouTubeError::Protocol(format!("a chat page didn't parse: {e}")))
    }

    /// The channel's most recent subscribers (only those who made their subscription public).
    ///
    /// # Errors
    /// The call's errors.
    pub fn recent_subscribers(&self, token: &str) -> Result<Vec<Value>, YouTubeError> {
        let answer = self.get(token, "subscriptions?part=subscriberSnippet&myRecentSubscribers=true&maxResults=50")?;
        Ok(answer.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn query_values_are_encoded() {
        assert_eq!(super::encode("abc-_.~9"), "abc-_.~9");
        assert_eq!(super::encode("a b&c"), "a%20b%26c");
    }
}
