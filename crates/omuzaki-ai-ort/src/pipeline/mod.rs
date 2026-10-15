//! Multi-graph models (`[[stages]]`) as one [`Session`]: the graphs run in manifest order, each stage reading slot
//! inputs or earlier stages' outputs, and the slot sees one model (`docs/design/ai-api.md` §3, ADR 0017).
//!
//! - **Wiring** ([`plan`]) is checked from the graphs' signatures, the same way for `probe` and `load`.
//! - **One ONNX Runtime session per stage**, all built with the same [`SessionOptions`]; each is an
//!   [`OrtModelSession`], so a stage keeps the no-allocation-per-run behaviour of a single-graph model.
//! - **Intermediate tensors** live in one [`TensorBuf`] per stage output, made at load and reused by every run, so a
//!   run allocates nothing here once the buffers have grown to the shapes it sees. The input list of each stage is a
//!   `Vec` sized at load and reused ([`recycle`]), so a stage may have any number of inputs.
//! - **Only what is read is computed:** a stage asks ONNX Runtime for the outputs a later stage consumes or the slot
//!   selects ([`plan::reads`]); `probe` still reports all of them.
//! - **One provider for all stages:** if the stages would land on different providers, all are loaded again on one.

mod plan;

use std::collections::BTreeMap;

use omuzaki_ai_api::{
    LoadError, ModelSource, Port, Probe, Provider, RunError, Session, SessionInfo, SessionOptions, Signature, Stage,
    TensorBuf, TensorProblem, TensorRef,
};

use crate::graph::{self, Skipped};
use crate::library;
use crate::scan;
use crate::session::OrtModelSession;
use plan::Source;

/// One stage's graph file, read and scanned.
struct Graph {
    bytes: Vec<u8>,
    opset: Option<u32>,
}

/// One stage's graph, loaded.
struct Opened {
    session: ort::session::Session,
    provider: Provider,
    skipped: Vec<Skipped>,
    signature: Signature,
}

/// Reads and scans a stage's graph file, the same refusals as a single graph's main file.
fn read_graph(source: &ModelSource<'_>, stage: &Stage) -> Result<Graph, LoadError> {
    let path = source.file(&stage.file).ok_or_else(|| LoadError::MissingFile(stage.file.clone()))?;
    let bytes = std::fs::read(path).map_err(|_| LoadError::MissingFile(stage.file.clone()))?;
    let scanned = scan::scan_model(&bytes).map_err(|error| match error {
        scan::ScanError::Refused(why) => LoadError::Refused(why),
        scan::ScanError::Malformed(why) => LoadError::Format(format!("`{}` is not an ONNX model ({why})", stage.file)),
    })?;
    Ok(Graph { bytes, opset: scanned.opset })
}

/// Creates a session for each graph with `options`; each stage picks its own provider.
fn create_all(graphs: &[Graph], options: &SessionOptions, build: &str) -> Result<Vec<Opened>, LoadError> {
    graphs
        .iter()
        .map(|graph| {
            let (session, provider, skipped) = graph::create(&graph.bytes, options, build)?;
            let signature = graph::signature_of(&session)?;
            Ok(Opened { session, provider, skipped, signature })
        })
        .collect()
}

/// Loads every stage's graph on one provider, in run order. A stage whose provider fails would otherwise fall back to
/// the CPU alone and leave the pipeline on a mix, so then every stage is loaded again on the best single provider that
/// all of them accept, ending on the CPU, which always works.
fn open(source: &ModelSource<'_>, graphs: &[Graph], options: &SessionOptions) -> Result<Vec<Opened>, LoadError> {
    let loaded = library::ensure_loaded(source.natives, &options.providers)?;
    let build = loaded.build.id;
    let first = create_all(graphs, options, build)?;
    if first.windows(2).all(|pair| pair[0].provider == pair[1].provider) {
        return Ok(first);
    }
    for &provider in options.providers.iter().filter(|p| **p != Provider::Cpu) {
        let mut only = options.clone();
        only.providers = vec![provider];
        let tried = create_all(graphs, &only, build)?;
        if tried.iter().all(|opened| opened.provider == provider) {
            return Ok(tried);
        }
    }
    let mut cpu = options.clone();
    cpu.providers = vec![Provider::Cpu];
    create_all(graphs, &cpu, build)
}

fn read_all(source: &ModelSource<'_>) -> Result<Vec<Graph>, LoadError> {
    source.stages.iter().map(|stage| read_graph(source, stage)).collect()
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
    let graphs = read_all(source)?;
    let opened = open(source, &graphs, &SessionOptions::default())?;
    let plan = plan_of(source, &opened)?;
    Ok(Probe { signature: plan.signature, opset: graphs.iter().filter_map(|g| g.opset).max() })
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
    let graphs = read_all(source)?;
    let opened = open(source, &graphs, options)?;
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
    let counts: Vec<usize> = opened.iter().map(|o| o.signature.outputs.len()).collect();
    let reads = plan::reads(&plan, &counts, &picks);
    let mut feeds = Vec::with_capacity(plan.stages.len());
    for stage in &plan.stages {
        feeds.push(
            stage
                .inputs
                .iter()
                .map(|source| match *source {
                    Source::Slot(ref name) => slot_input(name).map(Feed::Slot),
                    Source::Stage { stage, port } => reads[stage][port]
                        .map(|port| Feed::Stage { stage, port })
                        .ok_or_else(|| LoadError::Format("a stage reads an output it did not fetch".to_owned())),
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    let picks = picks
        .iter()
        .map(|&(stage, port)| reads[stage][port].map(|port| (stage, port)).ok_or_else(no_port))
        .collect::<Result<Vec<_>, _>>()?;
    let provider = opened
        .first()
        .map(|o| o.provider)
        .ok_or_else(|| LoadError::Format("a pipeline needs at least one stage".to_owned()))?;
    let info = SessionInfo { model: source.model.clone(), provider, sandboxed: false, fallback_for: None };
    let mut sessions = Vec::with_capacity(opened.len());
    let mut bufs = Vec::with_capacity(opened.len());
    for (opened, read) in opened.into_iter().zip(&reads) {
        // The stage's view of its own graph: all its inputs, and only the outputs something reads.
        let fetched: Vec<_> = read
            .iter()
            .zip(&opened.signature.outputs)
            .filter(|(at, _)| at.is_some())
            .map(|(_, spec)| spec.clone())
            .collect();
        let inputs = opened.signature.inputs.len();
        let picks = fetched.iter().map(|spec| spec.name.clone()).collect();
        let view = Signature { inputs: opened.signature.inputs, outputs: fetched };
        bufs.push(TensorBuf::for_outputs(&view));
        sessions.push(OrtModelSession::new(
            opened.session,
            view,
            (0..inputs).collect(),
            picks,
            info.clone(),
            opened.skipped,
        ));
    }
    let scratch = feeds.iter().map(|feeds| Vec::with_capacity(feeds.len())).collect();
    let stage_names = source.stages.iter().map(|s| s.name.clone()).collect();
    Ok(Box::new(PipelineSession {
        names: stage_names,
        sessions,
        feeds,
        scratch,
        bufs,
        picks,
        signature: slot.clone(),
        info,
    }))
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
    /// For each stage, the allocation of its input list, empty between runs.
    scratch: Vec<Vec<TensorRef<'static>>>,
    /// For each stage, the buffers its outputs are written to and later stages read from.
    bufs: Vec<Vec<TensorBuf>>,
    /// For each slot output, the stage and port it is copied from.
    picks: Vec<(usize, usize)>,
    signature: Signature,
    info: SessionInfo,
}

/// Empties `list` and returns its allocation for a list of borrows with another lifetime, so a run can build its
/// input list without allocating. (`collect` over an emptied `into_iter` reuses the buffer when the element layout is
/// the same, which it is; `tests/pipeline.rs` counts allocations and would show a std that stopped doing so.)
fn recycle<'b>(mut list: Vec<TensorRef<'_>>) -> Vec<TensorRef<'b>> {
    list.clear();
    list.into_iter().map(|_| unreachable!("the list was just emptied")).collect()
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
            let mut refs = recycle(std::mem::take(&mut self.scratch[stage]));
            refs.extend(feeds.iter().map(|feed| match *feed {
                Feed::Slot(at) => inputs[at],
                Feed::Stage { stage, port } => earlier[stage][port].as_ref(),
            }));
            let result = self.sessions[stage].run(&refs, &mut rest[0]);
            self.scratch[stage] = recycle(refs);
            result.map_err(|error| in_stage(&self.names[stage], &error))?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recycling_an_input_list_keeps_its_allocation() {
        let list: Vec<TensorRef<'static>> = Vec::with_capacity(12);
        let at = list.as_ptr();
        let again = recycle(list);
        assert_eq!((again.as_ptr(), again.capacity(), again.len()), (at, 12, 0));
    }
}
