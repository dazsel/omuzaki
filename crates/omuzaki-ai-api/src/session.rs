//! Sessions: a loaded model that runs on the caller's thread, and the execution providers it can run on.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ModelRef, RunError, Signature, TensorBuf, TensorRef};

/// Where a model's maths runs. Every shipped model runs on [`Provider::Cpu`]; the others are tried first when a
/// manifest and the person's settings allow them and the machine has them.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Provider {
    /// The CPU. Always available, always last.
    Cpu,
    /// NVIDIA CUDA.
    Cuda,
    /// `DirectML` (any DirectX 12 GPU on Windows).
    DirectMl,
    /// Apple Core ML (GPU and Neural Engine on macOS).
    CoreMl,
    /// ONNX Runtime's native WebGPU provider (Dawn); for GPUs the others don't cover.
    WebGpu,
}

/// How hard the runtime optimises a model's graph when it loads it. A higher level fuses and rewrites more operators,
/// which can run faster but changes the order of floating-point maths, so a slot opts in only after its numeric check
/// and its run-to-run determinism test pass at that level.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum GraphOptimisation {
    /// The runtime's own default, whatever it is: the setting every session had before this option existed.
    #[default]
    Default,
    /// Constant folding, redundant-node removal and the fusions that keep the graph's meaning. Results can differ
    /// from the default in the last digits.
    Basic,
    /// [`Basic`](GraphOptimisation::Basic) plus the fusions that are specific to the CPU provider.
    Extended,
}

/// How to load a session. The registry builds it: the manifest's provider preferences, filtered by the person's
/// settings, and the slot's thread count. Build one with [`SessionOptions::new`] (the contract grows, so it is
/// `#[non_exhaustive]`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SessionOptions {
    /// Providers to try, best first. A runtime skips any it doesn't have and always ends with [`Provider::Cpu`].
    pub providers: Vec<Provider>,
    /// Threads for the model's own maths (ONNX Runtime's intra-op pool). Never spin-waiting: `cpu.total` counts every
    /// cycle (spec §10).
    pub threads: u16,
    /// How hard the runtime optimises the graph. Absent in stored options means [`GraphOptimisation::Default`].
    #[serde(default)]
    pub graph_optimisation: GraphOptimisation,
}

impl SessionOptions {
    /// Options for `providers` and `threads`, at the runtime's default graph optimisation.
    #[must_use]
    pub fn new(providers: Vec<Provider>, threads: u16) -> Self {
        Self { providers, threads, graph_optimisation: GraphOptimisation::Default }
    }

    /// The same options at `level`.
    #[must_use]
    pub fn with_graph_optimisation(mut self, level: GraphOptimisation) -> Self {
        self.graph_optimisation = level;
        self
    }
}

impl Default for SessionOptions {
    /// CPU only, one thread, the runtime's default graph optimisation: the cheapest setting, right for small per-frame
    /// models.
    fn default() -> Self {
        Self::new(vec![Provider::Cpu], 1)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_is_cpu_one_thread_and_the_runtimes_own_optimisation() {
        let options = SessionOptions::default();
        assert_eq!(options.providers, [Provider::Cpu]);
        assert_eq!(options.threads, 1);
        assert_eq!(options.graph_optimisation, GraphOptimisation::Default);
        assert_eq!(options, SessionOptions::new(vec![Provider::Cpu], 1));
    }

    #[test]
    fn a_level_is_set_with_the_builder_and_nothing_else_changes() {
        let options = SessionOptions::new(vec![Provider::Cuda, Provider::Cpu], 3)
            .with_graph_optimisation(GraphOptimisation::Basic);
        assert_eq!(options.graph_optimisation, GraphOptimisation::Basic);
        assert_eq!((options.providers, options.threads), (vec![Provider::Cuda, Provider::Cpu], 3));
    }

    #[test]
    fn stored_options_without_a_level_read_as_the_default() {
        let old: SessionOptions = serde_json::from_str(r#"{"providers":["cpu"],"threads":2}"#).unwrap();
        assert_eq!(old, SessionOptions::new(vec![Provider::Cpu], 2));
        let json =
            serde_json::to_string(&SessionOptions::default().with_graph_optimisation(GraphOptimisation::Extended))
                .unwrap();
        assert!(json.contains(r#""graph_optimisation":"extended""#), "{json}");
    }
}

/// What is running behind a [`Session`]: shown on the health meter and recorded with what the model makes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    /// The model that answers [`Session::run`] now.
    pub model: ModelRef,
    /// The provider it actually got.
    pub provider: Provider,
    /// True when it runs in a sandboxed worker process (user models).
    pub sandboxed: bool,
    /// The model it stands in for, after a fallback (the selected one crashed, stalled or went missing).
    pub fallback_for: Option<ModelRef>,
}

/// A loaded model that turns input tensors into output tensors. Runtimes create raw sessions; the registry wraps them
/// in a *slot session* that follows the slot (a new selection is swapped in between runs; a crash falls back to the
/// shipped model), so callers keep one session for as long as they need the slot.
///
/// **Threads.** `Send`, not `Sync`: one thread owns a session and calls `run` on it. That thread is the caller's (a
/// tracker thread, a speech worker), never the audio callback or the render thread, because a run takes
/// milliseconds and the runtime may allocate inside it. The runtime's own maths threads come from
/// [`SessionOptions::threads`].
pub trait Session: Send {
    /// The slot's signature: what [`run`](Session::run) takes and fills, in this order.
    fn signature(&self) -> &Signature;

    /// Runs the model once. `inputs` follow `signature().inputs`; `outputs` hold one buffer per output with the
    /// signature's element type (see [`TensorBuf::for_outputs`]), which the session resizes and fills.
    ///
    /// **Hot path:** checks the call with [`Signature::check_inputs`] and [`Signature::check_outputs`], then runs;
    /// once the output buffers have grown to size, the session itself allocates nothing.
    ///
    /// # Errors
    /// [`RunError`]; [`RunError::Unavailable`] means "no result this time, call again".
    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError>;

    /// Forgets whatever the model carries from one run to the next (the audio context of a streaming voice model),
    /// as at the start of a new stream. Models whose state travels in tensors, or that have none, do nothing.
    fn reset(&mut self);

    /// What is running now. Cold: for the health meter and records, not per frame.
    fn info(&self) -> SessionInfo;
}
