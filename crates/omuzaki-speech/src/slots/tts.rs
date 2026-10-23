//! `tts@1`: text to speech (ML-04, `ai-performer-voice-face.md` §3.1). Text and a voice in, mono speech at 24 kHz out.
//!
//! `voice` is a voice the model lists in its manifest's `[[voices]]` (`af_heart`), or empty for its first. `speed` is a
//! multiplier (1 is the model's own pace) and `expressiveness` runs from 0 (flat) to 1 (lively); a model that doesn't
//! honour one (its manifest's `[tts] honours`) is fed it and ignores it. Text is UTF-8 bytes.
//!
//! The evaluation is a round trip: the model reads fixed sentences, the shipped speech recogniser (`asr@1`) transcribes
//! what it said, and the word error rate is the score. A model that mumbles, skips words or reads them wrong fails.

use omuzaki_ai_api::{
    Better, Check, DType, Dim, EvalEnv, EvalError, EvalLevel, EvalReport, Metric, RunError, Session, Signature, Slot,
    SlotInfo, SlotMode, SlotRef, TensorBuf, TensorRef, TensorSpec, TestSet, TestSetKind,
};

use crate::slots::asr::{Reading, word_error_rate};
use crate::slots::asr_slot;

/// The rate of `pcm` out.
pub const TTS_RATE: u32 = 24_000;

/// The largest word error rate a model may have on the smoke sentences, as heard by the shipped recogniser. Kokoro-82M
/// with `af_heart` scores 0.000 heard by Whisper `tiny.en` (see the install test); the ceiling allows a recogniser slip
/// or two and refuses a model that gets one word in eight wrong.
pub const SMOKE_CEILING: f64 = 0.12;

/// What the smoke test has a model read: the project's own sentences, in the register a stream has (greetings, thanks,
/// questions, an exclamation), with no number a recogniser could write as digits.
pub const SENTENCES: [&str; 6] = [
    "Hello everyone, and welcome back to the stream.",
    "Today we are going to build a small wooden boat together.",
    "Thank you so much for the follow, it really means a lot.",
    "The weather outside is cold, so I made a cup of hot tea.",
    "Should we try the harder level first, or warm up on the easy one?",
    "That was close! I almost fell off the bridge.",
];

/// `tts@1`.
#[must_use]
pub fn tts_slot() -> SlotRef {
    super::slot_ref("tts")
}

/// `text u8[any]`, `voice u8[any]`, `speed f32[]`, `expressiveness f32[]` → `pcm f32[any]` (24 kHz mono).
#[must_use]
pub fn tts_signature() -> Signature {
    let any = |name: &str, dtype| TensorSpec::new(name, dtype, [Dim::ANY]);
    let scalar = |name: &str| TensorSpec::new(name, DType::F32, Vec::new());
    Signature {
        inputs: vec![any("text", DType::U8), any("voice", DType::U8), scalar("speed"), scalar("expressiveness")],
        outputs: vec![any("pcm", DType::F32)],
    }
}

/// One thing to say.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Utterance<'a> {
    /// What to say.
    pub text: &'a str,
    /// The voice, as the model names it; empty for its first.
    pub voice: &'a str,
    /// The pace, 1 for the model's own.
    pub speed: f32,
    /// From 0 (flat) to 1 (lively).
    pub expressiveness: f32,
}

impl<'a> Utterance<'a> {
    /// `text` in the model's first voice, at its own pace, moderately expressive.
    #[must_use]
    pub fn plain(text: &'a str) -> Self {
        Self { text, voice: "", speed: 1.0, expressiveness: 0.5 }
    }
}

/// Calls a `tts@1` session, keeping its output buffer between calls.
pub struct TtsCall {
    outputs: Vec<TensorBuf>,
}

impl Default for TtsCall {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsCall {
    /// A fresh buffer.
    #[must_use]
    pub fn new() -> Self {
        Self { outputs: TensorBuf::for_outputs(&tts_signature()) }
    }

    /// Speaks `utterance` on `session`: 24 kHz mono samples.
    ///
    /// # Errors
    /// [`RunError`] when the session fails or hands back something that isn't audio.
    pub fn speak(&mut self, session: &mut dyn Session, utterance: &Utterance<'_>) -> Result<&[f32], RunError> {
        let (text, voice) = (utterance.text.as_bytes(), utterance.voice.as_bytes());
        let (text_shape, voice_shape) = ([text.len()], [voice.len()]);
        let (speed, expressiveness) = ([utterance.speed], [utterance.expressiveness]);
        let inputs = [
            TensorRef::u8(text, &text_shape),
            TensorRef::u8(voice, &voice_shape),
            TensorRef::f32(&speed, &[]),
            TensorRef::f32(&expressiveness, &[]),
        ];
        session.run(&inputs, &mut self.outputs)?;
        let pcm = self.outputs[0].as_f32().ok_or_else(|| RunError::Failed("tts@1 gave no f32 audio".into()))?;
        if pcm.iter().any(|s| !s.is_finite()) {
            return Err(RunError::Failed("the speech has samples that aren't numbers".into()));
        }
        Ok(pcm)
    }
}

/// `input` at `from` Hz resampled to `to` Hz with a windowed-sinc low-pass (32 taps a side, Blackman window), so a
/// 24 kHz voice reaches a 16 kHz recogniser without aliasing.
#[must_use]
pub fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    const TAPS: isize = 32;
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let step = f64::from(from) / f64::from(to);
    // The cutoff, as a fraction of the input's Nyquist: the lower of the two, a little under it.
    let cutoff = (f64::from(to) / f64::from(from)).min(1.0) * 0.95;
    let len = (input.len() as f64 / step).floor() as usize;
    let half = TAPS as f64;
    (0..len)
        .map(|n| {
            let t = n as f64 * step;
            let centre = t.floor() as isize;
            let (mut sum, mut weight) = (0.0f64, 0.0f64);
            for k in centre - TAPS + 1..=centre + TAPS {
                let Some(&x) = usize::try_from(k).ok().and_then(|k| input.get(k)) else { continue };
                let d = t - k as f64;
                let sinc = if d == 0.0 {
                    cutoff
                } else {
                    (std::f64::consts::PI * cutoff * d).sin() / (std::f64::consts::PI * d)
                };
                let phase = std::f64::consts::PI * d / half;
                let window = 0.42 + 0.5 * phase.cos() + 0.08 * (2.0 * phase).cos();
                let h = sinc * window;
                sum += f64::from(x) * h;
                weight += h;
            }
            if weight.abs() > 1e-9 { (sum / weight) as f32 } else { 0.0 }
        })
        .collect()
}

/// The word error rate of `tts` reading `sentences`, heard by `asr` (an `asr@1` session).
///
/// # Errors
/// [`EvalError::Run`] when either model fails, [`EvalError::Output`] when the speech is empty.
pub fn round_trip_error(tts: &mut dyn Session, asr: &mut dyn Session, sentences: &[&str]) -> Result<f64, EvalError> {
    if tts.signature() != &tts_signature() {
        return Err(EvalError::Output("the session doesn't present tts@1's signature".into()));
    }
    let mut call = TtsCall::new();
    let mut heard = Vec::with_capacity(sentences.len());
    for sentence in sentences {
        let pcm = call.speak(tts, &Utterance::plain(sentence))?;
        if pcm.len() < TTS_RATE as usize / 4 {
            return Err(EvalError::Output(format!("{sentence:?} came out {} samples long", pcm.len())));
        }
        heard.push(resample(pcm, TTS_RATE, crate::SAMPLE_RATE));
    }
    let items: Vec<Reading<'_>> = sentences
        .iter()
        .zip(&heard)
        .map(|(text, audio)| Reading { utterances: vec![audio.as_slice()], text })
        .collect();
    word_error_rate(asr, &items)
}

/// `tts@1`. Smoke and full read the same sentences today.
#[derive(Default)]
pub struct TtsSlot;

impl Slot for TtsSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            slot: tts_slot(),
            summary: "Text to speech: a character's voice, for AI replies and chat read aloud.".into(),
            without_model: "characters don't speak: AI replies and chat read aloud stay text".into(),
            mode: SlotMode::Call { stall_ms: 30_000 },
            threads: 4,
            likeness: None,
        }
    }

    fn signature(&self) -> Signature {
        tts_signature()
    }

    fn evaluate(
        &self,
        session: &mut dyn Session,
        env: &dyn EvalEnv,
        level: EvalLevel,
    ) -> Result<EvalReport, EvalError> {
        let mut asr = env.shipped(&asr_slot())?;
        let wer = round_trip_error(session, asr.as_mut(), &SENTENCES)?;
        let set = TestSet { id: "omuzaki-tts-smoke-1".into(), kind: TestSetKind::Synthetic };
        let metric = Metric { name: "round_trip_wer".into(), value: wer, better: Better::Lower, set };
        Ok(EvalReport { level, checks: vec![Check { metric, floor: Some(SMOKE_CEILING) }] })
    }
}

omuzaki_ai_api::register_slot!(TtsSlot);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_signature_is_text_voice_and_controls_to_24k_audio() {
        let s = tts_signature();
        let names: Vec<&str> = s.inputs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, ["text", "voice", "speed", "expressiveness"]);
        assert!(s.inputs[2].shape.is_empty() && s.inputs[3].shape.is_empty(), "the controls are scalars");
        assert_eq!(s.outputs.len(), 1);
        assert_eq!(s.outputs[0].name, "pcm");
        assert_eq!(s.outputs[0].dtype, DType::F32);
        assert_eq!(tts_slot().to_string(), "tts@1");
    }

    #[test]
    fn resampling_keeps_a_tone_and_drops_what_would_alias() {
        let tone = |hz: f64, rate: u32, n: usize| -> Vec<f32> {
            (0..n).map(|i| (std::f64::consts::TAU * hz * i as f64 / f64::from(rate)).sin() as f32 * 0.5).collect()
        };
        let rms = |x: &[f32]| (x.iter().map(|s| f64::from(*s).powi(2)).sum::<f64>() / x.len() as f64).sqrt();
        let kept = resample(&tone(440.0, 24_000, 24_000), 24_000, 16_000);
        assert_eq!(kept.len(), 16_000);
        let expected = tone(440.0, 16_000, 16_000);
        let error =
            kept[100..15_900].iter().zip(&expected[100..15_900]).map(|(a, b)| (a - b).abs()).fold(0.0, f32::max);
        assert!(error < 0.01, "a 440 Hz tone comes through: worst error {error}");
        let above = resample(&tone(10_000.0, 24_000, 24_000), 24_000, 16_000);
        assert!(rms(&above[100..15_900]) < 0.01, "10 kHz is above 16 kHz's Nyquist: {}", rms(&above));
    }
}
