//! Whisper through `whisper-rs` (whisper.cpp), CPU by default.

use std::path::Path;
use std::time::{Duration, Instant};

use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperState};

use crate::{SAMPLE_RATE, SpeechError};

/// How Whisper decodes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decoding {
    /// Greedy search, the fastest: the live-captions and conversation setting.
    Greedy,
    /// Beam search with the given width: slower, a little more accurate; for offline transcription.
    Beam(u8),
}

/// Per-call transcription settings.
#[derive(Clone, Debug, PartialEq)]
pub struct AsrOptions {
    /// Decoding strategy.
    pub decoding: Decoding,
    /// CPU threads for the encoder and decoder.
    pub threads: u32,
    /// `Some("en")` for English-only models; `None` lets multilingual models detect the language.
    pub language: Option<String>,
    /// Shrinks the encoder window to the audio length instead of a fixed 30 s (whisper.cpp's `audio_ctx`). It makes
    /// short segments several times faster, but the decoder then often fails to stop and repeats itself: on this
    /// spike's clips `tiny.en` went from 2.3 % to 91 % word error rate (see the README). Keep it off; it exists so
    /// the benchmark can re-measure the trade-off on future whisper.cpp versions.
    pub fit_audio_context: bool,
}

impl Default for AsrOptions {
    fn default() -> Self {
        Self { decoding: Decoding::Greedy, threads: 4, language: Some("en".into()), fit_audio_context: false }
    }
}

/// The result of one transcription.
#[derive(Clone, Debug, PartialEq)]
pub struct Transcript {
    /// The recognised text, segments joined.
    pub text: String,
    /// Segments as `(start_s, end_s, text)`, times relative to the start of the audio passed in.
    pub segments: Vec<(f32, f32, String)>,
    /// Wall-clock time `full()` took (mel, encoder, decoder).
    pub elapsed: Duration,
}

/// A loaded Whisper model plus a reusable decoding state.
pub struct Whisper {
    // Held for its lifetime: the state borrows nothing from it in Rust terms, but whisper.cpp needs the model alive.
    _context: WhisperContext,
    state: WhisperState,
}

impl Whisper {
    /// Loads a ggml model file on the CPU.
    ///
    /// # Errors
    /// [`SpeechError::PathNotUtf8`] for a non-UTF-8 path, [`SpeechError::Whisper`] when the model cannot be loaded.
    pub fn load(model: &Path) -> Result<Self, SpeechError> {
        let path = model.to_str().ok_or_else(|| SpeechError::PathNotUtf8(model.to_path_buf()))?;
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let context = WhisperContext::new_with_params(path, params)?;
        let state = context.create_state()?;
        Ok(Self { _context: context, state })
    }

    /// Transcribes 16 kHz mono audio (up to 30 s in one window; longer audio should be cut by the segmenter first).
    ///
    /// # Errors
    /// [`SpeechError::Whisper`] when whisper.cpp fails.
    pub fn transcribe(&mut self, audio: &[f32], options: &AsrOptions) -> Result<Transcript, SpeechError> {
        let strategy = match options.decoding {
            Decoding::Greedy => SamplingStrategy::Greedy { best_of: 1 },
            Decoding::Beam(width) => SamplingStrategy::BeamSearch { beam_size: i32::from(width), patience: -1.0 },
        };
        let mut params = FullParams::new(strategy);
        params.set_n_threads(i32::try_from(options.threads).unwrap_or(i32::MAX));
        params.set_language(options.language.as_deref());
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
        if options.fit_audio_context {
            params.set_audio_ctx(audio_context_for(audio.len()));
        }

        let started = Instant::now();
        self.state.full(params, audio)?;
        let elapsed = started.elapsed();

        let segments: Vec<(f32, f32, String)> = self
            .state
            .as_iter()
            .map(|s| {
                (s.start_timestamp() as f32 / 100.0, s.end_timestamp() as f32 / 100.0, s.to_string().trim().to_owned())
            })
            .collect();
        let text = segments.iter().map(|(_, _, t)| t.as_str()).collect::<Vec<_>>().join(" ");
        Ok(Transcript { text, segments, elapsed })
    }
}

/// The encoder `audio_ctx` for `samples` of audio: 50 positions per second (20 ms each), plus 25% headroom and a floor
/// of one second, rounded up to a multiple of 8. The full window is 1500 positions (30 s).
#[must_use]
pub fn audio_context_for(samples: usize) -> i32 {
    let positions = samples * 50 / SAMPLE_RATE as usize;
    let padded = (positions + positions / 4).max(50).div_ceil(8) * 8;
    i32::try_from(padded.min(1500)).unwrap_or(1500)
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

    #[test]
    fn audio_context_scales_with_length_and_caps_at_the_full_window() {
        assert_eq!(audio_context_for(0), 56); // floor of one second, rounded up to a multiple of 8
        assert_eq!(audio_context_for(3 * 16_000), 192); // 150 positions + 25%
        assert_eq!(audio_context_for(30 * 16_000), 1500);
        assert_eq!(audio_context_for(40 * 16_000), 1500);
    }
}
