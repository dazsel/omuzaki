//! Fake sessions: an affine function of the first input, a delay line (a streaming model with look-ahead), or outputs
//! replayed from a script; any of them can be told to crash after a number of runs.

use omuzaki_ai_api::{
    ModelId, ModelRef, Provider, RunError, Session, SessionInfo, Sha256, Signature, TensorBuf, TensorData,
    TensorDataMut, TensorRef, Version,
};

/// The [`ModelRef`] a fake session reports unless told otherwise.
pub fn fake_model() -> ModelRef {
    let id = ModelId::new("fake").unwrap_or_else(|_| unreachable!("a literal slug"));
    ModelRef { id, version: Version::new(0, 0, 0), digest: Sha256([0; 32]) }
}

enum Behaviour {
    /// Every `f32` output is `scale * x + bias` over input 0, in input 0's shape; other outputs are left empty.
    Affine { scale: f32, bias: f32 },
    /// Run `n` copies `script[n % len]` into the outputs.
    Script { script: Vec<Vec<TensorBuf>>, next: usize },
    /// Every `f32` output is input 0 delayed by `line.len()` samples across runs, as a streaming model whose own
    /// latency is that long; the line starts silent.
    Delay { line: Vec<f32>, pos: usize },
}

/// A deterministic [`Session`]: checks every call against its signature exactly as a real one must, and allocates
/// nothing once its output buffers have grown.
pub struct FakeSession {
    signature: Signature,
    behaviour: Behaviour,
    info: SessionInfo,
    runs: u64,
    crash_after: Option<u64>,
    stall: Option<std::time::Duration>,
}

impl FakeSession {
    /// `y = scale * x + bias`, elementwise, from input 0 to every `f32` output.
    pub fn affine(signature: Signature, scale: f32, bias: f32) -> Self {
        Self::with(signature, Behaviour::Affine { scale, bias })
    }

    /// Input 0 delayed by `samples` samples, carried across runs and cleared by [`Session::reset`]: a streaming model
    /// whose [`StreamSpec::latency`](omuzaki_ai_api::StreamSpec::latency) is `samples`. Zero is the identity.
    pub fn delay(signature: Signature, samples: usize) -> Self {
        Self::with(signature, Behaviour::Delay { line: vec![0.0; samples], pos: 0 })
    }

    /// Replays `script` (one set of outputs per run, in order, cycling). An empty script fills nothing.
    pub fn scripted(signature: Signature, script: Vec<Vec<TensorBuf>>) -> Self {
        Self::with(signature, Behaviour::Script { script, next: 0 })
    }

    fn with(signature: Signature, behaviour: Behaviour) -> Self {
        let info = SessionInfo { model: fake_model(), provider: Provider::Cpu, sandboxed: false, fallback_for: None };
        Self { signature, behaviour, info, runs: 0, crash_after: None, stall: None }
    }

    /// Every run after the first `runs` returns [`RunError::Crashed`], like a sandboxed model whose worker died.
    #[must_use]
    pub fn crashing_after(mut self, runs: u64) -> Self {
        self.crash_after = Some(runs);
        self
    }

    /// Every run sleeps `stall` before it answers, like a model that hangs (a sandboxed one is then killed).
    #[must_use]
    pub fn stalling(mut self, stall: std::time::Duration) -> Self {
        self.stall = Some(stall);
        self
    }

    /// Reports `model` in [`Session::info`].
    #[must_use]
    pub fn reporting(mut self, model: ModelRef) -> Self {
        self.info.model = model;
        self
    }

    /// How many runs succeeded.
    pub fn runs(&self) -> u64 {
        self.runs
    }
}

/// Writes `f(sample)` for every sample of input 0 into `out`, shaped like input 0. Non-`f32` inputs write nothing.
fn map_f32(x: &TensorRef<'_>, out: &mut TensorBuf, mut f: impl FnMut(f32) -> f32) -> Result<(), RunError> {
    let TensorData::F32(src) = x.data else { return Ok(()) };
    let problem = |problem| RunError::Mismatch { port: omuzaki_ai_api::Port::Output, index: 0, problem };
    if let TensorDataMut::F32(dst) = out.resize(x.shape).map_err(problem)? {
        for (d, s) in dst.iter_mut().zip(src) {
            *d = f(*s);
        }
    }
    Ok(())
}

/// Pushes `sample` into the delay line and returns the one it displaces.
fn delayed(line: &mut [f32], pos: &mut usize, sample: f32) -> f32 {
    if line.is_empty() {
        return sample;
    }
    let out = std::mem::replace(&mut line[*pos], sample);
    *pos = (*pos + 1) % line.len();
    out
}

impl Session for FakeSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        if self.crash_after.is_some_and(|n| self.runs >= n) {
            return Err(RunError::Crashed);
        }
        if let Some(stall) = self.stall {
            std::thread::sleep(stall);
        }
        match &mut self.behaviour {
            Behaviour::Affine { scale, bias } => {
                if let Some(x) = inputs.first() {
                    for out in outputs.iter_mut().filter(|o| o.dtype() == x.dtype()) {
                        map_f32(x, out, |s| *scale * s + *bias)?;
                    }
                }
            }
            Behaviour::Delay { line, pos } => {
                // One shared line: the first f32 output gets the delayed signal; a second one would delay it again.
                if let Some(x) = inputs.first()
                    && let Some(out) = outputs.iter_mut().find(|o| o.dtype() == x.dtype())
                {
                    map_f32(x, out, |s| delayed(line, pos, s))?;
                }
            }
            Behaviour::Script { script, next } => {
                if let Some(step) = script.get(*next % script.len().max(1)) {
                    for (index, (out, src)) in outputs.iter_mut().zip(step).enumerate() {
                        out.set(&src.as_ref()).map_err(|problem| RunError::Mismatch {
                            port: omuzaki_ai_api::Port::Output,
                            index,
                            problem,
                        })?;
                    }
                    *next += 1;
                }
            }
        }
        self.runs += 1;
        Ok(())
    }

    fn reset(&mut self) {
        match &mut self.behaviour {
            Behaviour::Script { next, .. } => *next = 0,
            Behaviour::Delay { line, pos } => {
                line.fill(0.0);
                *pos = 0;
            }
            Behaviour::Affine { .. } => {}
        }
    }

    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}
