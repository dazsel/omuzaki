//! Speech to text on an `asr@1` session (Whisper through [`crate::whisper_runtime`] by default; a GPU or a person's own
//! model is another session in the same slot).

use std::time::{Duration, Instant};

use omuzaki_ai_api::{Session, TensorBuf, TensorRef};

use crate::SpeechError;
use crate::slots::asr_signature;

/// The result of one transcription.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    /// The recognised text, trimmed.
    pub text: String,
    /// Wall-clock time the recogniser took.
    pub elapsed: Duration,
}

/// Something that turns one utterance of 16 kHz mono audio into text. [`Whisper`] is the real one; the host takes the
/// trait so its queues and back-pressure can be tested without a model.
pub trait Recognizer: Send {
    /// Transcribes `audio` (one utterance, at most 30 s). `language` is a language code such as `"en"`, or `None` to
    /// let a multilingual model detect it.
    ///
    /// # Errors
    /// Whatever the implementation reports; [`SpeechError::Run`] with
    /// [`RunError::Unavailable`](omuzaki_ai_api::RunError::Unavailable) means "no answer this time", which the host
    /// counts and skips.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<Transcript, SpeechError>;
}

/// The output buffers of `asr@1` runs, kept between calls on any session of the slot.
pub struct AsrCall {
    outputs: Vec<TensorBuf>,
    /// The language the last transcription was in, as the model reported it.
    language: String,
}

impl Default for AsrCall {
    fn default() -> Self {
        Self::new()
    }
}

impl AsrCall {
    /// Fresh buffers.
    #[must_use]
    pub fn new() -> Self {
        Self { outputs: TensorBuf::for_outputs(&asr_signature()), language: String::new() }
    }

    /// Transcribes one utterance on `session` (see [`Recognizer::transcribe`]).
    ///
    /// # Errors
    /// [`SpeechError::Run`] when the session fails, [`SpeechError::ModelOutput`] when its text isn't UTF-8.
    pub fn transcribe(
        &mut self,
        session: &mut dyn Session,
        audio: &[f32],
        language: Option<&str>,
    ) -> Result<Transcript, SpeechError> {
        let language = language.unwrap_or("").as_bytes();
        let started = Instant::now();
        let shape = [audio.len()];
        let language_shape = [language.len()];
        let inputs = [TensorRef::f32(audio, &shape), TensorRef::u8(language, &language_shape)];
        session.run(&inputs, &mut self.outputs)?;
        let elapsed = started.elapsed();
        let text = |buf: &TensorBuf| {
            std::str::from_utf8(buf.as_u8().unwrap_or_default())
                .map(|s| s.trim().to_owned())
                .map_err(|_| SpeechError::ModelOutput("the transcript isn't UTF-8".into()))
        };
        self.language = text(&self.outputs[1])?;
        Ok(Transcript { text: text(&self.outputs[0])?, elapsed })
    }

    /// The language of the last transcription, as the model reported it (`"en"`); empty before the first.
    #[must_use]
    pub fn language(&self) -> &str {
        &self.language
    }
}

/// An `asr@1` session (Whisper by default): one utterance in, its text (and language) out.
pub struct Whisper {
    session: Box<dyn Session>,
    call: AsrCall,
}

impl Whisper {
    /// Wraps a session the registry opened for `asr@1`.
    ///
    /// # Errors
    /// [`SpeechError::Signature`] when the session presents another slot's signature.
    pub fn new(session: Box<dyn Session>) -> Result<Self, SpeechError> {
        if session.signature() != &asr_signature() {
            return Err(SpeechError::Signature("asr@1"));
        }
        Ok(Self { session, call: AsrCall::new() })
    }

    /// The language of the last transcription, as the model reported it (`"en"`); empty before the first.
    #[must_use]
    pub fn language(&self) -> &str {
        self.call.language()
    }
}

impl Recognizer for Whisper {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<Transcript, SpeechError> {
        self.call.transcribe(self.session.as_mut(), audio, language)
    }
}
