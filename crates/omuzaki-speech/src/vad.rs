//! Voice activity on a `vad@1` session (Silero VAD's interface).
//!
//! The model is stateful: each run takes one 512-sample frame (32 ms at 16 kHz), prefixed with the last 64 samples
//! of the previous frame as context, plus a recurrent state that it returns for the next run. [`VadStream`] owns that
//! context and state, so callers just push frames in order. A run may allocate inside the runtime, so it belongs on
//! the host's VAD thread, never in an audio callback.

use omuzaki_ai_api::{RunError, Session, TensorBuf, TensorRef};

use crate::segmenter::FRAME;
use crate::slots::vad_signature;
use crate::{SAMPLE_RATE, SpeechError};

/// Samples of the previous frame prepended to each frame (Silero's fixed context size at 16 kHz).
pub const CONTEXT: usize = 64;
/// The recurrent state is `[2, batch = 1, 128]`.
const STATE_LEN: usize = 2 * 128;
const INPUT_SHAPE: [usize; 2] = [1, CONTEXT + FRAME];
const STATE_SHAPE: [usize; 3] = [2, 1, 128];
const RATE: [i64; 1] = [SAMPLE_RATE as i64];

/// Something that scores 32 ms frames for speech, in order. [`SileroVad`] is the real one; the host takes the trait so
/// its threading, queues and back-pressure can be tested without a model.
pub trait VoiceActivity: Send {
    /// Scores one frame and returns the probability of speech, 0.0 to 1.0.
    ///
    /// # Errors
    /// Whatever the implementation's model reports; [`SpeechError::Run`] with [`RunError::Unavailable`] means "no
    /// answer this time" (the model is being switched), which the host counts and treats as silence.
    fn probability(&mut self, frame: &[f32; FRAME]) -> Result<f32, SpeechError>;

    /// Forgets all streaming state, as at the start of a new stream.
    fn reset(&mut self);
}

/// The streaming context and state of one audio stream through a `vad@1` session. Allocation-free after the first
/// run (the output buffers keep their size).
pub struct VadStream {
    state: Vec<f32>,
    /// Context (64 samples) followed by the frame being scored: the model input, reused across calls.
    input: Vec<f32>,
    outputs: Vec<TensorBuf>,
}

impl Default for VadStream {
    fn default() -> Self {
        Self::new()
    }
}

impl VadStream {
    /// A fresh stream.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: vec![0.0; STATE_LEN],
            input: vec![0.0; CONTEXT + FRAME],
            outputs: TensorBuf::for_outputs(&vad_signature()),
        }
    }

    /// Scores the next frame of the stream on `session`.
    ///
    /// # Errors
    /// [`SpeechError::Run`] when the session fails, [`SpeechError::ModelOutput`] on an output of the wrong size.
    pub fn probability(&mut self, session: &mut dyn Session, frame: &[f32; FRAME]) -> Result<f32, SpeechError> {
        // Slide the context: the tail of last call's frame becomes the head of this call's input.
        self.input.copy_within(FRAME.., 0);
        self.input[CONTEXT..].copy_from_slice(frame);
        let inputs = [
            TensorRef::f32(&self.input, &INPUT_SHAPE),
            TensorRef::f32(&self.state, &STATE_SHAPE),
            TensorRef::i64(&RATE, &[]),
        ];
        session.run(&inputs, &mut self.outputs)?;
        let next_state = self.outputs[1].as_f32().unwrap_or_default();
        if next_state.len() != STATE_LEN {
            return Err(SpeechError::ModelOutput(format!(
                "state has {} values, expected {STATE_LEN}",
                next_state.len()
            )));
        }
        self.state.copy_from_slice(next_state);
        self.outputs[0]
            .as_f32()
            .and_then(|p| p.first().copied())
            .ok_or_else(|| SpeechError::ModelOutput("empty probability tensor".into()))
    }

    /// Forgets the context and the state.
    pub fn reset(&mut self) {
        self.state.fill(0.0);
        self.input.fill(0.0);
    }
}

/// A `vad@1` session (Silero VAD by default) with its streaming state.
pub struct SileroVad {
    session: Box<dyn Session>,
    stream: VadStream,
}

impl SileroVad {
    /// Wraps a session the registry opened for `vad@1`.
    ///
    /// # Errors
    /// [`SpeechError::Signature`] when the session presents another slot's signature.
    pub fn new(session: Box<dyn Session>) -> Result<Self, SpeechError> {
        if session.signature() != &vad_signature() {
            return Err(SpeechError::Signature("vad@1"));
        }
        Ok(Self { session, stream: VadStream::new() })
    }

    /// Scores a whole recording from a fresh state, one probability per [`FRAME`]; a final partial frame is padded
    /// with silence.
    ///
    /// # Errors
    /// As [`VoiceActivity::probability`].
    pub fn probabilities(&mut self, audio: &[f32]) -> Result<Vec<f32>, SpeechError> {
        self.reset();
        let mut frame = [0.0f32; FRAME];
        audio
            .chunks(FRAME)
            .map(|chunk| {
                frame.fill(0.0);
                frame[..chunk.len()].copy_from_slice(chunk);
                self.probability(&frame)
            })
            .collect()
    }
}

impl VoiceActivity for SileroVad {
    fn probability(&mut self, frame: &[f32; FRAME]) -> Result<f32, SpeechError> {
        self.stream.probability(self.session.as_mut(), frame)
    }

    fn reset(&mut self) {
        self.stream.reset();
        self.session.reset();
    }
}

/// True when `error` is a session's "no answer this time" (a model switch or fallback in progress).
#[must_use]
pub fn is_unavailable(error: &SpeechError) -> bool {
    matches!(error, SpeechError::Run(RunError::Unavailable))
}
