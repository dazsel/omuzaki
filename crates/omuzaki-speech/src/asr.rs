//! Whisper through `whisper-rs` (whisper.cpp) on the CPU. A GPU backend would be another [`Recognizer`].
//!
//! The settings are the spike's latency bounds: greedy decoding, no carried-over context, one segment, a token
//! cap proportional to the audio and no temperature fallback. See `spikes/speech/README.md` for why `audio_ctx`
//! (shrinking the encoder window) is *not* used: it made the decoder repeat itself (word error rate 2 % to 91 %).

use std::path::Path;
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::{SAMPLE_RATE, SpeechError};

/// The result of one transcription.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    /// The recognised text, trimmed.
    pub text: String,
    /// Wall-clock time the recogniser took (mel, encoder, decoder).
    pub elapsed: Duration,
}

/// Something that turns one utterance of 16 kHz mono audio into text. [`Whisper`] is the real one; the host takes the
/// trait so its queues and back-pressure can be tested without a model.
pub trait Recognizer: Send {
    /// Transcribes `audio` (one utterance, at most 30 s). `language` is a Whisper language code such as `"en"`, or
    /// `None` to let a multilingual model detect it.
    ///
    /// # Errors
    /// Whatever the implementation reports.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<Transcript, SpeechError>;
}

/// A loaded Whisper model plus a reusable decoding state.
pub struct Whisper {
    // Held for its lifetime: the state borrows nothing from it in Rust terms, but whisper.cpp needs the model alive.
    _context: WhisperContext,
    state: WhisperState,
    threads: i32,
}

impl Whisper {
    /// Loads a ggml model file on the CPU; `threads` is used by the encoder and decoder.
    ///
    /// # Errors
    /// [`SpeechError::PathNotUtf8`] for a non-UTF-8 path, [`SpeechError::Whisper`] when the model cannot be loaded.
    pub fn load(model: &Path, threads: u32) -> Result<Self, SpeechError> {
        let path = model.to_str().ok_or_else(|| SpeechError::PathNotUtf8(model.to_path_buf()))?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let context = WhisperContext::new_with_params(path, params)?;
        let state = context.create_state()?;
        Ok(Self { _context: context, state, threads: i32::try_from(threads).unwrap_or(i32::MAX) })
    }
}

impl Recognizer for Whisper {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<Transcript, SpeechError> {
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_n_threads(self.threads);
        params.set_language(language);
        params.set_translate(false);
        // One utterance in, one window out: no timestamp tokens to decode and no carried-over text context. Each
        // call is independent, so results never depend on what was transcribed before.
        params.set_single_segment(true);
        params.set_no_timestamps(true);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        // Bound the worst case. Whisper can fall into a repetition loop ("and said to him and said to him ...") that
        // runs until the 224-token decoder limit, taking tens of seconds; a cap proportional to the audio keeps the
        // latency bounded. The default temperature fallback re-decodes a failed window up to six times, so it is
        // switched off: a live pipeline would rather show a flawed line now than a better one seconds later.
        params.set_max_tokens(max_tokens_for(audio.len()));
        params.set_temperature_inc(0.0);

        let started = Instant::now();
        self.state.full(params, audio)?;
        let elapsed = started.elapsed();

        let text = self.state.as_iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" ").trim().to_owned();
        Ok(Transcript { text, elapsed })
    }
}

/// Most tokens Whisper may emit for `samples` of audio: 8 per second (speech runs at about 4 to 5 tokens a second)
/// plus 16 for short utterances and punctuation.
#[must_use]
pub fn max_tokens_for(samples: usize) -> i32 {
    i32::try_from(samples * 8 / SAMPLE_RATE as usize + 16).unwrap_or(i32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_cap_grows_with_length() {
        assert_eq!(max_tokens_for(0), 16);
        assert_eq!(max_tokens_for(5 * 16_000), 56);
    }
}
