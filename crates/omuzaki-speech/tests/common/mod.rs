//! Shared test helpers: fakes for the VAD and the recogniser. The tests on real models are in
//! `omuzaki-test/tests/speech_models.rs`, where the registry, the `onnx` runtime and this crate meet.
#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::needless_pass_by_value,
    clippy::type_complexity
)] // shared by several test binaries; panics are test failures

use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_speech::SpeechError;
use omuzaki_speech::asr::{Recognizer, Transcript};
use omuzaki_speech::host::{AudioInput, SpeechEvent, SpeechHost};
use omuzaki_speech::segmenter::FRAME;
use omuzaki_speech::vad::VoiceActivity;

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
