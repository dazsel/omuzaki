//! Fakes for the AI performer's voice (`ai-performer-voice-face.md` §6 and §7): [`FakeTts`] for `tts@1` and
//! [`FakeVoiceToFace`] for `voice_to_face@1`. Their outputs are exact functions of their inputs, so the TTS host and the
//! mouth drive test against numbers worked out by hand (§6: "Hi chat!" is 11,520 samples, a vowel opens the jaw to 0.53
//! and a consonant to 0.14).

// Sample counts and indices here stay far below 2^24, where `f32` is still exact.
#![allow(clippy::cast_precision_loss)]

use std::collections::BTreeSet;
use std::f32::consts::TAU;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::Receiver;
use std::time::Duration;

use omuzaki_ai_api::{
    DType, Dim, Port, Provider, RunError, Session, SessionInfo, Signature, StreamSpec, TensorBuf, TensorData,
    TensorDataMut, TensorRef, TensorSpec,
};

use crate::session::fake_model;

/// [`FakeTts`]'s rate: `tts@1`'s 24 kHz.
pub const FAKE_TTS_RATE: u32 = 24_000;
/// Samples per character at speed 1: 60 ms.
pub const SAMPLES_PER_CHAR: usize = 1_440;
/// The tone's pitch.
pub const TONE_HZ: f32 = 200.0;
/// A vowel's amplitude.
pub const VOWEL: f32 = 0.3;
/// Any other letter's or digit's amplitude.
pub const CONSONANT: f32 = 0.08;

/// [`FakeVoiceToFace`]'s rate and block: 20 ms at 16 kHz.
pub const FACE_RATE: u32 = 16_000;
/// Samples per block.
pub const FACE_BLOCK: u32 = 320;
/// `jawOpen`'s place in the ARKit-52 order (`omuzaki-anim-api`'s `Channel::JAW_OPEN`).
pub const JAW_OPEN: usize = 24;
/// Channels out: the 52 of `ARKit`.
pub const ARKIT: usize = 52;

/// `tts@1`'s signature: `text u8[any]`, `voice u8[any]`, `speed f32[]`, `expressiveness f32[]` → `pcm f32[any]`.
/// (The slot itself lives in `omuzaki-speech`, which tests that the two agree.)
pub fn tts_signature() -> Signature {
    let any = |name: &str, dtype| TensorSpec::new(name, dtype, [Dim::ANY]);
    let scalar = |name: &str| TensorSpec::new(name, DType::F32, Vec::new());
    Signature {
        inputs: vec![any("text", DType::U8), any("voice", DType::U8), scalar("speed"), scalar("expressiveness")],
        outputs: vec![any("pcm", DType::F32)],
    }
}

/// `voice_to_face@1`'s signature: `audio f32[any]` (one 16 kHz block) → `channels f32[52]`, in `ARKit` order.
pub fn voice_to_face_signature() -> Signature {
    Signature {
        inputs: vec![TensorSpec::new("audio", DType::F32, [Dim::ANY])],
        outputs: vec![TensorSpec::new("channels", DType::F32, [Dim::Fixed(ARKIT)])],
    }
}

/// The amplitude [`FakeTts`] gives `c`: [`VOWEL`], [`CONSONANT`] for any other letter or digit, 0 for the rest.
pub fn amplitude(c: char) -> f32 {
    if "aeiouAEIOU".contains(c) {
        VOWEL
    } else if c.is_alphanumeric() {
        CONSONANT
    } else {
        0.0
    }
}

/// The waveform [`FakeTts`] gives `text` at `speed`: per character, `round(1440 / speed)` samples of a 200 Hz tone at
/// its [`amplitude`], the tone's phase running on across characters.
pub fn fake_speech(text: &str, speed: f32) -> Vec<f32> {
    let per_char = per_char(speed);
    let mut out = Vec::with_capacity(text.chars().count() * per_char);
    for c in text.chars() {
        let a = amplitude(c);
        let start = out.len();
        out.extend((start..start + per_char).map(|n| a * (TAU * TONE_HZ * n as f32 / FAKE_TTS_RATE as f32).sin()));
    }
    out
}

fn per_char(speed: f32) -> usize {
    let speed = if speed.is_finite() && speed > 0.0 { speed.clamp(0.25, 4.0) } else { 1.0 };
    // At most 1440 / 0.25 = 5760: the cast is exact.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (SAMPLES_PER_CHAR as f32 / speed).round() as usize;
    n
}

/// A `tts@1` session that speaks [`fake_speech`]: speed honoured, expressiveness fed and ignored (like a model whose
/// manifest honours only `speed`). It can be scripted to take time, to fail on given calls, to wait for the test's go
/// on each call, and to know only some voices; it counts its calls.
pub struct FakeTts {
    signature: Signature,
    voices: Vec<String>,
    latency: Vec<Duration>,
    fail: BTreeSet<usize>,
    gate: Option<Receiver<()>>,
    calls: Arc<AtomicUsize>,
}

impl Default for FakeTts {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeTts {
    /// Any voice, no latency, no failures.
    pub fn new() -> Self {
        Self {
            signature: tts_signature(),
            voices: Vec::new(),
            latency: Vec::new(),
            fail: BTreeSet::new(),
            gate: None,
            calls: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Only these voices (and the empty voice, the first of them); any other is an error.
    #[must_use]
    pub fn with_voices(mut self, voices: &[&str]) -> Self {
        self.voices = voices.iter().map(|v| (*v).to_owned()).collect();
        self
    }

    /// Call `n` takes `latency[n]` (the last entry for every later call) before it returns.
    #[must_use]
    pub fn with_latency(mut self, latency: Vec<Duration>) -> Self {
        self.latency = latency;
        self
    }

    /// The calls with these numbers (from 0) fail with [`RunError::Failed`].
    #[must_use]
    pub fn failing_on(mut self, calls: impl IntoIterator<Item = usize>) -> Self {
        self.fail = calls.into_iter().collect();
        self
    }

    /// Every call waits for one message on `gate` before it synthesises (a closed gate lets every call through), so
    /// a test decides exactly when synthesis finishes instead of sleeping.
    #[must_use]
    pub fn gated(mut self, gate: Receiver<()>) -> Self {
        self.gate = Some(gate);
        self
    }

    /// The number of calls so far, readable after the session has moved into a host.
    pub fn calls(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.calls)
    }
}

impl Session for FakeTts {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            let _ = gate.recv();
        }
        if let Some(latency) = self.latency.get(call).or(self.latency.last()) {
            std::thread::sleep(*latency);
        }
        if self.fail.contains(&call) {
            return Err(RunError::Failed(format!("scripted failure on call {call}")));
        }
        let (TensorData::U8(text), TensorData::U8(voice), TensorData::F32(speed)) =
            (inputs[0].data, inputs[1].data, inputs[2].data)
        else {
            return Err(RunError::Failed("tts@1 takes UTF-8 text and voice and an f32 speed".into()));
        };
        let text = std::str::from_utf8(text).map_err(|_| RunError::Failed("the text isn't UTF-8".into()))?;
        let voice = std::str::from_utf8(voice).map_err(|_| RunError::Failed("the voice isn't UTF-8".into()))?;
        if !voice.is_empty() && !self.voices.is_empty() && !self.voices.iter().any(|v| v == voice) {
            return Err(RunError::Failed(format!("no voice {voice:?}")));
        }
        let pcm = fake_speech(text, speed.first().copied().unwrap_or(1.0));
        let problem = |problem| RunError::Mismatch { port: Port::Output, index: 0, problem };
        if let TensorDataMut::F32(out) = outputs[0].resize(&[pcm.len()]).map_err(problem)? {
            out.copy_from_slice(&pcm);
        }
        Ok(())
    }

    fn reset(&mut self) {}

    fn info(&self) -> SessionInfo {
        SessionInfo { model: fake_model(), provider: Provider::Cpu, sandboxed: false, fallback_for: None }
    }
}

/// A `voice_to_face@1` stream session: for each 320-sample block, `jawOpen = min(1, 2.5 × RMS)` of the input interval
/// the block depicts, every other channel 0. With a `latency` of `L` samples the output is that envelope delayed by
/// `L` (block `k` depicts input `[kB − L, (k+1)B − L)`, silence before the start), as a model with look-ahead.
pub struct FakeVoiceToFace {
    signature: Signature,
    latency: usize,
    /// Every input sample since the last reset, preceded by `latency` zeros.
    history: Vec<f32>,
    /// The next block's first sample in `history`.
    next: usize,
}

impl FakeVoiceToFace {
    /// With no latency of its own.
    pub fn new() -> Self {
        Self::with_latency(0)
    }

    /// With `latency` samples of its own.
    pub fn with_latency(latency: u32) -> Self {
        let latency = usize::try_from(latency).unwrap_or(0);
        Self { signature: voice_to_face_signature(), latency, history: vec![0.0; latency], next: 0 }
    }

    /// Its [`StreamSpec`]: 16 kHz, 320-sample blocks, its latency.
    pub fn spec(&self) -> StreamSpec {
        StreamSpec { sample_rate: FACE_RATE, block: FACE_BLOCK, latency: u32::try_from(self.latency).unwrap_or(0) }
    }
}

impl Default for FakeVoiceToFace {
    fn default() -> Self {
        Self::new()
    }
}

/// `min(1, 2.5 × RMS)` of `samples`.
pub fn jaw_open(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let power: f32 = samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32;
    (2.5 * power.sqrt()).min(1.0)
}

impl Session for FakeVoiceToFace {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        let TensorData::F32(block) = inputs[0].data else {
            return Err(RunError::Failed("voice_to_face@1 takes f32 audio".into()));
        };
        self.history.extend_from_slice(block);
        let depicted = &self.history[self.next..self.next + block.len()];
        let jaw = jaw_open(depicted);
        self.next += block.len();
        let problem = |problem| RunError::Mismatch { port: Port::Output, index: 0, problem };
        if let TensorDataMut::F32(out) = outputs[0].resize(&[ARKIT]).map_err(problem)? {
            out.fill(0.0);
            out[JAW_OPEN] = jaw;
        }
        Ok(())
    }

    fn reset(&mut self) {
        self.history.clear();
        self.history.resize(self.latency, 0.0);
        self.next = 0;
    }

    fn info(&self) -> SessionInfo {
        SessionInfo { model: fake_model(), provider: Provider::Cpu, sandboxed: false, fallback_for: None }
    }
}
