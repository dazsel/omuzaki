//! The co-host as the studio builds it, from the app's own services: the speech models the person chose in the
//! model registry (Silero VAD, Whisper, Kokoro), the microphone and the mixer of the audio engine, the LLM of the
//! `performer.llm` setting, and the perform host that moves the character. [`MicMouth`] gives the character's mouth
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

use omuzaki_ai_api::{ModelRegistry, SlotRef};
use omuzaki_audio_api::clock::EngineClock;
use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::{AI_VOICE_BUS, BusSpec, Channels, GraphSpec, SourceRef};
use omuzaki_audio_api::ids::{BusId, FeedId};
use omuzaki_audio_api::tap::{DropPolicy, TapFormat, TapPoint, TapRate, TapRequest, VoiceUse};
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_llm_api::LlmClient;
use omuzaki_llm_api::provider::{
    ProviderConfig, ProviderEnv, RuntimeHandle, SecretLabel, SecretRef, SecretStore, SecretValue, endpoint_host,
    is_loopback_endpoint, provider,
};
use omuzaki_performer::brain::LlmBrain;
use omuzaki_performer::persona::Persona;
use omuzaki_performer::show::{Parts, Show, ShowConfig};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_settings_api::service::Settings;
use omuzaki_speech::slots::{asr_slot, tts_slot, vad_slot};
use omuzaki_speech::tts::FEEDS;
use omuzaki_speech::{EchoLog, HostConfig, SpeechHost, TtsHost, TtsVoice};
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_track_voice::{LipSyncConfig, VoiceAnalyzer, VoiceListener, VowelProfile, voice_mailbox};
use serde_json::json;

use crate::service::{Booking, Built, CohostFactory, StartError};
use crate::settings::{LlmChoice, PerformerLlm};
use crate::stage::PerformStage;
use crate::{Cohost, attach_voice};

/// The voice a persona without one speaks in: Kokoro's `af_heart`.
pub const DEFAULT_VOICE: &str = "kokoro-82m/af_heart";
/// The bus the co-host's private lines play on (heard on the monitor only).
pub const AI_VOICE_PRIVATE_BUS: &str = "ai_voice_private";
/// The engine's graph rate.
const GRAPH_RATE: u32 = 48_000;

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
        let secrets: Arc<dyn SecretStore> = match &self.parts.secrets {
            Some(secrets) => Arc::new(Keychain(Arc::clone(secrets))),
            None => Arc::new(NoKeys),
        };
        let env = ProviderEnv::new(secrets, self.runtime()?);
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
        let choice = self.choice();
        if is_loopback_endpoint(&choice.endpoint) {
            None
        } else {
            Some(endpoint_host(&choice.endpoint).unwrap_or(choice.endpoint.as_str()).to_owned())
        }
    }

    fn check(&self) -> Result<(), StartError> {
        self.engine()?;
        let models = self.models()?;
        Self::slot_ready(models.as_ref(), &vad_slot(), "voice activity")?;
        Self::slot_ready(models.as_ref(), &asr_slot(), "speech recognition")?;
        Self::slot_ready(models.as_ref(), &tts_slot(), "text to speech")?;
        Ok(())
    }

    #[allow(clippy::too_many_lines)] // The parts assembled in the order they depend on each other, undone on failure.
    fn build(&self, booking: &Booking) -> Result<Built, StartError> {
        self.check()?;
        let (engine, models) = (Arc::clone(self.engine()?), Arc::clone(self.models()?));
        let choice = self.choice();
        let client = self.client(&choice)?;
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
            let parts = Parts::new(
                Box::new(brain),
                Box::new(ears),
                Box::new(tts),
                Arc::clone(&stage) as Arc<dyn omuzaki_performer_api::stage::Stage>,
            );
            let config = ShowConfig::new(booking.member.clone(), voice.clone()).respond(booking.respond);
            let show = Show::new(&booking.persona, parts, config);
            let about = json!({
                "llm": {
                    "kind": choice.kind,
                    "endpoint_host": endpoint_host(&choice.endpoint),
                    "model": choice.model,
                    "local": is_loopback_endpoint(&choice.endpoint),
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
