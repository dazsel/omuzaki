//! The co-host as the studio builds it, from the app's own services: the speech models the person chose in the
//! model registry (Silero VAD, Whisper, Kokoro), the microphone and the mixer of the audio engine, the LLM of the
//! `performer.llm` setting, the decider of the `performer.decider` setting (the rule floor unless the person chose a
//! model), and the perform host that moves the character. [`MicMouth`] gives the character's mouth
//! to the person's microphone whenever the co-host is not speaking through it.
//!
//! ```text
//!  mic ─▶ Voice(Listen) tap ─▶ SpeechHost (VAD, Whisper) ─▶ ears ─┐
//!                                                                  ▼
//!                              performer.llm ─▶ LlmBrain ─▶ Show ─▶ TtsHost (Kokoro) ─▶ ai_voice feeds ─▶ mixer
//!                                                                  │ intents            │ lead PCM
//!                                                                  ▼                    ▼
//!                                                       PerformStage ─▶ PerformHost ◀─ LeadAnalysis (lip sync)
//! ```

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use omuzaki_ai_api::{ModelRegistry, SlotRef};
use omuzaki_audio_api::clock::EngineClock;
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{AI_VOICE_BUS, BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapRequest, VoiceUse};
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_llm::json::JsonClient;
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::provider::{
    ProviderConfig, ProviderEnv, RuntimeHandle, SecretLabel, SecretRef, SecretStore, SecretValue, endpoint_host,
    is_loopback_endpoint, provider,
};
use omuzaki_llm_api::usage::MicroUsd;
use omuzaki_llm_realtime::{OpenAiRealtime, RealtimeConfig, TurnDetection};
use omuzaki_ops_api::Spend;
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::decide::LlmDecider;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::prompt::realtime_instructions;
use omuzaki_performer::realtime::{RealtimeParts, RealtimeShow};
use omuzaki_performer::show::{Parts, Show, ShowConfig};
use omuzaki_performer::systemone::{self, JsonTransport, SystemOneDecider};
use omuzaki_performer_api::decide::{DecideError, Decider};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::spend::{SpendError, SpendGate, Wallet};
use omuzaki_settings_api::service::Settings;
use omuzaki_speech::slots::{asr_slot, tts_slot, vad_slot};
use omuzaki_speech::tts::FEEDS;
use omuzaki_speech::{EchoLog, HostConfig, SpeechHost, TtsHost, TtsVoice};
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_track_voice::{LipSyncConfig, VoiceAnalyzer, VoiceListener, VowelProfile, voice_mailbox};
use serde_json::{Value, json};

use crate::service::{Booking, Built, CohostFactory, StartError};
use crate::settings::{
    DECIDER_LLM, DECIDER_RULES, DECIDER_SYSTEM_ONE, DeciderChoice, LlmChoice, OPENAI_COMPATIBLE, PerformerDecider,
    PerformerLlm, PerformerRealtime, REALTIME_OPENAI, RealtimeChoice,
};
use crate::stage::PerformStage;
use crate::{Cohost, attach_voice};

/// The voice a persona without one speaks in: Kokoro's `af_heart`.
pub const DEFAULT_VOICE: &str = "kokoro-82m/af_heart";
/// The bus the co-host's private lines play on (heard on the monitor only).
pub const AI_VOICE_PRIVATE_BUS: &str = "ai_voice_private";
/// The engine's graph rate.
const GRAPH_RATE: u32 = 48_000;
/// How long a hosted decision request may take before it is dropped. The show stops waiting much sooner (the rule
/// floor answers after 400 ms); this only bounds the provider's own thread.
const DECISION_REQUEST_LIMIT: Duration = Duration::from_secs(3);

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The program clock: the audio engine's, which the TTS host times its playback on.
struct EngineTime(Arc<EngineClock>);

impl Clock for EngineTime {
    fn now(&self) -> Seconds {
        self.0.read().time
    }
}

/// The app's keychain as the LLM providers read it: one key by its label, at request time.
struct Keychain(Arc<dyn omuzaki_secrets_api::prelude::SecretStore>);

impl SecretStore for Keychain {
    fn get(&self, label: &SecretLabel) -> Option<SecretValue> {
        let label = omuzaki_secrets_api::prelude::SecretLabel::new(label.0.as_str()).ok()?;
        let value = self.0.get(&label).ok().flatten()?;
        Some(SecretValue::new(value.expose()))
    }
}

/// The System One API over `omuzaki-llm`'s JSON client.
struct SystemOneHttp(JsonClient);

impl JsonTransport for SystemOneHttp {
    fn post(&self, body: &Value) -> Result<Value, DecideError> {
        self.0.post(systemone::PATH, body).map_err(|e| match e {
            LlmError::Auth => DecideError::NotConfigured("the provider refused the key".into()),
            other => DecideError::provider(other.to_string()),
        })
    }
}

/// One show's purse for a realtime model: the cap from `performer.realtime`, and what it has been charged.
struct ShowWallet {
    cap: MicroUsd,
    spent: Mutex<u64>,
}

impl Wallet for ShowWallet {
    fn remaining(&self) -> Result<MicroUsd, SpendError> {
        Ok(MicroUsd(self.cap.0.saturating_sub(*lock(&self.spent))))
    }

    fn charge(&self, spend: Spend) -> Result<(), SpendError> {
        let mut spent = lock(&self.spent);
        *spent = spent.saturating_add(spend.usd_micros);
        Ok(())
    }
}

/// No keychain: no keys.
struct NoKeys;

impl SecretStore for NoKeys {
    fn get(&self, _: &SecretLabel) -> Option<SecretValue> {
        None
    }
}

/// The character's mouth from the person's microphone: the `Voice(Mouth)` tap through the voice analyser into the
/// perform host's lip sync. The co-host takes the mouth while it runs ([`detach`](Self::detach)) and gives it back
/// when it stops ([`attach`](Self::attach)).
pub struct MicMouth {
    host: Arc<dyn PerformHost>,
    engine: Arc<dyn AudioEngine>,
    config: LipSyncConfig,
    listener: Mutex<Option<VoiceListener>>,
}

impl MicMouth {
    /// The mouth of `host`, heard on `engine`'s microphone; detached until [`attach`](Self::attach).
    pub fn new(host: Arc<dyn PerformHost>, engine: Arc<dyn AudioEngine>) -> Self {
        Self { host, engine, config: LipSyncConfig::default(), listener: Mutex::new(None) }
    }

    /// Gives the mouth to the microphone. Attaching again restarts the listener.
    ///
    /// # Errors
    /// What failed, in words: the engine has no mouth tap, the analyser refused its rate, or the host its voice.
    pub fn attach(&self) -> Result<(), String> {
        let request = TapRequest {
            point: TapPoint::Voice(VoiceUse::Mouth),
            format: TapFormat { rate: TapRate::Graph48k, mono: true },
            capacity: Seconds(0.5),
            policy: DropPolicy::Overwrite,
            consumer: "lip sync".into(),
            leaves_device: false,
        };
        let (reader, info) =
            self.engine.tap_ring(&request).map_err(|e| format!("the microphone can't be heard for lip sync: {e}"))?;
        let analyzer = VoiceAnalyzer::new(info.rate, VowelProfile::default()).map_err(|e| e.to_string())?;
        let (writer, voice) = voice_mailbox();
        let listener = VoiceListener::start(reader, analyzer, writer).map_err(|e| e.to_string())?;
        self.host.set_voice(Some(voice), self.config).map_err(|e| e.to_string())?;
        *lock(&self.listener) = Some(listener);
        Ok(())
    }

    /// Takes the mouth from the microphone: stops the listener (the next voice the host gets replaces it).
    pub fn detach(&self) {
        let listener = lock(&self.listener).take();
        drop(listener);
    }

    /// Whether the microphone has the mouth.
    pub fn attached(&self) -> bool {
        lock(&self.listener).is_some()
    }
}

/// What the studio hands the local factory.
#[derive(Clone)]
#[non_exhaustive]
pub struct LocalParts {
    /// Moves the character.
    pub host: Arc<dyn PerformHost>,
    /// The microphone and the mixer; `None` when the audio engine did not start.
    pub engine: Option<Arc<dyn AudioEngine>>,
    /// The speech models; `None` without a data folder.
    pub models: Option<Arc<dyn ModelRegistry>>,
    /// Where `performer.llm` is read; `None` reads its default.
    pub settings: Option<Arc<dyn Settings>>,
    /// Where a hosted provider's key is read; `None` has no keys.
    pub secrets: Option<Arc<dyn omuzaki_secrets_api::prelude::SecretStore>>,
}

impl LocalParts {
    /// The parts over `host`, with nothing else yet.
    pub fn new(host: Arc<dyn PerformHost>) -> Self {
        Self { host, engine: None, models: None, settings: None, secrets: None }
    }

    /// The same, with the audio engine.
    #[must_use]
    pub fn engine(mut self, engine: Arc<dyn AudioEngine>) -> Self {
        self.engine = Some(engine);
        self
    }

    /// The same, with the model registry.
    #[must_use]
    pub fn models(mut self, models: Arc<dyn ModelRegistry>) -> Self {
        self.models = Some(models);
        self
    }

    /// The same, with the settings.
    #[must_use]
    pub fn settings(mut self, settings: Arc<dyn Settings>) -> Self {
        self.settings = Some(settings);
        self
    }

    /// The same, with the keychain.
    #[must_use]
    pub fn secrets(mut self, secrets: Arc<dyn omuzaki_secrets_api::prelude::SecretStore>) -> Self {
        self.secrets = Some(secrets);
        self
    }
}

/// Builds the co-host from the app's own services ([`LocalParts`]).
pub struct LocalFactory {
    parts: LocalParts,
    mouth: Option<Arc<MicMouth>>,
    runtime: Mutex<Option<RuntimeHandle>>,
}

impl LocalFactory {
    /// A factory over `parts`. With an audio engine, the microphone gets the character's mouth at once.
    pub fn new(parts: LocalParts) -> Self {
        let mouth =
            parts.engine.as_ref().map(|engine| Arc::new(MicMouth::new(Arc::clone(&parts.host), Arc::clone(engine))));
        if let Some(mouth) = &mouth
            && let Err(error) = mouth.attach()
        {
            tracing::warn!(%error, "the microphone does not move the mouth");
        }
        Self { parts, mouth, runtime: Mutex::new(None) }
    }

    /// The microphone's hold on the mouth, if there is an audio engine.
    pub fn mouth(&self) -> Option<&Arc<MicMouth>> {
        self.mouth.as_ref()
    }

    fn choice(&self) -> LlmChoice {
        self.parts
            .settings
            .as_ref()
            .and_then(|settings| settings.snapshot().get::<PerformerLlm>().ok())
            .unwrap_or_default()
    }

    fn realtime_choice(&self) -> RealtimeChoice {
        self.parts
            .settings
            .as_ref()
            .and_then(|settings| settings.snapshot().get::<PerformerRealtime>().ok())
            .unwrap_or_default()
    }

    fn decider_choice(&self) -> DeciderChoice {
        self.parts
            .settings
            .as_ref()
            .and_then(|settings| settings.snapshot().get::<PerformerDecider>().ok())
            .unwrap_or_default()
    }

    fn keys(&self) -> Arc<dyn SecretStore> {
        match &self.parts.secrets {
            Some(secrets) => Arc::new(Keychain(Arc::clone(secrets))),
            None => Arc::new(NoKeys),
        }
    }

    /// The decider `choice` names, given the co-host's own LLM: `None` for the rule floor alone.
    ///
    /// # Errors
    /// [`StartError::NotReady`] for a provider that can't be reached as set: no key, a remote host not allowed, a bad
    /// endpoint.
    pub fn decider(
        &self,
        choice: &DeciderChoice,
        llm: &LlmChoice,
        talking: &Arc<dyn LlmClient>,
    ) -> Result<Option<Box<dyn Decider>>, StartError> {
        self.decider_with(choice, llm, || Ok(Arc::clone(talking)))
    }

    /// [`decider`](Self::decider), building the co-host's own LLM client only if the decider shares it.
    fn decider_with(
        &self,
        choice: &DeciderChoice,
        llm: &LlmChoice,
        talking: impl FnOnce() -> Result<Arc<dyn LlmClient>, StartError>,
    ) -> Result<Option<Box<dyn Decider>>, StartError> {
        let fix = "set performer.decider in Settings > Co-host, or its kind to `rules`";
        match choice.kind.as_str() {
            DECIDER_RULES => Ok(None),
            DECIDER_LLM => {
                let (client, model) = match &choice.endpoint {
                    None => (talking()?, llm.model.clone()),
                    Some(endpoint) => {
                        let own = LlmChoice {
                            kind: OPENAI_COMPATIBLE.to_owned(),
                            endpoint: endpoint.clone(),
                            model: choice.model.clone(),
                            key: choice.key.clone(),
                            allow_remote: choice.allow_remote,
                            reasoning: false,
                        };
                        (self.client(&own)?, choice.model.clone())
                    }
                };
                let decider = LlmDecider::new(client, DECIDER_LLM);
                let qwen3 = model.unwrap_or_default().to_ascii_lowercase().starts_with("qwen3");
                Ok(Some(Box::new(if qwen3 { decider.with_note("/no_think") } else { decider })))
            }
            DECIDER_SYSTEM_ONE => {
                let endpoint = choice.endpoint().unwrap_or(systemone::DEFAULT_ENDPOINT);
                let key = match &choice.key {
                    Some(label) => Some(self.keys().get(&SecretLabel(label.clone())).ok_or_else(|| {
                        StartError::not_ready(
                            format!("performer.decider's key `{label}` is not in the keychain"),
                            "add your TypeSafe key to the keychain under that label",
                        )
                    })?),
                    None if is_loopback_endpoint(endpoint) => None,
                    None => {
                        return Err(StartError::not_ready(
                            "performer.decider is `systemone` but names no key",
                            "add your TypeSafe key to the keychain and set performer.decider's `key` to its label",
                        ));
                    }
                };
                let client =
                    JsonClient::new(endpoint, key, choice.allow_remote, &self.runtime()?, DECISION_REQUEST_LIMIT)
                        .map_err(|e| {
                            StartError::not_ready(
                                format!("the decision provider can't be used: {e}"),
                                "set performer.decider's `allow_remote` to send the transcript to a hosted provider",
                            )
                        })?;
                let model = choice.model.as_deref().unwrap_or(systemone::DEFAULT_MODEL);
                Ok(Some(Box::new(SystemOneDecider::new(Arc::new(SystemOneHttp(client)), model))))
            }
            other => Err(StartError::not_ready(format!("performer.decider has an unknown kind `{other}`"), fix)),
        }
    }

    /// The key `performer.realtime` names, checked against its address: `None` for a server on this machine that
    /// needs none.
    ///
    /// # Errors
    /// [`StartError::NotReady`]: a hosted address without `allow_remote`, no key, or a key not in the keychain.
    fn realtime_key(
        &self,
        choice: &RealtimeChoice,
    ) -> Result<Option<omuzaki_secrets_api::prelude::SecretValue>, StartError> {
        let fix = "set performer.realtime in Settings > Co-host, or its kind to `off`";
        if choice.kind != REALTIME_OPENAI {
            return Err(StartError::not_ready(
                format!("performer.realtime has an unknown kind `{}`", choice.kind),
                fix,
            ));
        }
        let endpoint = choice.endpoint().unwrap_or(omuzaki_llm_realtime::DEFAULT_ENDPOINT);
        let (host, local) = omuzaki_llm_realtime::endpoint_host(endpoint)
            .ok_or_else(|| StartError::not_ready("performer.realtime's endpoint is not a WebSocket address", fix))?;
        if !local && !choice.allow_remote {
            return Err(StartError::not_ready(
                format!("performer.realtime would send what the co-host hears to {host}"),
                "set performer.realtime's `allow_remote` to use a hosted model",
            ));
        }
        let Some(label) = &choice.key else {
            return if local {
                Ok(None)
            } else {
                Err(StartError::not_ready(
                    "performer.realtime is on but names no key",
                    "add your OpenAI key to the keychain and set performer.realtime's `key` to its label",
                ))
            };
        };
        let missing = || {
            StartError::not_ready(
                format!("performer.realtime's key `{label}` is not in the keychain"),
                "add your OpenAI key to the keychain under that label",
            )
        };
        let secrets = self.parts.secrets.as_ref().ok_or_else(missing)?;
        let label = omuzaki_secrets_api::prelude::SecretLabel::new(label.as_str()).map_err(|_| missing())?;
        secrets.get(&label).ok().flatten().map(Some).ok_or_else(missing)
    }

    /// The co-host over a realtime model: it hears the microphone (48 kHz, sent as it is captured), answers in the
    /// model's own voice through the TTS host's feeds and lip sync, gets its face from the decision layer, and bills
    /// every reply against the show's cap.
    #[allow(clippy::too_many_lines)] // The parts assembled in the order they depend on each other, undone on failure.
    fn build_realtime(&self, booking: &Booking, choice: &RealtimeChoice) -> Result<Built, StartError> {
        let engine = Arc::clone(self.engine()?);
        let key = self.realtime_key(choice)?;
        let llm = self.choice();
        let deciding = self.decider_choice();
        let decider = self.decider_with(&deciding, &llm, || self.client(&llm))?;
        ensure_voice_buses(engine.as_ref())?;
        let host = Arc::clone(&self.parts.host);
        let stage = Arc::new(PerformStage::new(Arc::clone(&host), booking.member.clone()));
        if let Some(mouth) = &self.mouth {
            mouth.detach();
        }
        let restore = self.mouth.clone();
        let give_back = move || {
            if let Some(mouth) = &restore
                && let Err(error) = mouth.attach()
            {
                tracing::warn!(%error, "the microphone did not get the mouth back");
            }
        };
        let lead = match attach_voice(host.as_ref(), LipSyncConfig::default()) {
            Ok(lead) => lead,
            Err(error) => {
                give_back();
                return Err(StartError::failed(format!("the character can't take the co-host's voice: {error}")));
            }
        };
        let built = (|| {
            let voice_name = choice.voice.as_deref().unwrap_or(omuzaki_llm_realtime::DEFAULT_VOICE);
            let voice_id = format!("openai/{voice_name}");
            let voice = VoiceId::new(&voice_id).map_err(|e| StartError::failed(format!("voice `{voice_id}`: {e}")))?;
            let mut tts =
                TtsHost::open_streamed(Arc::clone(&engine), TtsVoice::new(voice.clone()), Some(Box::new(lead.clone())))
                    .map_err(StartError::failed)?;
            tts.spawn_pump().map_err(StartError::failed)?;
            let endpoint = choice.endpoint().unwrap_or(omuzaki_llm_realtime::DEFAULT_ENDPOINT);
            let (endpoint_host, local) = omuzaki_llm_realtime::endpoint_host(endpoint).unwrap_or_default();
            let request = TapRequest {
                point: TapPoint::Voice(VoiceUse::Listen),
                format: TapFormat { rate: TapRate::Graph48k, mono: true },
                capacity: Seconds(2.0),
                policy: DropPolicy::DropNewest,
                consumer: "co-host realtime ears".into(),
                leaves_device: !local,
            };
            let (mic, info) = engine.tap_ring(&request).map_err(|error| {
                StartError::not_ready(
                    format!("the co-host can't hear the microphone: {error}"),
                    "choose a microphone in Settings > Audio",
                )
            })?;
            if info.rate != omuzaki_llm_realtime::MIC_RATE {
                return Err(StartError::failed(format!("the microphone tap runs at {} Hz, not 48 kHz", info.rate)));
            }
            let turns = match choice.turns.as_deref() {
                Some("silence") => TurnDetection::Silence,
                _ => TurnDetection::Semantic,
            };
            let config = RealtimeConfig::new(realtime_instructions(&booking.persona))
                .endpoint(endpoint)
                .model(choice.model())
                .voice(voice_name)
                .turns(turns);
            let duplex = OpenAiRealtime::connect(&config, key.as_ref(), Some(mic)).map_err(|e| {
                StartError::not_ready(
                    format!("the realtime model can't be used: {e}"),
                    "check performer.realtime's key and address in Settings > Co-host",
                )
            })?;
            let cap = MicroUsd(choice.cap_cents().saturating_mul(10_000));
            let wallet = ShowWallet { cap, spent: Mutex::new(0) };
            let mut parts = RealtimeParts::new(
                Box::new(duplex),
                Box::new(tts),
                Arc::clone(&stage) as Arc<dyn omuzaki_performer_api::stage::Stage>,
            )
            .spend(SpendGate::new(Arc::new(wallet)));
            if let Some(decider) = decider {
                parts = parts.decider(decider);
            }
            let config = ShowConfig::new(booking.member.clone(), voice.clone()).respond(booking.respond);
            let show = RealtimeShow::new(&booking.persona, parts, config);
            let (prices, known) = omuzaki_llm_realtime::prices(choice.model());
            let about = json!({
                "realtime": {
                    "kind": omuzaki_llm_realtime::KIND,
                    "endpoint_host": endpoint_host,
                    "model": choice.model(),
                    "local": local,
                    "listening_usd_per_minute": usd(prices.listening_per_minute()),
                    "speaking_usd_per_minute": usd(prices.speaking_per_minute()),
                    "cap_usd": cents_usd(choice.cap_cents()),
                    "prices_known": known,
                },
                "decider": {
                    "kind": deciding.kind,
                    "endpoint_host": deciding.endpoint().and_then(endpoint_host_of),
                    "model": deciding.model,
                },
                "voice": voice.to_string(),
                "hearing": { "rate": info.rate },
            });
            Ok((show, about, engine.clock()))
        })();
        match built {
            Ok((show, about, clock)) => {
                let cohost = Cohost::new(show, stage, Some(lead));
                Ok(Built::new(cohost, Arc::new(EngineTime(clock))).about(about).on_stop(give_back))
            }
            Err(error) => {
                give_back();
                Err(error)
            }
        }
    }

    fn engine(&self) -> Result<&Arc<dyn AudioEngine>, StartError> {
        self.parts.engine.as_ref().ok_or_else(|| {
            StartError::not_ready(
                "the audio engine did not start, so the co-host can't hear or speak",
                "check the microphone in Settings > Audio, then restart the studio",
            )
        })
    }

    fn models(&self) -> Result<&Arc<dyn ModelRegistry>, StartError> {
        self.parts.models.as_ref().ok_or_else(|| {
            StartError::not_ready("the model registry is not open", "the studio needs its data folder to keep models")
        })
    }

    fn runtime(&self) -> Result<RuntimeHandle, StartError> {
        let mut runtime = lock(&self.runtime);
        if let Some(handle) = runtime.as_ref() {
            return Ok(handle.clone());
        }
        let handle = omuzaki_llm::runtime::dedicated().map_err(StartError::failed)?;
        *runtime = Some(handle.clone());
        Ok(handle)
    }

    fn provider_config(choice: &LlmChoice) -> ProviderConfig {
        let mut config = ProviderConfig::new("performer", choice.kind.as_str(), choice.endpoint.as_str())
            .with_allow_remote(choice.allow_remote);
        if let Some(model) = &choice.model {
            config = config.with_model(model.as_str());
        }
        if let Some(key) = &choice.key {
            config = config.with_key(SecretRef(SecretLabel(key.clone())));
        }
        config
    }

    fn client(&self, choice: &LlmChoice) -> Result<Arc<dyn LlmClient>, StartError> {
        let kind = provider(&choice.kind).map_err(StartError::failed)?.ok_or_else(|| {
            StartError::not_ready(
                format!("no LLM provider of kind `{}` is built into this studio", choice.kind),
                "set performer.llm's `kind` to `openai-compatible` in Settings > Co-host",
            )
        })?;
        let env = ProviderEnv::new(self.keys(), self.runtime()?);
        let client = (kind.build)(&Self::provider_config(choice), &env).map_err(|e| {
            StartError::not_ready(
                format!("the co-host's LLM at {} can't be used: {e}", choice.endpoint),
                "start a local OpenAI-compatible server there (llama-server, Ollama, LM Studio), or set performer.llm \
                 in Settings > Co-host",
            )
        })?;
        Ok(Arc::from(client))
    }

    fn slot_ready(models: &dyn ModelRegistry, slot: &SlotRef, what: &str) -> Result<(), StartError> {
        let ready = models.slots().iter().any(|s| s.info.slot == *slot && s.active.is_some());
        if ready {
            Ok(())
        } else {
            Err(StartError::not_ready(
                format!("no {what} model is installed ({slot})"),
                format!("install one in Settings > Models, or with model.install for the {slot} slot"),
            ))
        }
    }
}

/// Adds the co-host's buses to `engine`'s running mix when its graph lacks them: `ai_voice` over the two public feeds and
/// `ai_voice_private` over the private one, so the TTS host's feeds play.
///
/// # Errors
/// [`StartError::Failed`] when the mixer refuses the graph.
pub fn ensure_voice_buses(engine: &dyn AudioEngine) -> Result<(), StartError> {
    let graph = engine.graph();
    let id = |s: &str| BusId::new(s).map_err(StartError::failed);
    let feed = |s: &str| FeedId::new(s).map(|id| SourceRef::Feed { id }).map_err(StartError::failed);
    let (public, private) = (id(AI_VOICE_BUS)?, id(AI_VOICE_PRIVATE_BUS)?);
    if graph.bus(&public).is_some() && graph.bus(&private).is_some() {
        return Ok(());
    }
    let bus = |id: BusId, sources: Vec<SourceRef>| BusSpec {
        id,
        channels: Channels::Mono,
        sources,
        chain: vec![],
        gain_db: 0.0,
        muted: false,
        sends: vec![],
        outputs: vec![],
    };
    let mut next: GraphSpec = graph.clone();
    if graph.bus(&public).is_none() {
        next.buses.push(bus(public, vec![feed(FEEDS[0])?, feed(FEEDS[1])?]));
    }
    if graph.bus(&private).is_none() {
        next.buses.push(bus(private, vec![feed(FEEDS[2])?]));
    }
    engine.apply(&next).map_err(|e| StartError::failed(format!("the mixer refused the co-host's voice: {e}")))
}

/// Micro-dollars as dollars, for a person to read.
fn usd(amount: MicroUsd) -> f64 {
    // Amounts far below 2^52 micro-dollars.
    #[allow(clippy::cast_precision_loss)]
    let dollars = amount.0 as f64 / 1_000_000.0;
    dollars
}

/// Cents as dollars.
fn cents_usd(cents: u64) -> f64 {
    usd(MicroUsd(cents.saturating_mul(10_000)))
}

/// [`endpoint_host`] for an `&str`, as an owned name.
fn endpoint_host_of(endpoint: &str) -> Option<String> {
    endpoint_host(endpoint).map(str::to_owned)
}

/// The persona's own words for a reasoning model that should answer at once: Qwen3's soft switch. Other models take
/// no switch in the prompt; a server-side setting turns their reasoning off.
fn no_think(persona: &Persona, choice: &LlmChoice) -> Persona {
    let mut persona = persona.clone();
    let model = choice.model.as_deref().unwrap_or_default().to_ascii_lowercase();
    if !choice.reasoning && model.starts_with("qwen3") && !persona.style.contains("/no_think") {
        persona.style.push_str(" /no_think");
    }
    persona
}

impl CohostFactory for LocalFactory {
    fn sends_to(&self) -> Option<String> {
        let llm = self.choice();
        let decider = self.decider_choice();
        let realtime = self.realtime_choice();
        let mut hosts: Vec<String> = Vec::new();
        let mut add = |host: String| {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        };
        // With a realtime model, the local LLM isn't asked.
        let talking = (!realtime.on()).then_some(llm.endpoint.as_str());
        for endpoint in [talking, decider.endpoint()].into_iter().flatten() {
            if !is_loopback_endpoint(endpoint) {
                add(endpoint_host(endpoint).unwrap_or(endpoint).to_owned());
            }
        }
        if let Some((host, local)) = realtime.endpoint().and_then(omuzaki_llm_realtime::endpoint_host)
            && !local
        {
            add(host);
        }
        (!hosts.is_empty()).then(|| hosts.join(", "))
    }

    fn costs(&self) -> Option<Value> {
        let realtime = self.realtime_choice();
        realtime.on().then(|| {
            let (prices, known) = omuzaki_llm_realtime::prices(realtime.model());
            json!({
                "provider": omuzaki_llm_realtime::KIND,
                "model": realtime.model(),
                "listening_usd_per_minute": usd(prices.listening_per_minute()),
                "speaking_usd_per_minute": usd(prices.speaking_per_minute()),
                "cap_usd": cents_usd(realtime.cap_cents()),
                "prices_known": known,
            })
        })
    }

    fn check(&self) -> Result<(), StartError> {
        self.engine()?;
        let realtime = self.realtime_choice();
        if realtime.on() {
            // The model hears and speaks itself: only its key and address need to be right.
            return self.realtime_key(&realtime).map(drop);
        }
        let models = self.models()?;
        Self::slot_ready(models.as_ref(), &vad_slot(), "voice activity")?;
        Self::slot_ready(models.as_ref(), &asr_slot(), "speech recognition")?;
        Self::slot_ready(models.as_ref(), &tts_slot(), "text to speech")?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // The parts assembled in the order they depend on each other, undone on failure.
    fn build(&self, booking: &Booking) -> Result<Built, StartError> {
        let realtime = self.realtime_choice();
        if realtime.on() {
            return self.build_realtime(booking, &realtime);
        }
        self.check()?;
        let (engine, models) = (Arc::clone(self.engine()?), Arc::clone(self.models()?));
        let choice = self.choice();
        let client = self.client(&choice)?;
        let deciding = self.decider_choice();
        let decider = self.decider(&deciding, &choice, &client)?;
        ensure_voice_buses(engine.as_ref())?;
        let open = |slot: &SlotRef| models.open(slot, None).map_err(StartError::failed);

        let host = Arc::clone(&self.parts.host);
        let stage = Arc::new(PerformStage::new(Arc::clone(&host), booking.member.clone()));
        if let Some(mouth) = &self.mouth {
            mouth.detach();
        }
        let restore = self.mouth.clone();
        let give_back = move || {
            if let Some(mouth) = &restore
                && let Err(error) = mouth.attach()
            {
                tracing::warn!(%error, "the microphone did not get the mouth back");
            }
        };
        let lead = match attach_voice(host.as_ref(), LipSyncConfig::default()) {
            Ok(lead) => lead,
            Err(error) => {
                give_back();
                return Err(StartError::failed(format!("the character can't take the co-host's voice: {error}")));
            }
        };
        let built = (|| {
            let voice_id = booking.persona.voice.as_deref().unwrap_or(DEFAULT_VOICE);
            let voice = VoiceId::new(voice_id).map_err(|e| StartError::failed(format!("voice `{voice_id}`: {e}")))?;
            let echo = EchoLog::new();
            let mut tts = TtsHost::open(
                Arc::clone(&engine),
                open(&tts_slot())?,
                TtsVoice::new(voice.clone()),
                Some(Box::new(lead.clone())),
            )
            .map_err(StartError::failed)?;
            tts.spawn_pump().map_err(StartError::failed)?;
            tts.set_echo(echo.clone());
            let (mut speech, input) = SpeechHost::open(open(&vad_slot())?, open(&asr_slot())?, &HostConfig::default())
                .map_err(StartError::failed)?;
            speech.request_transcripts().keep();
            let request = TapRequest {
                point: TapPoint::Voice(VoiceUse::Listen),
                format: TapFormat { rate: TapRate::Speech16k, mono: true },
                capacity: Seconds(2.0),
                policy: DropPolicy::DropNewest,
                consumer: "co-host ears".into(),
                leaves_device: false,
            };
            let (tap, info) = match engine.tap(&request, Box::new(input)) {
                Ok(opened) => opened,
                Err(error) => {
                    speech.stop();
                    return Err(StartError::not_ready(
                        format!("the co-host can't hear the microphone: {error}"),
                        "choose a microphone in Settings > Audio",
                    ));
                }
            };
            // The tap's first sample on the engine clock, less what the chain before it delays: the ears' zero.
            let clock = engine.clock();
            let first = info.first_sample.saturating_mul(u64::from(GRAPH_RATE / info.rate.max(1)));
            let origin = Seconds(clock.time_of(first).0 - info.capture_offset.0);
            let ears = speech.ears(origin, echo);
            let persona = no_think(&booking.persona, &choice);
            let brain = LlmBrain::new(client, persona.clone());
            let mut parts = Parts::new(
                Box::new(brain),
                Box::new(ears),
                Box::new(tts),
                Arc::clone(&stage) as Arc<dyn omuzaki_performer_api::stage::Stage>,
            );
            if let Some(decider) = decider {
                parts = parts.decider(decider);
            }
            let config = ShowConfig::new(booking.member.clone(), voice.clone()).respond(booking.respond);
            let show = Show::new(&booking.persona, parts, config);
            let about = json!({
                "llm": {
                    "kind": choice.kind,
                    "endpoint_host": endpoint_host(&choice.endpoint),
                    "model": choice.model,
                    "local": is_loopback_endpoint(&choice.endpoint),
                },
                "decider": {
                    "kind": deciding.kind,
                    "endpoint_host": deciding.endpoint().and_then(endpoint_host),
                    "model": deciding.model,
                    "local": match deciding.endpoint() {
                        Some(endpoint) => is_loopback_endpoint(endpoint),
                        None => deciding.kind == DECIDER_RULES || is_loopback_endpoint(&choice.endpoint),
                    },
                },
                "voice": voice.to_string(),
                "hearing": { "rate": info.rate },
            });
            let stop = move || {
                drop(tap);
                speech.stop();
            };
            Ok((show, about, stop, clock))
        })();
        match built {
            Ok((show, about, stop, clock)) => {
                let cohost = Cohost::new(show, stage, Some(lead));
                Ok(Built::new(cohost, Arc::new(EngineTime(clock))).about(about).on_stop(move || {
                    stop();
                    give_back();
                }))
            }
            Err(error) => {
                give_back();
                Err(error)
            }
        }
    }
}
