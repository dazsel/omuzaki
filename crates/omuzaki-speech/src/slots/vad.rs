//! `vad@1`: the probability that a 32 ms frame of 16 kHz audio is speech, with the streaming state carried in tensors.
//!
//! The interface is Silero VAD's graph, so its ONNX file plugs in as it is: `input` is the frame (512 samples) after
//! the last 64 samples of the frame before it, `state` the recurrent state the previous run returned, `sr` the sample
//! rate. [`crate::SileroVad`] keeps the context and the state for the host.

use omuzaki_ai_api::{
    Better, Check, DType, Dim, EvalEnv, EvalError, EvalLevel, EvalReport, Metric, Session, Signature, Slot, SlotInfo,
    SlotMode, SlotRef, TensorSpec, TestSet, TestSetKind,
};

use crate::segmenter::FRAME;
use crate::slots::clips::{CLIPS, Clip};
use crate::vad::{CONTEXT, VadStream};
use crate::{SAMPLE_RATE, synth};

/// The smallest balanced frame accuracy a model must reach on the LibriVox clips. Their labels are Silero VAD 6.2.3's own
/// segments away from the edges, so it scores 0.988 and the number measures agreement with it; the floor leaves room
/// for a model that draws its edges differently.
pub const SMOKE_FLOOR: f64 = 0.90;

/// The largest share of synthetic non-speech frames (noise, hum, a whistle, clicks) a model may score as speech.
/// Silero VAD 6.2.3 scores none of them.
pub const FALSE_ALARM_CEILING: f64 = 0.05;

/// `vad@1`.
#[must_use]
pub fn vad_slot() -> SlotRef {
    super::slot_ref("vad")
}

/// `input f32[1,576]`, `state f32[2,1,128]`, `sr i64[]` → `output f32[1,1]`, `stateN f32[2,1,128]`.
#[must_use]
pub fn vad_signature() -> Signature {
    let fixed = |name: &str, shape: &[usize]| {
        TensorSpec::new(name, DType::F32, shape.iter().map(|&n| Dim::Fixed(n)).collect::<Vec<_>>())
    };
    Signature {
        inputs: vec![
            fixed("input", &[1, CONTEXT + FRAME]),
            fixed("state", &[2, 1, 128]),
            TensorSpec::new("sr", DType::I64, []),
        ],
        outputs: vec![fixed("output", &[1, 1]), fixed("stateN", &[2, 1, 128])],
    }
}

/// One stretch of labelled audio for the evaluation.
pub struct Labelled<'a> {
    /// 16 kHz mono.
    pub audio: &'a [f32],
    /// Speech, pause, or not scored, at a time in seconds.
    pub label: &'a dyn Fn(f64) -> Option<bool>,
}

/// The balanced frame accuracy of `session` over `items`: the mean of the share of speech frames scored as speech
/// (at least 0.5) and the share of other frames scored below it, so a model that always says yes, or always no,
/// gets 0.5. Each item starts from a fresh state.
///
/// # Errors
/// [`EvalError::Run`] when the model fails, [`EvalError::Output`] when its signature isn't `vad@1`'s or a set has no
/// frames of one kind.
pub fn balanced_accuracy(session: &mut dyn Session, items: &[Labelled<'_>]) -> Result<f64, EvalError> {
    let counts = count(session, items)?;
    if counts.speech == 0 || counts.quiet == 0 {
        return Err(EvalError::Output("the test set needs both speech and non-speech frames".into()));
    }
    Ok(f64::midpoint(counts.speech_hit as f64 / counts.speech as f64, counts.quiet_hit as f64 / counts.quiet as f64))
}

/// The share of frames of `audio` (none of it speech) that `session` scores as speech (at least 0.5).
///
/// # Errors
/// As [`balanced_accuracy`].
pub fn false_alarms(session: &mut dyn Session, audio: &[Vec<f32>]) -> Result<f64, EvalError> {
    let items: Vec<Labelled<'_>> = audio.iter().map(|audio| Labelled { audio, label: &never }).collect();
    let counts = count(session, &items)?;
    Ok(if counts.quiet == 0 { 0.0 } else { 1.0 - counts.quiet_hit as f64 / counts.quiet as f64 })
}

/// Frames scored, by label, and how many of each the model got right.
#[derive(Default)]
struct Counts {
    speech: u64,
    speech_hit: u64,
    quiet: u64,
    quiet_hit: u64,
}

fn count(session: &mut dyn Session, items: &[Labelled<'_>]) -> Result<Counts, EvalError> {
    if session.signature() != &vad_signature() {
        return Err(EvalError::Output("the session doesn't present vad@1's signature".into()));
    }
    let mut vad = VadStream::new();
    let mut counts = Counts::default();
    let mut frame = [0.0f32; FRAME];
    for item in items {
        vad.reset();
        for (index, chunk) in item.audio.chunks(FRAME).enumerate() {
            frame.fill(0.0);
            frame[..chunk.len()].copy_from_slice(chunk);
            let probability = vad.probability(session, &frame).map_err(|e| match e {
                crate::SpeechError::Run(run) => EvalError::Run(run),
                other => EvalError::Output(other.to_string()),
            })?;
            let middle = (index * FRAME + FRAME / 2) as f64 / f64::from(SAMPLE_RATE);
            match (item.label)(middle) {
                Some(true) => {
                    counts.speech += 1;
                    counts.speech_hit += u64::from(probability >= 0.5);
                }
                Some(false) => {
                    counts.quiet += 1;
                    counts.quiet_hit += u64::from(probability < 0.5);
                }
                None => {}
            }
        }
    }
    Ok(counts)
}

/// The synthetic sounds that are never speech, 2 s each: silence, noise at two levels, mains hum, a whistle and
/// keyboard clicks (the crate's deterministic [`synth`]).
#[must_use]
pub fn non_speech() -> Vec<Vec<f32>> {
    vec![
        synth::silence(2.0),
        synth::white_noise(2.0, 0.1, 1),
        synth::white_noise(2.0, 0.01, 2),
        synth::tone(2.0, 50.0, 0.1),
        synth::tone(2.0, 1000.0, 0.2),
        synth::clicks(2.0, 0.25, 0.3, 3),
    ]
}

#[allow(clippy::unnecessary_wraps)] // a label function: speech, pause or unscored
fn never(_: f64) -> Option<bool> {
    Some(false)
}

fn metric(name: &str, value: f64, better: Better, id: &str, kind: TestSetKind) -> Metric {
    Metric { name: name.into(), value, better, set: TestSet { id: id.into(), kind } }
}

/// `vad@1`. Smoke and full run the same items today: the LibriVox clips (real speech) and the synthetic non-speech,
/// each reported on its own and never averaged together.
#[derive(Default)]
pub struct VadSlot;

impl Slot for VadSlot {
    fn info(&self) -> SlotInfo {
        SlotInfo {
            slot: vad_slot(),
            summary: "Voice activity: how likely each 32 ms of the microphone is to be speech.".into(),
            without_model: "speech isn't detected: live captions, listening and voice commands are off".into(),
            mode: SlotMode::Call { stall_ms: 50 },
            threads: 1,
            likeness: None,
        }
    }

    fn signature(&self) -> Signature {
        vad_signature()
    }

    fn evaluate(
        &self,
        session: &mut dyn Session,
        env: &dyn EvalEnv,
        level: EvalLevel,
    ) -> Result<EvalReport, EvalError> {
        let clips: Vec<(Clip, Vec<f32>)> =
            CLIPS.iter().map(|clip| Ok((*clip, clip.load(env)?))).collect::<Result<_, EvalError>>()?;
        let labels: Vec<Box<dyn Fn(f64) -> Option<bool>>> = clips
            .iter()
            .map(|(clip, _)| Box::new(move |t| clip.label(t)) as Box<dyn Fn(f64) -> Option<bool>>)
            .collect();
        let real: Vec<Labelled<'_>> =
            clips.iter().zip(&labels).map(|((_, audio), label)| Labelled { audio, label: label.as_ref() }).collect();
        let accuracy = balanced_accuracy(session, &real)?;
        let alarms = false_alarms(session, &non_speech())?;
        let real = metric("balanced_frame_accuracy", accuracy, Better::Higher, "librivox-aesop-3-1", TestSetKind::Real);
        let synthetic = metric("false_alarm_rate", alarms, Better::Lower, "synth-non-speech-1", TestSetKind::Synthetic);
        Ok(EvalReport {
            level,
            checks: vec![
                Check { metric: real, floor: Some(SMOKE_FLOOR) },
                Check { metric: synthetic, floor: Some(FALSE_ALARM_CEILING) },
            ],
        })
    }
}

omuzaki_ai_api::register_slot!(VadSlot);
