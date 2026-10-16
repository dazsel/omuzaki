//! The Anthropic Messages provider (llm-api D6): `POST /v1/messages` with `stream: true`.

mod decode;
mod wire;

use std::sync::Arc;
use std::time::Duration;

use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::TurnRequest;
use omuzaki_llm_api::provider::{
    CostModel, LlmCapability, Privacy, ProviderConfig, ProviderEnv, ProviderManifest, RuntimeHandle, SecretValue,
};
use omuzaki_llm_api::stream::{Cancel, TurnStream};
use omuzaki_llm_api::usage::{ImageCost, ModelInfo, PriceTable};
use omuzaki_llm_api::{register_llm_provider, validate};
use serde_json::Value;
use tokio::runtime::Handle;

use crate::common::{KeyRef, check_hosted_prices, key_ref, refuse_camera};
use crate::engine::{self, Decoder, Fail, Job, Protocol, Timeouts, redact_bytes, redact_error};
use crate::http::{Endpoint, HeaderValue, Http, Request, fetch_metadata, query_value};
use crate::prices::{ANTHROPIC_IMAGE_TILE, anthropic_model};
use crate::retry::random_seed;
use crate::runtime::{block_on, handle_of};

/// Where the hosted API is, when a configuration names no endpoint.
const DEFAULT_ENDPOINT: &str = "https://api.anthropic.com";
/// The API version header every request sends (the docs' own examples, retrieved 2026-10-15).
const VERSION: &str = "2023-06-01";
/// The most `GET /v1/models` pages `list_models` reads.
const MAX_PAGES: usize = 10;

/// The Anthropic wire: URL, headers, error mapping and decoder.
struct Wire {
    endpoint: Endpoint,
    context_window: u64,
}

impl Protocol for Wire {
    fn url(&self) -> String {
        self.endpoint.url("/v1/messages")
    }

    fn headers(&self, key: Option<&SecretValue>) -> Result<Vec<(&'static str, HeaderValue)>, LlmError> {
        let mut headers = vec![
            ("anthropic-version", HeaderValue::Plain(VERSION.into())),
            ("content-type", HeaderValue::Plain("application/json".into())),
            ("accept", HeaderValue::Plain("text/event-stream".into())),
        ];
        headers.extend(key.map(|k| ("x-api-key", HeaderValue::Secret(k.clone()))));
        Ok(headers)
    }

    fn key_required(&self) -> bool {
        true
    }

    fn classify(&self, status: u16, retry_after: Option<Duration>, body: &[u8], secret: Option<&SecretValue>) -> Fail {
        decode::classify(status, retry_after, body, self.context_window, secret)
    }

    fn decoder(&self, secret: Option<SecretValue>) -> Box<dyn Decoder> {
        Box::new(decode::AnthropicDecoder::new(secret))
    }
}

/// A client for one Anthropic model.
pub struct AnthropicClient {
    info: ModelInfo,
    prices: PriceTable,
    http: Http,
    wire: Arc<Wire>,
    handle: Handle,
    key: KeyRef,
    max_retries: u8,
    timeouts: Timeouts,
    seed: u64,
    /// Keeps a runtime this crate started alive for as long as the client.
    _runtime: RuntimeHandle,
}

impl std::fmt::Debug for AnthropicClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AnthropicClient")
            .field("model", &self.info.id)
            .field("loopback", &self.wire.endpoint.loopback)
            .field("key", &self.key.as_ref().map(|_| "<reference>"))
            .finish_non_exhaustive()
    }
}

fn endpoint_of(config: &ProviderConfig) -> Result<Endpoint, LlmError> {
    let named = config.endpoint.trim();
    Endpoint::parse(if named.is_empty() { DEFAULT_ENDPOINT } else { named })
}

impl AnthropicClient {
    /// Builds the client `config` describes, for the model it names.
    ///
    /// # Errors
    /// `Unsupported` if no model is chosen, the model is not in the shipped table (an unpriced model would make every
    /// cap meaningless), the endpoint is refused, or there is no tokio runtime. No I/O happens here.
    pub fn new(config: &ProviderConfig, env: &ProviderEnv) -> Result<Self, LlmError> {
        let model = config.model.as_deref().ok_or_else(|| LlmError::Unsupported("no model is chosen".into()))?;
        let entry = anthropic_model(model)?
            .ok_or_else(|| LlmError::Unsupported(format!("{model} has no price table; add it to prices.toml")))?;
        let endpoint = endpoint_of(config)?;
        let mut info = entry.info.clone();
        if let Some(window) = config.context_window {
            info.context_window = window;
        }
        let prices = config.prices.clone().unwrap_or_else(|| entry.prices.clone());
        check_hosted_prices(&prices)?;
        // The wire reports the window the client validates against, override included.
        let window = info.context_window;
        Ok(Self {
            info,
            prices,
            http: Http::new(&endpoint)?,
            wire: Arc::new(Wire { context_window: window, endpoint }),
            handle: handle_of(&env.runtime)?,
            key: key_ref(config, env),
            max_retries: config.retry.max_retries,
            timeouts: Timeouts::default(),
            seed: random_seed(),
            _runtime: env.runtime.clone(),
        })
    }

    /// Replaces the first-byte and idle timeouts (tests shorten them).
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: Timeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Fixes the seed of the retry jitter, so a test gets the same waits every run.
    #[must_use]
    pub fn with_seed(mut self, seed: u64) -> Self {
        self.seed = seed;
        self
    }

    /// The prices a caller charges this client's usage with: the entry's override, else the shipped table.
    pub fn prices(&self) -> &PriceTable {
        &self.prices
    }
}

impl LlmClient for AnthropicClient {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        validate::request(&req, &self.info)?;
        refuse_camera(&req, self.wire.endpoint.loopback)?;
        // The history and the images are encoded on the task, not here (llm-api D1).
        let (model, effort, caching) = (self.info.id.clone(), self.info.effort, self.info.caching);
        let (http, wire) = (self.http.clone(), Arc::clone(&self.wire));
        let (key, max_retries, timeouts, seed) = (self.key.clone(), self.max_retries, self.timeouts, self.seed);
        let make = move || Job {
            http,
            protocol: wire as Arc<dyn Protocol>,
            body: wire::body(&req, &model, effort, caching),
            key,
            max_retries,
            timeouts,
            seed,
        };
        Ok(engine::spawn(&self.handle, make, cancel))
    }
}

fn manifest() -> ProviderManifest {
    ProviderManifest::new(
        vec![LlmCapability::Llm, LlmCapability::Vision],
        // The configured endpoint can be a proxy that forwards anywhere: an Anthropic client is always hosted.
        Privacy::Hosted,
        CostModel::Metered,
        "https://www.anthropic.com/legal/commercial-terms",
    )
}

fn build(config: &ProviderConfig, env: &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError> {
    Ok(Box::new(AnthropicClient::new(config, env)?))
}

/// What `GET /v1/models` says about a model, merged with the shipped table.
fn listed(entry: &Value) -> Option<ModelInfo> {
    let id = entry.get("id")?.as_str()?;
    if let Ok(Some(known)) = anthropic_model(id) {
        return Some(known.info.clone());
    }
    // Listed but not usable until priced: `build` refuses it. Its limits are what the API reports.
    let limit = |key| entry.get(key).and_then(Value::as_u64).unwrap_or(0);
    Some(ModelInfo::new(id, limit("max_input_tokens"), limit("max_tokens"), ImageCost::tiles(ANTHROPIC_IMAGE_TILE)))
}

fn list_models(config: &ProviderConfig, env: &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError> {
    list_models_within(config, env, &Timeouts::default())
}

/// Lists the models the account can use, each request bounded by `timeouts.metadata`.
///
/// # Errors
/// `Unsupported` for a bad endpoint or no runtime, `Auth` without a key, `Network` if the server does not answer in
/// time, the provider's own error otherwise (the key redacted), and `Malformed` for a list that does not end within
/// the page cap.
pub fn list_models_within(
    config: &ProviderConfig,
    env: &ProviderEnv,
    timeouts: &Timeouts,
) -> Result<Vec<ModelInfo>, LlmError> {
    let limit = timeouts.metadata;
    let endpoint = endpoint_of(config)?;
    let http = Http::new(&endpoint)?;
    let handle = handle_of(&env.runtime)?;
    let key = key_ref(config, env).and_then(|(label, store)| store.get(&label)).ok_or(LlmError::Auth)?;
    block_on(&handle, async move {
        let mut models = Vec::new();
        let mut after: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let query = after.as_ref().map_or(String::new(), |id| format!("&after_id={}", query_value(id)));
            let url = endpoint.url(&format!("/v1/models?limit=100{query}"));
            let headers = vec![
                ("anthropic-version", HeaderValue::Plain(VERSION.into())),
                ("x-api-key", HeaderValue::Secret(key.clone())),
            ];
            let (status, body) = fetch_metadata(&http, Request::get(url, headers), limit).await?;
            if status != 200 {
                let error = match decode::classify(status, None, &redact_bytes(body, Some(&key)), 0, Some(&key)) {
                    Fail::Fatal(e) | Fail::Retry { error: e, .. } => e,
                };
                // A server may echo the key into its message.
                return Err(redact_error(error, Some(&key)));
            }
            let page: Value = serde_json::from_slice(&body)
                .map_err(|e| LlmError::Malformed(format!("the model list is not JSON: {e}")))?;
            models.extend(page.get("data").and_then(Value::as_array).into_iter().flatten().filter_map(listed));
            after = page
                .get("has_more")
                .and_then(Value::as_bool)
                .filter(|more| *more)
                .and(page.get("last_id").and_then(Value::as_str).map(str::to_owned));
            if after.is_none() {
                return Ok(models);
            }
        }
        // The cap stays, but a listing that is cut short says so rather than passing for the whole.
        Err(LlmError::Malformed(format!("the model list has more than {MAX_PAGES} pages")))
    })?
}

register_llm_provider! { kind: "anthropic", manifest: manifest, build: build, list_models: list_models }
