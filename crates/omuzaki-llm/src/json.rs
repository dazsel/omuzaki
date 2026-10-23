//! One JSON request and its JSON answer, over the same HTTP stack as the chat providers: for a service that is not a
//! chat model but answers a structured question, such as the performer's hosted decision provider (#2348).
//!
//! The rules are the chat providers': plain http only on loopback, the proxy bypassed there, no redirects (a redirect
//! would carry the key to another host), a host off this machine only with `allow_remote`, and the key sent as a bearer
//! token and redacted from every error.

use std::time::Duration;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{RuntimeHandle, SecretValue};
use serde_json::Value;
use tokio::runtime::Handle;

use crate::engine::{redact_error, short};
use crate::http::{Endpoint, HeaderValue, Http, Request, bearer, fetch_metadata};
use crate::runtime::{block_on, handle_of};

/// A client for one JSON endpoint.
pub struct JsonClient {
    endpoint: Endpoint,
    http: Http,
    handle: Handle,
    // Keeps a runtime this crate started alive as long as the client.
    _runtime: RuntimeHandle,
    key: Option<SecretValue>,
    limit: Duration,
}

impl JsonClient {
    /// A client for `endpoint`, sending `key` as a bearer token, each request bounded by `limit`.
    ///
    /// # Errors
    /// `Unsupported` for a bad endpoint, a host off this machine without `allow_remote`, or a runtime that is not this
    /// crate's.
    pub fn new(
        endpoint: &str,
        key: Option<SecretValue>,
        allow_remote: bool,
        runtime: &RuntimeHandle,
        limit: Duration,
    ) -> Result<Self, LlmError> {
        let endpoint = Endpoint::parse(endpoint)?;
        if !endpoint.loopback && !allow_remote {
            return Err(LlmError::Unsupported(
                "the endpoint is not this machine, and sending there was not allowed (allow_remote)".into(),
            ));
        }
        let http = Http::new(&endpoint)?;
        let handle = handle_of(runtime)?;
        Ok(Self { endpoint, http, handle, _runtime: runtime.clone(), key, limit })
    }

    /// Whether the endpoint is this machine.
    pub fn is_local(&self) -> bool {
        self.endpoint.loopback
    }

    /// Posts `body` to `path` below the endpoint and waits for the answer on the calling thread.
    ///
    /// # Errors
    /// `Auth` for 401 or 403, `RateLimited` for 429, `Overloaded` for 503 or 529, `Network` for no answer in time or
    /// another status (with the server's message, the key redacted), `Malformed` for an answer that isn't JSON.
    pub fn post(&self, path: &str, body: &Value) -> Result<Value, LlmError> {
        let bytes = serde_json::to_vec(body).map_err(|e| LlmError::Malformed(e.to_string()))?;
        let mut headers = vec![
            ("accept", HeaderValue::Plain("application/json".into())),
            ("content-type", HeaderValue::Plain("application/json".into())),
        ];
        headers.extend(self.key.as_ref().map(bearer));
        let request = Request::post(self.endpoint.url(path), headers, bytes);
        let (http, limit, key) = (self.http.clone(), self.limit, self.key.clone());
        block_on(&self.handle, async move {
            let (status, answer) = fetch_metadata(&http, request, limit, key.as_ref()).await?;
            let text = String::from_utf8_lossy(&answer);
            let error = match status {
                200..=299 => {
                    return serde_json::from_slice(&answer)
                        .map_err(|e| LlmError::Malformed(format!("the answer is not JSON: {e}")));
                }
                401 | 403 => LlmError::Auth,
                429 => LlmError::rate_limited(None),
                503 | 529 => LlmError::Overloaded,
                _ => LlmError::Network(format!("the server answered {status}: {}", short(&text, key.as_ref()))),
            };
            Err(redact_error(error, key.as_ref()))
        })?
    }
}

impl std::fmt::Debug for JsonClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JsonClient")
            .field("local", &self.endpoint.loopback)
            .field("key", &self.key.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}
