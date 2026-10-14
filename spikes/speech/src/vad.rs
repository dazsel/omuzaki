//! Silero VAD (MIT) run through `ort` with an ONNX Runtime library we load ourselves.
//!
//! The model is stateful: each call takes one 512-sample frame (32 ms at 16 kHz), prefixed with the last 64 samples
//! of the previous frame as context, plus an LSTM state that it returns for the next call. The wrapper owns that
//! context and state, so callers just push frames in order.

use std::path::Path;

use ort::session::Session;
use ort::value::TensorRef;

use crate::segmenter::FRAME;
use crate::{SAMPLE_RATE, SpeechError};

/// Samples of the previous frame prepended to each frame (Silero's fixed context size at 16 kHz).
const CONTEXT: usize = 64;
/// The LSTM state is `[2, batch = 1, 128]`.
const STATE_LEN: usize = 2 * 128;

/// Loads the ONNX Runtime shared library at `dylib`. Call once, before the first [`SileroVad::new`].
///
/// `ort` with `load-dynamic` would otherwise look the library up lazily and panic if it is missing; loading it here
/// turns that into an error. Returns `false` when an environment already exists (the first call wins).
///
/// # Errors
/// [`SpeechError::OrtLoad`] when the library cannot be loaded.
pub fn load_runtime(dylib: &Path) -> Result<bool, SpeechError> {
    Ok(ort::init_from(dylib)?.with_name("omuzaki-speech").commit())
}

/// A Silero VAD session with its streaming state.
pub struct SileroVad {
    session: Session,
    state: Vec<f32>,
    /// Context (64 samples) followed by the frame being scored: the model input, reused across calls.
    input: Vec<f32>,
}

impl SileroVad {
    /// Loads the model with a single-threaded session: one 32 ms frame is far too small to split across threads, and
    /// a thread pool would spin-wait between frames.
    ///
    /// # Errors
    /// [`SpeechError::Ort`] when the model cannot be loaded.
    pub fn new(model: &Path) -> Result<Self, SpeechError> {
        let session = Session::builder()?
            .with_intra_threads(1)?
            .with_inter_threads(1)?
            .with_intra_op_spinning(false)?
            .commit_from_file(model)?;
        Ok(Self { session, state: vec![0.0; STATE_LEN], input: vec![0.0; CONTEXT + FRAME] })
    }

    /// Forgets all streaming state, as at the start of a new stream.
    pub fn reset(&mut self) {
        self.state.fill(0.0);
        self.input.fill(0.0);
    }

    /// Scores one frame and returns the probability of speech, 0.0 to 1.0.
    ///
    /// # Errors
    /// [`SpeechError::Ort`] when the model fails, [`SpeechError::ModelOutput`] on an unexpected output shape.
    pub fn probability(&mut self, frame: &[f32; FRAME]) -> Result<f32, SpeechError> {
        // Slide the context: the tail of last call's frame becomes the head of this call's input.
        self.input.copy_within(FRAME.., 0);
        self.input[CONTEXT..].copy_from_slice(frame);

        let input = TensorRef::from_array_view(([1usize, CONTEXT + FRAME], self.input.as_slice()))?;
        let state = TensorRef::from_array_view(([2usize, 1, 128], self.state.as_slice()))?;
        let rate = [i64::from(SAMPLE_RATE)];
        let sr = TensorRef::from_array_view(([0usize; 0], rate.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input, state, sr])?;

        let (_, probability) = outputs["output"].try_extract_tensor::<f32>()?;
        let (_, next_state) = outputs["stateN"].try_extract_tensor::<f32>()?;
        if next_state.len() != STATE_LEN {
            return Err(SpeechError::ModelOutput(format!(
                "state has {} values, expected {STATE_LEN}",
                next_state.len()
            )));
        }
        self.state.copy_from_slice(next_state);
        probability.first().copied().ok_or_else(|| SpeechError::ModelOutput("empty probability tensor".into()))
    }

    /// Scores a whole recording from a fresh state, one probability per [`FRAME`]; a final partial frame is padded
    /// with silence.
    ///
    /// # Errors
    /// As [`probability`](Self::probability).
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
