//! What an OpenAI-compatible server and model can do (llm-api D6): the server's own metadata first, then two free
//! local probes. The result is cached per endpoint and model.
//!
//! Sources, read on 2026-10-15:
//! - Ollama `POST /api/show` (`{"model": ...}`): the `capabilities` array (`completion`, `vision`, and `tools` where
//!   the model has it; the docs' example shows the first two) and `model_info["<arch>.context_length"]`, from
//!   github.com/ollama/ollama docs/api.md. The `tools` value is not in that example: **unverified**, so a model whose
//!   array lacks it is probed rather than trusted.
//! - llama.cpp `GET /props`: `default_generation_settings.n_ctx` and `modalities.vision`, from the `llama-server`
//!   README (github.com/ggml-org/llama.cpp, tools/server/README.md).
//! - vLLM `max_model_len` in `GET /v1/models`: **unverified**. The vLLM documentation could not be reached on
//!   2026-10-15, so the field is read when present and nothing relies on it: a missing value falls through to the
//!   override or the 8,192 default.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::SecretValue;
use serde_json::Value;

use super::wire::probe_body;
use crate::http::{Endpoint, HeaderValue, Http, Request};

/// The context window assumed when neither the server nor the configuration says (llm-api D6).
pub(crate) const FALLBACK_CONTEXT: u64 = 8_192;
/// The largest metadata answer that is read.
const MAX_METADATA: usize = 4 * 1024 * 1024;

/// What was found out about one model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Capabilities {
    /// The context window the server reported, if it did.
    pub context_window: Option<u64>,
    pub tools: bool,
    pub vision: bool,
}

type Cache = Mutex<HashMap<(String, String), Capabilities>>;

fn cache() -> &'static Cache {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    CACHE.get_or_init(Cache::default)
}

/// The cached answer for `model` at `endpoint`.
pub(crate) fn cached(endpoint: &Endpoint, model: &str) -> Option<Capabilities> {
    cache().lock().unwrap_or_else(PoisonError::into_inner).get(&(endpoint.base.clone(), model.to_owned())).copied()
}

fn remember(endpoint: &Endpoint, model: &str, found: Capabilities) {
    cache().lock().unwrap_or_else(PoisonError::into_inner).insert((endpoint.base.clone(), model.to_owned()), found);
}

/// A server and key to ask.
struct Ask<'a> {
    http: &'a Http,
    endpoint: &'a Endpoint,
    key: Option<&'a SecretValue>,
}

impl Ask<'_> {
    fn headers(&self) -> Vec<(&'static str, HeaderValue)> {
        let mut headers = vec![("content-type", HeaderValue::Plain("application/json".into()))];
        if let Some(key) = self.key {
            headers.push(("authorization", HeaderValue::Secret(SecretValue::new(format!("Bearer {}", key.expose())))));
        }
        headers
    }

    /// The status and body of `request`.
    async fn run(&self, request: Request) -> Result<(u16, Vec<u8>), LlmError> {
        let response = self.http.send(request).await.map_err(LlmError::from)?;
        let status = response.status;
        let body = response.read_limited(MAX_METADATA).await.map_err(LlmError::from)?;
        if matches!(status, 401 | 403) {
            return Err(LlmError::Auth);
        }
        Ok((status, body))
    }

    /// The JSON a metadata endpoint answers with, or `None` if this server does not have the endpoint.
    async fn json(&self, request: Request) -> Result<Option<Value>, LlmError> {
        let (status, body) = self.run(request).await?;
        Ok((status == 200).then(|| serde_json::from_slice(&body).ok()).flatten())
    }

    /// Whether the server takes the probe body: `true` for a 2xx, `false` for a rejection (a 4xx that is not about
    /// access or rate, or 501), and an error for anything that says nothing about the model.
    async fn accepts(&self, body: Vec<u8>) -> Result<bool, LlmError> {
        let url = self.endpoint.url("/v1/chat/completions");
        let (status, _) = self.run(Request::post(url, self.headers(), body)).await?;
        match status {
            200..=299 => Ok(true),
            400..=499 if !matches!(status, 408 | 429) => Ok(false),
            501 => Ok(false),
            other => Err(LlmError::Network(format!("the capability probe was answered {other}"))),
        }
    }
}

/// The server root: the endpoint without a trailing `/v1`, where `/api/show` and `/props` live.
fn root(endpoint: &Endpoint) -> &str {
    endpoint.base.strip_suffix("/v1").unwrap_or(&endpoint.base)
}

/// Finds out what `model` can do. A loopback server is asked and probed; a remote one is assumed to take tools and
/// images, and the server's own answer to a real request is what says otherwise.
///
/// # Errors
/// `Auth` if the key is refused; `Network` if the server cannot be reached or a probe says nothing. Nothing is cached
/// then, so the next build asks again.
pub(crate) async fn probe(
    http: &Http,
    endpoint: &Endpoint,
    key: Option<&SecretValue>,
    model: &str,
) -> Result<Capabilities, LlmError> {
    let mut found = Capabilities { context_window: None, tools: true, vision: true };
    if !endpoint.loopback {
        // Metadata requests and probes would tell a remote host which model is asked for and cost money.
        remember(endpoint, model, found);
        return Ok(found);
    }
    let ask = Ask { http, endpoint, key };
    found.tools = false;
    found.vision = false;
    let mut tools_known = false;
    let mut vision_known = false;

    if let Some(list) = ask.json(Request::get(endpoint.url("/v1/models"), ask.headers())).await? {
        let entry = list
            .get("data")
            .and_then(Value::as_array)
            .and_then(|d| d.iter().find(|m| m.get("id").and_then(Value::as_str) == Some(model)));
        found.context_window = entry.and_then(|m| m.get("max_model_len")).and_then(Value::as_u64);
    }
    let show = serde_json::json!({"model": model}).to_string().into_bytes();
    if let Some(info) = ask.json(Request::post(format!("{}/api/show", root(endpoint)), ask.headers(), show)).await? {
        if let Some(caps) = info.get("capabilities").and_then(Value::as_array) {
            let has = |name: &str| caps.iter().any(|c| c.as_str() == Some(name));
            found.vision = has("vision");
            vision_known = true;
            // Only a positive answer for tools is taken as given; see the module docs.
            found.tools = has("tools");
            tools_known = found.tools;
        }
        let from_info = info
            .get("model_info")
            .and_then(Value::as_object)
            .and_then(|m| m.iter().find(|(k, _)| k.ends_with(".context_length")).and_then(|(_, v)| v.as_u64()));
        found.context_window = found.context_window.or(from_info);
    }
    if let Some(props) = ask.json(Request::get(format!("{}/props", root(endpoint)), ask.headers())).await? {
        let n_ctx = props.get("default_generation_settings").and_then(|d| d.get("n_ctx")).and_then(Value::as_u64);
        found.context_window = n_ctx.or(found.context_window);
        if let Some(vision) = props.get("modalities").and_then(|m| m.get("vision")).and_then(Value::as_bool) {
            found.vision = vision;
            vision_known = true;
        }
    }

    if !tools_known {
        found.tools = ask.accepts(probe_body(model, true, false)).await?;
    }
    if !vision_known {
        found.vision = ask.accepts(probe_body(model, false, true)).await?;
    }
    remember(endpoint, model, found);
    Ok(found)
}
