//! ONNX Runtime plumbing: load the shared library, build sessions for a chosen backend, run one f32 input.

use std::path::Path;

use ort::ep::{self, cuda::ConvAlgorithmSearch};
use ort::session::builder::GraphOptimizationLevel;
use ort::session::{Session, SessionOutputs};
use ort::value::TensorRef;

use crate::error::{Result, TrackError};

/// Where the models run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    /// ONNX Runtime's CPU provider.
    Cpu,
    /// The CUDA provider on one GPU. Session creation fails if CUDA cannot be used, so a silent CPU fallback never
    /// passes for a GPU measurement.
    Cuda {
        /// CUDA device index.
        device: i32,
    },
}

/// Session settings shared by the three models.
#[derive(Debug, Clone)]
pub struct RuntimeOptions {
    /// Execution provider.
    pub backend: Backend,
    /// CPU threads inside one operator (CPU provider; also the host side of the CUDA provider).
    pub intra_threads: usize,
    /// Whether worker threads busy-wait briefly between operators instead of sleeping.
    pub spin: bool,
}

impl Default for RuntimeOptions {
    /// One thread with spinning: the best latency per unit of CPU in the benchmark (see Findings in `README.md`). Without spinning
    /// the face mesh model gets slower with more threads; with it, extra threads buy speed at several times the CPU.
    fn default() -> Self {
        Self { backend: Backend::Cpu, intra_threads: 1, spin: true }
    }
}

/// Loads the ONNX Runtime shared library from `lib`. Call once, before the first [`Model::load`].
///
/// # Errors
/// Returns [`TrackError::OrtLoad`] when the library cannot be loaded.
pub fn init_runtime(lib: &Path) -> Result<()> {
    // `commit` returns false when an environment already exists, which only means a second call; nothing to do.
    let _already_initialised = !ort::init_from(lib)?.commit();
    Ok(())
}

/// One model file loaded into an ONNX Runtime session.
pub struct Model {
    session: Session,
}

impl Model {
    /// Loads and optimises `path`.
    ///
    /// # Errors
    /// Returns [`TrackError::Ort`] when the file is not a valid model or the backend cannot be set up.
    pub fn load(path: &Path, options: &RuntimeOptions) -> Result<Self> {
        Ok(Self { session: build_session(path, options)? })
    }

    /// Runs the model on one f32 input tensor of the given shape.
    ///
    /// # Errors
    /// Returns [`TrackError::Ort`] when the shape does not match the model or inference fails.
    pub fn run(&mut self, shape: &[usize], data: &[f32]) -> Result<SessionOutputs<'_>> {
        let input = TensorRef::from_array_view((shape, data))?;
        Ok(self.session.run(ort::inputs![input])?)
    }
}

/// Builds a session; a separate function so `?` converts the builder's recoverable errors into plain `ort::Error`s.
fn build_session(path: &Path, options: &RuntimeOptions) -> ort::Result<Session> {
    let mut builder = Session::builder()?
        .with_optimization_level(GraphOptimizationLevel::All)?
        .with_intra_threads(options.intra_threads)?
        .with_intra_op_spinning(options.spin)?
        .with_inter_op_spinning(options.spin)?;
    if let Backend::Cuda { device } = options.backend {
        // Heuristic cuDNN algorithm choice: the default exhaustive search makes the first frame take seconds.
        let cuda =
            ep::CUDA::default().with_device_id(device).with_conv_algorithm_search(ConvAlgorithmSearch::Heuristic);
        builder = builder.with_execution_providers([cuda.build().error_on_failure()])?;
    }
    builder.commit_from_file(path)
}

/// The f32 data of the output called `name`.
///
/// # Errors
/// Returns [`TrackError::Model`] when the model has no such output, and [`TrackError::Ort`] when it is not f32.
pub fn output_f32<'o>(outputs: &'o SessionOutputs<'_>, name: &str) -> Result<&'o [f32]> {
    let value = outputs.get(name).ok_or_else(|| TrackError::Model(format!("the model has no output `{name}`")))?;
    Ok(value.try_extract_tensor::<f32>()?.1)
}
