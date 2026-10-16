//! The OpenAI-compatible chat provider (llm-api D6): Ollama, llama.cpp `llama-server`, LM Studio, vLLM and any
//! compatible host, `POST /v1/chat/completions` with `stream: true` and `stream_options.include_usage`.
//!
//! Endpoints are loopback only unless `allow_remote` is set, and plain `http` is refused off loopback. Capabilities
//! are probed once per endpoint and model ([`probe`]); a model the server rejects tools or images for is marked so.

mod decode;
pub(crate) mod probe;
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
use crate::engine::{self, Decoder, Fail, Job, Protocol, Timeouts, redact_error};
use crate::http::{Endpoint, HeaderValue, Http, Request, bearer, fetch_metadata};
use crate::prices::local_prices;
use crate::retry::random_seed;
use crate::runtime::{block_on, handle_of, probes_of};
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
        headers.extend(key.map(bearer));
        Ok(headers)
    }

    fn key_required(&self) -> bool {
        self.key_required
    }

    fn classify(&self, status: u16, retry_after: Option<Duration>, body: &[u8], secret: Option<&SecretValue>) -> Fail {
        decode::classify(status, retry_after, body, self.context_window, secret)
    }

    fn decoder(&self, secret: Option<SecretValue>) -> Box<dyn Decoder> {
        Box::new(decode::OpenAiDecoder::new(self.model.clone(), self.context_window, secret))
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
        Self::new_within(config, env, &Timeouts::default())
    }

    /// As [`new`](Self::new), with the probe bounded by `timeouts`: each request by `metadata`, all of them together by
    /// `probe`. The client keeps the default turn timeouts (see [`with_timeouts`](Self::with_timeouts)).
    ///
    /// # Errors
    /// As `new`, and `Network` if the probe does not finish in time.
    pub fn new_within(config: &ProviderConfig, env: &ProviderEnv, timeouts: &Timeouts) -> Result<Self, LlmError> {
        let model = config.model.clone().ok_or_else(|| LlmError::Unsupported("no model is chosen".into()))?;
        let endpoint = endpoint_of(config)?;
        // A hosted model with no table cannot be built: an unpriced model would make every cap meaningless.
        let prices = match (&config.prices, endpoint.loopback) {
            (Some(prices), _) => {
                if !endpoint.loopback {
                    check_hosted_prices(prices)?;
                }
                prices.clone()
            }
            (None, true) => local_prices(),
            (None, false) => {
                return Err(LlmError::Unsupported("a remote model needs prices in its configuration".into()));
            }
        };
        let http = Http::new(&endpoint)?;
        let handle = handle_of(&env.runtime)?;
        let key = key_ref(config, env);
        let probes = probes_of(&env.runtime)?.clone();
        let found = if let Some(found) = probes.get(&endpoint, &model) {
            found
        } else {
            let secret = key.as_ref().and_then(|(label, store)| store.get(label));
            if key.is_some() && secret.is_none() {
                return Err(LlmError::Auth);
            }
            let (http, endpoint, model) = (http.clone(), endpoint.clone(), model.clone());
            let (limit, whole) = (timeouts.metadata, timeouts.probe);
            block_on(&handle, async move {
                tokio::time::timeout(whole, probe(&http, &endpoint, secret.as_ref(), &model, limit, &probes))
                    .await
                    .unwrap_or_else(|_| Err(LlmError::Network("the capability probe took too long".into())))
            })??
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
        // The history and the images are encoded on the task, not here (llm-api D1).
        let (model, effort) = (self.info.id.clone(), self.info.effort);
        let (http, wire) = (self.http.clone(), Arc::clone(&self.wire));
        let (key, max_retries, timeouts, seed) = (self.key.clone(), self.max_retries, self.timeouts, self.seed);
        let make = move || Job {
            http,
            protocol: wire as Arc<dyn Protocol>,
            body: wire::body(&req, &model, effort),
            key,
            max_retries,
            timeouts,
            seed,
        };
        Ok(engine::spawn(&self.handle, make, cancel))
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
    list_models_within(config, env, &Timeouts::default())
}

/// Lists the models the server serves, the request bounded by `timeouts.metadata`.
///
/// # Errors
/// `Unsupported` for a bad or remote-without-`allow_remote` endpoint or no runtime, `Network` if the server does not
/// answer in time, the server's own error otherwise (the key redacted).
pub fn list_models_within(
    config: &ProviderConfig,
    env: &ProviderEnv,
    timeouts: &Timeouts,
) -> Result<Vec<ModelInfo>, LlmError> {
    let limit = timeouts.metadata;
    let endpoint = endpoint_of(config)?;
    let http = Http::new(&endpoint)?;
    let handle = handle_of(&env.runtime)?;
    let key = key_ref(config, env).and_then(|(label, store)| store.get(&label));
    let window = config.context_window;
    block_on(&handle, async move {
        let mut headers = vec![("accept", HeaderValue::Plain("application/json".into()))];
        headers.extend(key.as_ref().map(bearer));
        let (status, body) =
            fetch_metadata(&http, Request::get(endpoint.url("/v1/models"), headers), limit, key.as_ref()).await?;
        if status != 200 {
            let error = match decode::classify(status, None, &body, window.unwrap_or(FALLBACK_CONTEXT), key.as_ref()) {
                Fail::Fatal(e) | Fail::Retry { error: e, .. } => e,
            };
            // A server may echo the key into its message.
            return Err(redact_error(error, key.as_ref()));
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
