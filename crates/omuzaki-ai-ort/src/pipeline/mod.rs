//! Multi-graph models (`[[stages]]`) as one [`Session`]: the graphs run in manifest order, each stage reading slot
//! inputs or earlier stages' outputs, and the slot sees one model (`docs/design/ai-api.md` §3, ADR 0017).
//!
//! - **Wiring** ([`plan`]) is checked from the graphs' signatures, the same way for `probe` and `load`.
//! - **One ONNX Runtime session per stage**, all built with the same [`SessionOptions`]; each is an
//!   [`OrtModelSession`], so a stage keeps the no-allocation-per-run behaviour of a single-graph model.
//! - **Intermediate tensors** live in one [`TensorBuf`] per stage output, made at load and reused by every run, so a
//!   run allocates nothing here once the buffers have grown to the shapes it sees. The wiring of each stage's inputs is
//!   built on the stack ([`MAX_STAGE_INPUTS`] bounds it).

mod plan;

use std::collections::BTreeMap;

use omuzaki_ai_api::{
    LoadError, ModelSource, Port, Probe, Provider, RunError, Session, SessionInfo, SessionOptions, Signature, Stage,
    TensorBuf, TensorData, TensorProblem, TensorRef,
};

use crate::graph::{self, Skipped};
use crate::library;
use crate::scan;
use crate::session::OrtModelSession;
use plan::Source;

/// The most inputs one stage's graph may have: the run builds its input list in an array this long, because a `Vec`
/// would allocate on every run.
pub const MAX_STAGE_INPUTS: usize = 8;

/// An input list entry that is overwritten before use.
const UNUSED: TensorRef<'static> = TensorRef { data: TensorData::F32(&[]), shape: &[] };

/// One stage's graph, loaded.
struct Opened {
    session: ort::session::Session,
    provider: Provider,
    skipped: Vec<Skipped>,
    signature: Signature,
    opset: Option<u32>,
}

/// Reads and scans a stage's graph file, the same refusals as a single graph's main file.
fn read_graph(source: &ModelSource<'_>, stage: &Stage) -> Result<(Vec<u8>, Option<u32>), LoadError> {
    let path = source.file(&stage.file).ok_or_else(|| LoadError::MissingFile(stage.file.clone()))?;
    let bytes = std::fs::read(path).map_err(|_| LoadError::MissingFile(stage.file.clone()))?;
    let scanned = scan::scan_model(&bytes).map_err(|error| match error {
        scan::ScanError::Refused(why) => LoadError::Refused(why),
        scan::ScanError::Malformed(why) => LoadError::Format(format!("`{}` is not an ONNX model ({why})", stage.file)),
    })?;
    Ok((bytes, scanned.opset))
}

/// Loads every stage's graph with `options`, in run order.
fn open(source: &ModelSource<'_>, options: &SessionOptions) -> Result<Vec<Opened>, LoadError> {
    let loaded = library::ensure_loaded(source.natives, &options.providers)?;
    source
        .stages
        .iter()
        .map(|stage| {
            let (bytes, opset) = read_graph(source, stage)?;
            let (session, provider, skipped) = graph::create(&bytes, options, loaded.build.id)?;
            let signature = graph::signature_of(&session)?;
            if signature.inputs.len() > MAX_STAGE_INPUTS {
                return Err(LoadError::Format(format!(
                    "stage `{}` has {} inputs, more than the {MAX_STAGE_INPUTS} a stage may have",
                    stage.name,
                    signature.inputs.len()
                )));
            }
            Ok(Opened { session, provider, skipped, signature, opset })
        })
        .collect()
}

fn plan_of(source: &ModelSource<'_>, opened: &[Opened]) -> Result<plan::Plan, LoadError> {
    let graphs: Vec<Signature> = opened.iter().map(|o| o.signature.clone()).collect();
    plan::plan(source.stages, &graphs)
}

/// [`Runtime::probe`](omuzaki_ai_api::Runtime::probe) for a model with stages: the slot inputs the stages read and
/// every stage output as `<stage>.<port>`.
///
/// # Errors
/// [`LoadError`]; wiring errors are [`LoadError::Format`] naming the stage, input and source.
pub(crate) fn probe(source: &ModelSource<'_>) -> Result<Probe, LoadError> {
    let opened = open(source, &SessionOptions::default())?;
    let plan = plan_of(source, &opened)?;
    Ok(Probe { signature: plan.signature, opset: opened.iter().filter_map(|o| o.opset).max() })
}

/// [`Runtime::load`](omuzaki_ai_api::Runtime::load) for a model with stages.
///
/// # Errors
/// [`LoadError`].
pub(crate) fn load(
    source: &ModelSource<'_>,
    slot: &Signature,
    io: &BTreeMap<String, String>,
    options: &SessionOptions,
) -> Result<Box<dyn Session>, LoadError> {
    let opened = open(source, options)?;
    let plan = plan_of(source, &opened)?;
    let named = plan.signature.with_slot_names(io);
    slot.accepts(&named).map_err(|mismatches| {
        let list: Vec<String> = mismatches.iter().map(ToString::to_string).collect();
        LoadError::Format(format!("the model does not fit the slot: {}", list.join("; ")))
    })?;
    // `accepts` matched the names, so each lookup below succeeds.
    let no_port = || LoadError::Format("a pipeline port has no slot port".to_owned());
    let slot_input = |name: &str| -> Result<usize, LoadError> {
        let at = plan.signature.inputs.iter().position(|p| p.name == name).ok_or_else(no_port)?;
        slot.inputs.iter().position(|s| s.name == named.inputs[at].name).ok_or_else(no_port)
    };
    let mut feeds = Vec::with_capacity(plan.stages.len());
    for stage in &plan.stages {
        feeds.push(
            stage
                .inputs
                .iter()
                .map(|source| match source {
                    Source::Slot(name) => slot_input(name).map(Feed::Slot),
                    Source::Stage { stage, port } => Ok(Feed::Stage { stage: *stage, port: *port }),
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    // The pipeline's outputs are the stages' outputs in order, so a flat position maps back to (stage, port).
    let locations: Vec<(usize, usize)> = opened
        .iter()
        .enumerate()
        .flat_map(|(stage, o)| (0..o.signature.outputs.len()).map(move |port| (stage, port)))
        .collect();
    let picks = slot
        .outputs
        .iter()
        .map(|port| named.outputs.iter().position(|m| m.name == port.name).map(|at| locations[at]))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(no_port)?;
    let first = opened.first().ok_or_else(|| LoadError::Format("a pipeline needs at least one stage".to_owned()))?;
    let info =
        SessionInfo { model: source.model.clone(), provider: first.provider, sandboxed: false, fallback_for: None };
    let stage_names = source.stages.iter().map(|s| s.name.clone()).collect();
    let bufs = opened.iter().map(|o| TensorBuf::for_outputs(&o.signature)).collect();
    let sessions = opened
        .into_iter()
        .map(|o| {
            let inputs = o.signature.inputs.len();
            let picks = o.signature.outputs.iter().map(|p| p.name.clone()).collect();
            OrtModelSession::new(o.session, o.signature, (0..inputs).collect(), picks, info.clone(), o.skipped)
        })
        .collect();
    Ok(Box::new(PipelineSession { names: stage_names, sessions, feeds, bufs, picks, signature: slot.clone(), info }))
}

/// Where a stage input comes from at run time.
#[derive(Clone, Copy)]
enum Feed {
    /// The caller's input at this position.
    Slot(usize),
    /// An earlier stage's output buffer.
    Stage { stage: usize, port: usize },
}

/// Several graphs run as one model: presents exactly the slot's signature.
struct PipelineSession {
    names: Vec<String>,
    sessions: Vec<OrtModelSession>,
    /// For each stage, where each of its graph's inputs reads from.
    feeds: Vec<Vec<Feed>>,
    /// For each stage, the buffers its outputs are written to and later stages read from.
    bufs: Vec<Vec<TensorBuf>>,
    /// For each slot output, the stage and port it is copied from.
    picks: Vec<(usize, usize)>,
    signature: Signature,
    info: SessionInfo,
}

/// A stage's failure, with the stage named (cold: only on errors).
fn in_stage(name: &str, error: &RunError) -> RunError {
    RunError::Failed(format!("stage `{name}`: {error}"))
}

impl PipelineSession {
    /// Checks that every slot output's actual shape fits the slot's before any caller buffer is written, so a rejected
    /// call leaves the caller's buffers as they were.
    fn check_picks(&self) -> Result<(), RunError> {
        for (index, (&(stage, port), spec)) in self.picks.iter().zip(&self.signature.outputs).enumerate() {
            let shape = self.bufs[stage][port].shape();
            let mismatch = |problem| RunError::Mismatch { port: Port::Output, index, problem };
            if spec.shape.len() != shape.len() {
                return Err(mismatch(TensorProblem::Rank { expected: spec.shape.len(), found: shape.len() }));
            }
            for (axis, (dim, &found)) in spec.shape.iter().zip(shape).enumerate() {
                if let omuzaki_ai_api::Dim::Fixed(expected) = *dim
                    && expected != found
                {
                    return Err(mismatch(TensorProblem::Dim { axis, expected, found }));
                }
            }
        }
        Ok(())
    }
}

impl Session for PipelineSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        for stage in 0..self.sessions.len() {
            let (earlier, rest) = self.bufs.split_at_mut(stage);
            let feeds = &self.feeds[stage];
            let mut refs = [UNUSED; MAX_STAGE_INPUTS];
            for (entry, feed) in refs.iter_mut().zip(feeds) {
                *entry = match *feed {
                    Feed::Slot(at) => inputs[at],
                    Feed::Stage { stage, port } => earlier[stage][port].as_ref(),
                };
            }
            self.sessions[stage]
                .run(&refs[..feeds.len()], &mut rest[0])
                .map_err(|error| in_stage(&self.names[stage], &error))?;
        }
        self.check_picks()?;
        for (out, &(stage, port)) in outputs.iter_mut().zip(&self.picks) {
            out.set(&self.bufs[stage][port].as_ref()).map_err(|problem| RunError::Failed(problem.to_string()))?;
        }
        Ok(())
    }

    fn reset(&mut self) {
        for session in &mut self.sessions {
            session.reset();
        }
    }

    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}
