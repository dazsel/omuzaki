//! The `whisper-ggml` runtime: Whisper models in whisper.cpp's ggml format, through `whisper-rs`, on the CPU. Its
//! sessions present `asr@1`.
//!
//! The settings are the spike's latency bounds: greedy decoding, no carried-over context, one segment, a token cap
//! proportional to the audio and no temperature fallback. See `spikes/speech/README.md` for why `audio_ctx` (shrinking
//! the encoder window) is *not* used: it made the decoder repeat itself (word error rate 2 % to 91 %).

use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;

use omuzaki_ai_api::{
    LoadError, ModelRef, ModelSource, Probe, Provider, RunError, Runtime, RuntimeId, Session, SessionInfo,
    SessionOptions, Signature, TensorBuf, TensorData, TensorDataMut, TensorRef,
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperError, WhisperState};

use crate::SAMPLE_RATE;
use crate::slots::asr_signature;

/// The runtime's id.
pub const WHISPER_GGML: &str = "whisper-ggml";

/// `ggml` as whisper.cpp writes it: the little-endian `u32` 0x67676d6c.
const MAGIC: u32 = 0x6767_6d6c;

/// The header after the magic: eleven `i32`s (vocabulary, audio context, state, heads, layers, text context, state,
/// heads, layers, mel bands, weight type).
const HEADER: usize = 4 + 11 * 4;

/// The `whisper-ggml` runtime.
#[derive(Default)]
pub struct WhisperGgml;

omuzaki_ai_api::register_runtime!(WhisperGgml);

/// What the header says: enough to know it is a Whisper model and not something else with the magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    /// Tokens in the vocabulary.
    pub vocabulary: i32,
    /// Encoder layers.
    pub audio_layers: i32,
    /// Decoder layers.
    pub text_layers: i32,
    /// Mel bands: 80, or 128 for large-v3.
    pub mels: i32,
}

/// Reads and checks a ggml Whisper header without loading the model.
///
/// # Errors
/// [`LoadError::Format`] for anything that isn't one.
#[allow(
    clippy::disallowed_methods,
    reason = "a model file the registry fetched and checked by hash, not a file an op names; the fence is for ops"
)]
pub fn read_header(path: &Path) -> Result<Header, LoadError> {
    let mut bytes = [0u8; HEADER];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut bytes))
        .map_err(|e| LoadError::Format(format!("can't read a ggml header: {e}")))?;
    let word = |i: usize| -> [u8; 4] { [bytes[4 * i], bytes[4 * i + 1], bytes[4 * i + 2], bytes[4 * i + 3]] };
    if u32::from_le_bytes(word(0)) != MAGIC {
        return Err(LoadError::Format("not a ggml file (wrong magic)".into()));
    }
    let field = |i: usize| i32::from_le_bytes(word(i + 1));
    let header = Header { vocabulary: field(0), audio_layers: field(4), text_layers: field(8), mels: field(9) };
    let sane = (1..=1_000_000).contains(&header.vocabulary)
        && (1..=64).contains(&header.audio_layers)
        && (1..=64).contains(&header.text_layers)
        && matches!(header.mels, 80 | 128)
        && (0..11).all(|i| field(i) > 0 || i == 10);
    if !sane {
        return Err(LoadError::Format(format!("not a Whisper ggml model: {header:?}")));
    }
    Ok(header)
}

impl Runtime for WhisperGgml {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(WHISPER_GGML).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        let main = source.files.first().ok_or_else(|| LoadError::MissingFile("the ggml model".into()))?;
        read_header(&main.path)?;
        Ok(Probe { signature: asr_signature(), opset: None })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        _io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError> {
        if slot != &asr_signature() {
            return Err(LoadError::Format("whisper-ggml models only fit asr@1".into()));
        }
        let main = source.files.first().ok_or_else(|| LoadError::MissingFile("the ggml model".into()))?;
        read_header(&main.path)?;
        let path = main.path.to_str().ok_or_else(|| LoadError::Format("the model's path isn't UTF-8".into()))?;
        let mut parameters = WhisperContextParameters::default();
        parameters.use_gpu(false);
        let context = WhisperContext::new_with_params(path, parameters).map_err(runtime)?;
        let state = context.create_state().map_err(runtime)?;
        Ok(Box::new(WhisperSession {
            _context: context,
            state,
            threads: i32::from(options.threads.max(1)),
            signature: asr_signature(),
            model: source.model.clone(),
        }))
    }
}

fn runtime(error: WhisperError) -> LoadError {
    LoadError::Runtime(error.to_string())
}

/// A loaded Whisper model and its reusable decoding state.
struct WhisperSession {
    // Held for its lifetime: the state borrows nothing from it in Rust terms, but whisper.cpp needs the model alive.
    _context: WhisperContext,
    state: WhisperState,
    threads: i32,
    signature: Signature,
    model: ModelRef,
}

impl WhisperSession {
    /// The text of `audio`, in `language` (`None` detects it), and the language used. A short line is encoded with a
    /// short context first; if that text loops ("clip clip clip ..."), the line is read again with the full window.
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<(String, String), WhisperError> {
        let context = audio_ctx_for(audio.len());
        let (text, used) = self.transcribe_with(audio, language, context)?;
        if context < 1_500 && loops(&text) {
            return self.transcribe_with(audio, language, 1_500);
        }
        Ok((text, used))
    }

    fn transcribe_with(
        &mut self,
        audio: &[f32],
        language: Option<&str>,
        context: i32,
    ) -> Result<(String, String), WhisperError> {
        if audio.is_empty() {
            return Ok((String::new(), language.unwrap_or("").to_owned()));
        }
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
        // Whisper encodes a 30-second window whatever the utterance's length, so a two-second command costs as much
        // as a long sentence. Encoding only the part that holds audio (and a margin) cuts that cost for short lines.
        params.set_audio_ctx(context);
        self.state.full(params, audio)?;
        let text = self.state.as_iter().map(|s| s.to_string()).collect::<Vec<_>>().join(" ").trim().to_owned();
        let used = match language {
            Some(language) => language.to_owned(),
            None => whisper_rs::get_lang_str(self.state.full_lang_id_from_state()).unwrap_or("").to_owned(),
        };
        Ok((text, used))
    }
}

/// The encoder frames Whisper reads for `samples` of 16 kHz audio: [`SHORT_CTX`] (about 10 s) for a line of at most
/// [`SHORT_LINE_S`], else the full window (1,500 frames, 30 s). A context only a little longer than the audio makes
/// Whisper repeat itself on long lines, so only short lines, with ample margin, get the shorter one.
fn audio_ctx_for(samples: usize) -> i32 {
    if samples as f32 / SAMPLE_RATE as f32 <= SHORT_LINE_S { SHORT_CTX } else { 1_500 }
}

/// Whether `text` repeats itself: one word four times in a row, or a run of two to eight words three times in a row,
/// the way Whisper loops when it loses its place.
fn loops(text: &str) -> bool {
    let words: Vec<String> = text
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()).to_lowercase())
        .filter(|w| !w.is_empty())
        .collect();
    (1..=8).any(|n| {
        let times = if n == 1 { 4 } else { 3 };
        words.len() >= n * times
            && (0..=words.len() - n * times).any(|start| {
                let first = &words[start..start + n];
                (1..times).all(|k| &words[start + k * n..start + (k + 1) * n] == first)
            })
    })
}

/// The longest line encoded with [`SHORT_CTX`], seconds.
const SHORT_LINE_S: f32 = 6.0;
/// The short lines' encoder context: 512 frames, 10.24 s.
const SHORT_CTX: i32 = 512;

/// Writes `text` into a `u8[any]` output.
fn write_text(out: &mut TensorBuf, index: usize, text: &str) -> Result<(), RunError> {
    let problem = |problem| RunError::Mismatch { port: omuzaki_ai_api::Port::Output, index, problem };
    if let TensorDataMut::U8(bytes) = out.resize(&[text.len()]).map_err(problem)? {
        bytes.copy_from_slice(text.as_bytes());
    }
    Ok(())
}

impl Session for WhisperSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        let (TensorData::F32(audio), TensorData::U8(language)) = (inputs[0].data, inputs[1].data) else {
            return Err(RunError::Failed("asr@1 takes f32 audio and a UTF-8 language".into()));
        };
        let language =
            std::str::from_utf8(language).map_err(|_| RunError::Failed("the language isn't UTF-8".into()))?;
        let language = (!language.is_empty()).then_some(language);
        let (text, used) = self.transcribe(audio, language).map_err(|e| RunError::Failed(e.to_string()))?;
        write_text(&mut outputs[0], 0, &text)?;
        write_text(&mut outputs[1], 1, &used)
    }

    fn reset(&mut self) {}

    fn info(&self) -> SessionInfo {
        SessionInfo { model: self.model.clone(), provider: Provider::Cpu, sandboxed: false, fallback_for: None }
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
    fn a_looping_transcript_is_caught() {
        assert!(loops("clip clip clip clip clip clip"));
        assert!(loops(
            "When he cried out for mercy and said. When he cried out for mercy and said. When he cried out for mercy and said."
        ));
        assert!(!loops("Clip that, please."));
        assert!(!loops("no no no, that was close"));
        assert!(!loops(""));
        assert_eq!((audio_ctx_for(16_000), audio_ctx_for(16_000 * 7)), (SHORT_CTX, 1_500));
    }

    #[test]
    fn token_cap_grows_with_length() {
        assert_eq!(max_tokens_for(0), 16);
        assert_eq!(max_tokens_for(5 * 16_000), 56);
    }

    #[test]
    #[allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]
    fn a_header_is_checked_without_loading_the_model() {
        let dir = std::env::temp_dir().join(format!("omuzaki-ggml-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let write = |name: &str, magic: u32, fields: [i32; 11]| {
            let mut bytes = magic.to_le_bytes().to_vec();
            for f in fields {
                bytes.extend(f.to_le_bytes());
            }
            let path = dir.join(name);
            std::fs::write(&path, bytes).unwrap();
            path
        };
        // tiny.en's header.
        let tiny = [51_864, 1500, 384, 6, 4, 448, 384, 6, 4, 80, 1];
        assert_eq!(
            read_header(&write("tiny.bin", MAGIC, tiny)).unwrap(),
            Header { vocabulary: 51_864, audio_layers: 4, text_layers: 4, mels: 80 }
        );
        assert!(matches!(read_header(&write("onnx.bin", 0x0807_0102, tiny)), Err(LoadError::Format(_))));
        let mut odd = tiny;
        odd[9] = 7;
        assert!(matches!(read_header(&write("odd.bin", MAGIC, odd)), Err(LoadError::Format(_))));
        assert!(matches!(read_header(&dir.join("missing.bin")), Err(LoadError::Format(_))));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
