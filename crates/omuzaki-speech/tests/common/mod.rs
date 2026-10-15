//! Shared test helpers: fakes for the VAD and the recogniser, and the gates for tests that need real models.
//!
//! Heavy files live outside git (docs/decisions/0003). Tests that need them read `OMUZAKI_SPEECH_MODELS` (and
//! `OMUZAKI_SPEECH_DATA` for the public-domain speech clips) and skip with a message when it is unset or the file is
//! absent; they never download anything.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::needless_pass_by_value,
    clippy::type_complexity
)] // shared by several test binaries; panics are test failures

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_speech::asr::{Recognizer, Transcript};
use omuzaki_speech::host::{AudioInput, SpeechEvent, SpeechHost};
use omuzaki_speech::segmenter::FRAME;
use omuzaki_speech::vad::{VoiceActivity, load_runtime};
use omuzaki_speech::{SileroVad, SpeechError};

/// Scores a frame by its energy: 0.9 above an RMS of 0.02, else 0.0. Deterministic and instant.
pub struct EnergyVad {
    /// Extra time spent per frame, to make the VAD thread the slow one.
    pub delay: Duration,
}

impl VoiceActivity for EnergyVad {
    fn probability(&mut self, frame: &[f32; FRAME]) -> Result<f32, SpeechError> {
        if !self.delay.is_zero() {
            std::thread::sleep(self.delay);
        }
        let rms = (frame.iter().map(|s| s * s).sum::<f32>() / FRAME as f32).sqrt();
        Ok(if rms > 0.02 { 0.9 } else { 0.0 })
    }

    fn reset(&mut self) {}
}

/// A recogniser that returns `"<language> <samples>"`, optionally slowly, optionally failing the first N calls.
pub struct FakeRecognizer {
    pub delay: Duration,
    pub fail_first: usize,
    pub calls: Arc<Mutex<Vec<(usize, Option<String>)>>>,
}

impl FakeRecognizer {
    pub fn new(delay: Duration) -> Self {
        Self { delay, fail_first: 0, calls: Arc::default() }
    }
}

impl Recognizer for FakeRecognizer {
    fn transcribe(&mut self, audio: &[f32], language: Option<&str>) -> Result<Transcript, SpeechError> {
        std::thread::sleep(self.delay);
        let mut calls = self.calls.lock().unwrap();
        calls.push((audio.len(), language.map(str::to_owned)));
        if calls.len() <= self.fail_first {
            return Err(SpeechError::ModelOutput("scripted failure".into()));
        }
        Ok(Transcript { text: format!("{} {}", language.unwrap_or("auto"), audio.len()), elapsed: self.delay })
    }
}

/// Starts a host over the fakes.
pub fn fake_host(
    config: omuzaki_speech::HostConfig,
    vad_delay: Duration,
    recognizer: FakeRecognizer,
) -> (SpeechHost, AudioInput) {
    let (host, input) =
        SpeechHost::start(&config, Box::new(EnergyVad { delay: vad_delay }), Box::new(recognizer)).unwrap();
    host.request_transcripts().keep();
    (host, input)
}

/// Pushes `audio` in 10 ms blocks (what a sound card callback delivers) and returns how many samples were refused.
pub fn feed(input: &mut AudioInput, audio: &[f32]) -> usize {
    audio.chunks(160).map(|block| block.len() - input.push(block)).sum()
}

/// Pushes `audio` without ever overflowing the ring: waits for room before each 10 ms block.
pub fn feed_all(input: &mut AudioInput, audio: &[f32]) {
    for block in audio.chunks(160) {
        while input.free() < block.len() {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert_eq!(input.push(block), block.len());
    }
}

/// Every event still queued.
pub fn drain(host: &SpeechHost) -> Vec<SpeechEvent> {
    std::iter::from_fn(|| host.try_next_event()).collect()
}

/// `a` followed by `b`.
pub fn cat(parts: &[&[f32]]) -> Vec<f32> {
    parts.concat()
}

/// The model directory from `OMUZAKI_SPEECH_MODELS`, or `None` (with a message) so the test skips.
pub fn models_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("OMUZAKI_SPEECH_MODELS").map(PathBuf::from);
    if dir.is_none() {
        eprintln!(
            "SKIPPED: OMUZAKI_SPEECH_MODELS is not set (point it at the folder with silero_vad.onnx and the ggml files)"
        );
    }
    dir
}

/// The public-domain speech clips from `OMUZAKI_SPEECH_DATA`, or `None` (with a message).
pub fn data_dir() -> Option<PathBuf> {
    let dir = std::env::var_os("OMUZAKI_SPEECH_DATA").map(PathBuf::from);
    if dir.is_none() {
        eprintln!("SKIPPED: OMUZAKI_SPEECH_DATA is not set (the LibriVox clips from spikes/speech)");
    }
    dir
}

/// `dir/name`, or `None` (with a message) when it does not exist.
pub fn existing(dir: &std::path::Path, name: &str) -> Option<PathBuf> {
    let path = dir.join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("SKIPPED: {} not found", path.display());
        None
    }
}

/// The ONNX Runtime library: `ORT_DYLIB_PATH`, or the copy unpacked next to the models.
pub fn ort_library(models: &std::path::Path) -> PathBuf {
    std::env::var_os("ORT_DYLIB_PATH")
        .map_or_else(|| models.join("onnxruntime-linux-x64-1.28.3/lib/libonnxruntime.so"), PathBuf::from)
}

/// A real Silero VAD session, or `None` (skip) when `OMUZAKI_SPEECH_MODELS` or a file is missing.
pub fn silero() -> Option<SileroVad> {
    let models = models_dir()?;
    let runtime = ort_library(&models);
    if !runtime.is_file() {
        eprintln!("SKIPPED: {} not found", runtime.display());
        return None;
    }
    let model = existing(&models, "silero_vad.onnx")?;
    load_runtime(&runtime).unwrap();
    Some(SileroVad::new(&model).unwrap())
}

/// Reads a 16 kHz mono WAV as `f32`.
pub fn read_wav(path: &std::path::Path) -> Vec<f32> {
    let mut reader = hound::WavReader::open(path).unwrap();
    let spec = reader.spec();
    assert_eq!((spec.sample_rate, spec.channels), (16_000, 1), "{}", path.display());
    match spec.sample_format {
        hound::SampleFormat::Float => reader.samples::<f32>().map(Result::unwrap).collect(),
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            reader.samples::<i32>().map(|s| s.unwrap() as f32 * scale).collect()
        }
    }
}

/// A VAD that fails on every frame, or panics on the first one.
pub struct BrokenVad {
    pub panic: bool,
}

impl VoiceActivity for BrokenVad {
    fn probability(&mut self, _frame: &[f32; FRAME]) -> Result<f32, SpeechError> {
        assert!(!self.panic, "scripted VAD panic");
        Err(SpeechError::ModelOutput("scripted VAD failure".into()))
    }

    fn reset(&mut self) {}
}

/// A recogniser that panics.
pub struct PanickingRecognizer;

impl Recognizer for PanickingRecognizer {
    fn transcribe(&mut self, _audio: &[f32], _language: Option<&str>) -> Result<Transcript, SpeechError> {
        panic!("scripted ASR panic");
    }
}
