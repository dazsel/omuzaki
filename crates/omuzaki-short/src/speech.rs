//! The project's local text-to-speech as a short's [`Speaker`]: a `tts@1` session (Kokoro-82M, shipped as
//! `kokoro-82m`) opened through the model registry, the same path the co-host's voice and `voice.preview` take.
//!
//! A short's voice passes the same gate a character's voice assignment does ([`admit`]): the voice is one the catalog
//! lists, its speed is in range, and a cloned or imitating voice names a consent record the app still finds.

use omuzaki_ai_api::{ConsentLookup, ModelRegistry, NoConsentRecords, Session};
use omuzaki_char_api::CharId;
use omuzaki_ops_api::{JobCtx, OpCtx, OpError};
use omuzaki_performer_api::ids::VoiceId;
use omuzaki_performer_api::speech::VoiceError;
use omuzaki_performer_api::voice::{AssignError, VoiceAssignment, VoiceCatalog, VoiceInfo, VoiceProvider};
use omuzaki_speech::slots::tts::{TTS_RATE, TtsCall, Utterance, resample};
use omuzaki_speech::tts::trim;
use omuzaki_speech::voices::catalog::local_voices;
use omuzaki_speech::voices::{admit, open_session};

use crate::error::ShortError;
use crate::voice::{RATE, Speaker, Spoken};

/// What a person installs for a spoken short, for error hints.
const INSTALL_HINT: &str = "install a text-to-speech model (Kokoro, `kokoro-82m`) in Settings > Models; model.list \
                            shows each model's status, voice.list the voices";

/// The local voices of the registry's `tts@1` models, when the app wired no catalog of its own.
struct RegistryVoices<'a>(&'a dyn ModelRegistry);

impl VoiceCatalog for RegistryVoices<'_> {
    fn voices(&self, provider: &VoiceProvider) -> Result<Vec<VoiceInfo>, VoiceError> {
        match provider {
            VoiceProvider::Local => Ok(local_voices(self.0).into_iter().map(|v| v.info).collect()),
            other => Err(VoiceError::Provider(format!("{other:?} isn't a local voice provider"))),
        }
    }
}

/// The services a spoken short reads from an op's context: the model registry, the voice catalog (the registry's
/// local voices when the app wired none) and the consent records (none, so every cloned voice is refused, when the app
/// wired none).
#[derive(Clone, Copy, Default)]
pub struct SpeechServices<'a> {
    /// `dyn ModelRegistry`.
    pub registry: Option<&'a dyn ModelRegistry>,
    /// `dyn VoiceCatalog`.
    pub catalog: Option<&'a dyn VoiceCatalog>,
    /// `dyn ConsentLookup`.
    pub consent: Option<&'a dyn ConsentLookup>,
}

impl<'a> SpeechServices<'a> {
    /// The services of an op's plan.
    #[must_use]
    pub fn of_op(ctx: &OpCtx<'a>) -> Self {
        Self {
            registry: ctx.service::<dyn ModelRegistry>().ok(),
            catalog: ctx.service::<dyn VoiceCatalog>().ok(),
            consent: ctx.service::<dyn ConsentLookup>().ok(),
        }
    }

    /// The services of a job's work.
    #[must_use]
    pub fn of_job(job: &JobCtx<'a>) -> Self {
        Self {
            registry: job.service::<dyn ModelRegistry>().ok(),
            catalog: job.service::<dyn VoiceCatalog>().ok(),
            consent: job.service::<dyn ConsentLookup>().ok(),
        }
    }
}

/// Says lines on a `tts@1` session of the registry, opening the session of a voice's model on first use.
pub struct TtsSpeaker<'a> {
    registry: &'a dyn ModelRegistry,
    /// The open session and the model it runs.
    open: Option<(String, Box<dyn Session>)>,
    call: TtsCall,
}

impl<'a> TtsSpeaker<'a> {
    /// A speaker over `registry`'s `tts@1` models.
    #[must_use]
    pub fn new(registry: &'a dyn ModelRegistry) -> Self {
        Self { registry, open: None, call: TtsCall::new() }
    }

    /// The speaker for `character` saying lines in `voice` at `speed`, over an op's services. The voice is admitted
    /// first.
    ///
    /// # Errors
    /// [`OpError`] `Unavailable` without a model registry; `NotFound`, `Invalid` or `Denied` when the voice isn't
    /// admitted, with a hint saying what to install or change.
    pub fn for_op(services: &SpeechServices<'a>, character: &CharId, voice: &str, speed: f32) -> Result<Self, OpError> {
        let registry = services.registry.ok_or_else(|| {
            OpError::unavailable("dyn ModelRegistry").hint(format!(
                "a spoken short needs the model registry to synthesise its lines, and this app has none: use a \
                 `silent` or `file` voice here, or render it in the studio. {INSTALL_HINT}"
            ))
        })?;
        let id = VoiceId::new(voice)
            .map_err(|e| OpError::invalid("voice.voice", format!("{voice:?} isn't a voice id: {}", e.rule)))?;
        let assignment = VoiceAssignment::new(character.clone(), id, VoiceProvider::Local).tuned(speed, 0.0);
        let local = RegistryVoices(registry);
        let catalog: &dyn VoiceCatalog = services.catalog.unwrap_or(&local);
        admit(&assignment, catalog, services.consent.unwrap_or(&NoConsentRecords)).map_err(|e| refused(&e))?;
        Ok(Self::new(registry))
    }

    /// Opens a session on `voice`'s model unless one is open.
    fn open(&mut self, voice: &VoiceId) -> Result<(), ShortError> {
        let model = voice.source();
        if self.open.as_ref().is_none_or(|(open, _)| open != model) {
            let session =
                open_session(self.registry, voice).map_err(|e| ShortError::Voice(format!("{e}; {INSTALL_HINT}")))?;
            self.open = Some((model.to_owned(), session));
        }
        Ok(())
    }
}

impl Speaker for TtsSpeaker<'_> {
    /// Synthesises at 24 kHz, trims the silence around the words to the TTS host's margin, and resamples to [`RATE`]
    /// with a windowed-sinc filter.
    fn speak(&mut self, text: &str, voice: &str, speed: f32) -> Result<Spoken, ShortError> {
        let id = VoiceId::new(voice).map_err(|e| ShortError::Voice(format!("{voice:?}: {}", e.rule)))?;
        self.open(&id)?;
        let Self { open: Some((_, session)), call, .. } = self else {
            return Err(ShortError::Voice(format!("{voice}'s session didn't open")));
        };
        let utterance = Utterance { text, voice: id.voice(), speed, ..Utterance::plain(text) };
        let pcm = call
            .speak(session.as_mut(), &utterance)
            .map_err(|e| ShortError::Voice(format!("{voice} couldn't say {text:?}: {e}")))?;
        Ok(Spoken { pcm: resample(trim(pcm, TTS_RATE), TTS_RATE, RATE), rate: RATE })
    }
}

/// The op error for a voice `admit` refused, worded so a person or an agent knows what to change.
fn refused(e: &AssignError) -> OpError {
    let message = e.to_string();
    match e {
        AssignError::Speed { .. } => OpError::invalid("voice.speed", message),
        AssignError::Voice(VoiceError::UnknownVoice { .. }) => {
            let mut error = OpError::not_found(message).hint(format!("voice.list lists the voices; {INSTALL_HINT}"));
            error.field = Some("voice.voice".into());
            error
        }
        AssignError::Voice(VoiceError::NoConsent { .. }) => OpError::denied(message).hint(
            "a cloned or imitating voice needs a consent record from the person it sounds like; pick a stock voice",
        ),
        _ => OpError::invalid("voice.voice", message),
    }
}
