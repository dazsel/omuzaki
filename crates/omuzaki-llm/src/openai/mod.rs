//! The OpenAI-compatible chat provider (llm-api D6): Ollama, llama.cpp `llama-server`, LM Studio, vLLM and any
//! compatible host, `POST /v1/chat/completions` with `stream: true` and `stream_options.include_usage`.
//!
//! Endpoints are loopback only unless `allow_remote` is set, and plain `http` is refused off loopback. Capabilities
//! are probed once per endpoint and model ([`probe`]); a model the server rejects tools or images for is marked so.

mod decode;
mod probe;
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

use crate::common::{KeyRef, key_ref, refuse_camera};
use crate::engine::{self, Decoder, Fail, Job, Protocol, Timeouts};
use crate::http::{Endpoint, HeaderValue, Http, Request};
use crate::prices::local_prices;
use crate::retry::random_seed;
use crate::runtime::{block_on, handle_of};
use probe::{FALLBACK_CONTEXT, probe};

/// An image costs this many tokens in the pre-request bound; these servers publish no tile rule, so one flat figure
/// that errs high.
const IMAGE_TOKENS: u64 = 1_024;

/// The OpenAI-compatible wire: URL, headers, error mapping and decoder.
struct Wire {
    endpoint: Endpoint,
    model: String,
    context_window: u64,
    /// A key is configured, so a store that does not hold it is an `Auth` failure.
    key_required: bool,
}

impl Protocol for Wire {
    fn url(&self) -> String {
        self.endpoint.url("/v1/chat/completions")
    }

    fn headers(&self, key: Option<&SecretValue>) -> Result<Vec<(&'static str, HeaderValue)>, LlmError> {
        let mut headers = vec![
            ("content-type", HeaderValue::Plain("application/json".into())),
            ("accept", HeaderValue::Plain("text/event-stream".into())),
        ];
        headers.extend(
            key.map(|k| ("authorization", HeaderValue::Secret(SecretValue::new(format!("Bearer {}", k.expose()))))),
        );
        Ok(headers)
    }

    fn key_required(&self) -> bool {
        self.key_required
    }

    fn classify(&self, status: u16, retry_after: Option<Duration>, body: &[u8]) -> Fail {
        decode::classify(status, retry_after, body, self.context_window)
    }

    fn decoder(&self) -> Box<dyn Decoder> {
        Box::new(decode::OpenAiDecoder::new(self.model.clone(), self.context_window))
    }
}

/// A client for one model on an OpenAI-compatible server.
pub struct OpenAiClient {
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

impl std::fmt::Debug for OpenAiClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiClient")
            .field("model", &self.info.id)
            .field("loopback", &self.wire.endpoint.loopback)
            .field("key", &self.key.as_ref().map(|_| "<reference>"))
            .finish_non_exhaustive()
    }
}

/// Checks the endpoint of `config` against the loopback rule.
fn endpoint_of(config: &ProviderConfig) -> Result<Endpoint, LlmError> {
    let endpoint = Endpoint::parse(&config.endpoint)?;
    if !endpoint.loopback && !config.allow_remote {
        return Err(LlmError::Unsupported("the endpoint is not on this machine; set allow_remote to use it".into()));
    }
    Ok(endpoint)
}

impl OpenAiClient {
    /// Builds the client `config` describes, for the model it names, probing what the model can do (once per endpoint
    /// and model; the answer is cached).
    ///
    /// # Errors
    /// `Unsupported` if no model is chosen, the endpoint is remote without `allow_remote` or plain `http` off
    /// loopback, a remote model has no prices in the configuration, or there is no tokio runtime; `Auth` or
    /// `Network` if the probe cannot be made.
    pub fn new(config: &ProviderConfig, env: &ProviderEnv) -> Result<Self, LlmError> {
        let model = config.model.clone().ok_or_else(|| LlmError::Unsupported("no model is chosen".into()))?;
        let endpoint = endpoint_of(config)?;
        // A hosted model with no table cannot be built: an unpriced model would make every cap meaningless.
        let prices = match (&config.prices, endpoint.loopback) {
            (Some(prices), _) => prices.clone(),
            (None, true) => local_prices(),
            (None, false) => {
                return Err(LlmError::Unsupported("a remote model needs prices in its configuration".into()));
            }
        };
        let http = Http::new(&endpoint)?;
        let handle = handle_of(&env.runtime)?;
        let key = key_ref(config, env);
        let found = if let Some(found) = probe::cached(&endpoint, &model) {
            found
        } else {
            let secret = key.as_ref().and_then(|(label, store)| store.get(label));
            if key.is_some() && secret.is_none() {
                return Err(LlmError::Auth);
            }
            let (http, endpoint, model) = (http.clone(), endpoint.clone(), model.clone());
            block_on(&handle, async move { probe(&http, &endpoint, secret.as_ref(), &model).await })??
        };
        let window = config.context_window.or(found.context_window).unwrap_or(FALLBACK_CONTEXT);
        let info = ModelInfo::new(model.clone(), window, window, ImageCost::Flat(IMAGE_TOKENS))
            .with_vision(found.vision)
            .with_tools(found.tools)
            .with_effort(true)
            .with_caching(false);
        Ok(Self {
            info,
            prices,
            http,
            wire: Arc::new(Wire { endpoint, model, context_window: window, key_required: config.key.is_some() }),
            handle,
            key,
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

    /// The prices a caller charges this client's usage with: the configuration's, else zero for a local model.
    pub fn prices(&self) -> &PriceTable {
        &self.prices
    }
}

impl LlmClient for OpenAiClient {
    fn info(&self) -> &ModelInfo {
        &self.info
    }

    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError> {
        if cancel.is_cancelled() {
            return Err(LlmError::Cancelled);
        }
        validate::request(&req, &self.info)?;
        refuse_camera(&req, self.wire.endpoint.loopback)?;
        let job = Job {
            http: self.http.clone(),
            protocol: Arc::clone(&self.wire) as Arc<dyn Protocol>,
            body: wire::body(&req, &self.info.id, self.info.effort),
            key: self.key.clone(),
            max_retries: self.max_retries,
            timeouts: self.timeouts,
            seed: self.seed,
        };
        Ok(engine::spawn(&self.handle, job, cancel))
    }
}

fn manifest() -> ProviderManifest {
    // The stricter class when the configuration is not known; `privacy_for` refines it per configuration.
    ProviderManifest::new(vec![LlmCapability::Llm, LlmCapability::Vision], Privacy::Hosted, CostModel::Free, "")
}

/// A local endpoint sends nothing off the machine; any other, or one that does not parse, is hosted.
fn privacy_for(config: &ProviderConfig) -> Privacy {
    Privacy::for_endpoint(&config.endpoint)
}

fn build(config: &ProviderConfig, env: &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError> {
    Ok(Box::new(OpenAiClient::new(config, env)?))
}

fn list_models(config: &ProviderConfig, env: &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError> {
    let endpoint = endpoint_of(config)?;
    let http = Http::new(&endpoint)?;
    let handle = handle_of(&env.runtime)?;
    let key = key_ref(config, env).and_then(|(label, store)| store.get(&label));
    let window = config.context_window;
    block_on(&handle, async move {
        let mut headers = vec![("accept", HeaderValue::Plain("application/json".into()))];
        headers.extend(
            key.map(|k| ("authorization", HeaderValue::Secret(SecretValue::new(format!("Bearer {}", k.expose()))))),
        );
        let response = http.send(Request::get(endpoint.url("/v1/models"), headers)).await.map_err(LlmError::from)?;
        let status = response.status;
        let body = response.read_limited(4 * 1024 * 1024).await.map_err(LlmError::from)?;
        if status != 200 {
            return Err(match decode::classify(status, None, &body, window.unwrap_or(FALLBACK_CONTEXT)) {
                Fail::Fatal(e) | Fail::Retry { error: e, .. } => e,
            });
        }
        let list: Value = serde_json::from_slice(&body)
            .map_err(|e| LlmError::Malformed(format!("the model list is not JSON: {e}")))?;
        let models = list.get("data").and_then(Value::as_array).into_iter().flatten().filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            let served = m.get("max_model_len").and_then(Value::as_u64);
            let size = window.or(served).unwrap_or(FALLBACK_CONTEXT);
            // Listing makes no probe: whether a model takes tools or images is found out when it is built.
            let sees = m
                .pointer("/architecture/input_modalities")
                .and_then(Value::as_array)
                .is_some_and(|a| a.iter().any(|v| v == "image"));
            Some(
                ModelInfo::new(id, size, size, ImageCost::Flat(IMAGE_TOKENS))
                    .with_vision(sees)
                    .with_tools(true)
                    .with_effort(true),
            )
        });
        Ok(models.collect())
    })?
}

register_llm_provider! {
    kind: "openai-compatible", manifest: manifest, privacy_for: privacy_for, build: build, list_models: list_models,
}
