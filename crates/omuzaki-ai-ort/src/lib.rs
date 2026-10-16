//! The `onnx` runtime: ONNX Runtime through `ort`, loaded from a pinned Microsoft build on first use (`omuzaki-ai-api`
//! is the contract, `docs/design/ai-api.md` §9 the design, ADR 0017 the decisions).
//!
//! - **Builds** ([`builds`]): Microsoft's release archives, pinned by URL list, size and SHA-256, one per platform and
//!   flavour; the CPU build unless the person's settings ask for CUDA; Core ML is part of the macOS build. They come
//!   from the app's [`FileSource`](omuzaki_ai_api::FileSource), the one verified path every pinned file takes.
//! - **Once per process.** The app calls [`prefer_providers`] at start-up with the person's settings, before any probe,
//!   because a probe wants no provider and would otherwise fix the CPU build for good. The library is unpacked next to
//!   its verified archive and handed to `ort::init_from` once.
//!   `ort` keeps the first library it is given for the life of the process, so if `omuzaki-audio` (which loads its own
//!   pin of the same version for the speech host) initialised it first, that copy serves every session and ours is not
//!   used; the flavour is fixed by whichever crate loads first. Both pin 1.28.3, so only a provider flavour (CUDA) can
//!   differ, and an unavailable provider falls back to the CPU and is reported.
//! - **Providers**: [`SessionOptions::providers`](omuzaki_ai_api::SessionOptions) in order, skipping what the build or
//!   machine lacks, always ending on the CPU. [`SessionInfo::provider`](omuzaki_ai_api::SessionInfo) is the one used and
//!   [`OrtModelSession::skipped_providers`] says why the others weren't.
//! - **No allocation per run** once a session has seen its shapes: inputs are copied into tensors the session owns and
//!   outputs go to preallocated tensors, rebuilt only when a call's input shapes change (`session.rs`).
//! - **Known exceptions:** two cases allocate inside `ort` itself: a model with more than 6 inputs or 4 outputs
//!   (7 allocations per run for 7 and 5), and port names whose list totals 768 bytes or more (1 per run for an
//!   800-byte name). Both are measured and pinned in `tests/allocations.rs`.
//! - **Quiet**: one inter-op thread, `threads` intra-op threads, and no spin-waiting in either pool.
//! - **Refusals** (ADR 0016 rule 4): custom-op domains and external data are refused before ONNX Runtime sees the
//!   bytes ([`scan`]); `register_custom_ops_library` is never called. Models load from memory, so a relative
//!   external-data path could not resolve anyway.
//! - **Multi-graph models** (`[[stages]]`) run as one session (`pipeline`): one ONNX Runtime session per stage with the
//!   same options, stage inputs read from slot inputs or earlier stages' outputs, intermediate tensors in buffers reused
//!   by every run. `probe` reports the slot inputs the stages read and every stage output as `<stage>.<port>`; the
//!   manifest's `io` maps slot outputs to those names. [`OrtRuntime::load_session`] is for single graphs only.
//!
//! The runtime registers itself as `onnx`; link this crate and [`omuzaki_ai_api::find_runtime`] finds it.
#![deny(missing_docs)]

pub mod builds;
mod graph;
mod library;
mod pipeline;
mod scan;
mod session;

use std::collections::BTreeMap;

use omuzaki_ai_api::{
    LoadError, ModelSource, Probe, Runtime, RuntimeId, SessionInfo, SessionOptions, Signature, register_runtime,
};

pub use graph::Skipped;
pub use library::{Loaded, loaded, prefer_providers};
#[cfg(feature = "fuzzing")] // the model scan, for the fuzz target in `fuzz/` (#1653); never on in a build
pub use scan::{Scan, ScanError, scan_model};
pub use session::OrtModelSession;

/// The runtime id manifests name in `runtime`.
pub const RUNTIME_ID: &str = "onnx";

/// ONNX Runtime as a [`Runtime`]. A stateless handle over the process-wide engine.
#[derive(Default)]
pub struct OrtRuntime;

register_runtime!(OrtRuntime);

/// The model's one graph, read whole: the main file of a single-graph model.
fn main_graph(source: &ModelSource<'_>) -> Result<Vec<u8>, LoadError> {
    if !source.stages.is_empty() {
        return Err(LoadError::Format(
            "a multi-graph model loads through `Runtime::load`, not `load_session`".to_owned(),
        ));
    }
    let file = source.files.first().ok_or_else(|| LoadError::MissingFile("(the main model file)".to_owned()))?;
    let bytes = std::fs::read(&file.path).map_err(|_| LoadError::MissingFile(file.name.clone()))?;
    scan::scan_model(&bytes).map_err(|error| match error {
        scan::ScanError::Refused(why) => LoadError::Refused(why),
        scan::ScanError::Malformed(why) => LoadError::Format(format!("not an ONNX model ({why})")),
    })?;
    Ok(bytes)
}

impl Runtime for OrtRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(RUNTIME_ID).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        if !source.stages.is_empty() {
            return pipeline::probe(source);
        }
        let bytes = main_graph(source)?;
        let opset = scan::scan_model(&bytes).ok().and_then(|scan| scan.opset);
        let loaded = library::ensure_loaded(source.natives, &[])?;
        let (session, _, _) = graph::create(&bytes, &SessionOptions::default(), loaded.build.id)?;
        Ok(Probe { signature: graph::signature_of(&session)?, opset })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn omuzaki_ai_api::Session>, LoadError> {
        if !source.stages.is_empty() {
            return pipeline::load(source, slot, io, options);
        }
        Ok(Box::new(self.load_session(source, slot, io, options)?))
    }
}

impl OrtRuntime {
    /// [`Runtime::load`] with the concrete session, which also reports the providers that were skipped
    /// ([`OrtModelSession::skipped_providers`]).
    ///
    /// # Errors
    /// [`LoadError`].
    pub fn load_session(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<OrtModelSession, LoadError> {
        let bytes = main_graph(source)?;
        let loaded = library::ensure_loaded(source.natives, &options.providers)?;
        let (session, provider, skipped) = graph::create(&bytes, options, loaded.build.id)?;
        let model = graph::signature_of(&session)?;
        let named = model.with_slot_names(io);
        slot.accepts(&named).map_err(|mismatches| {
            let list: Vec<String> = mismatches.iter().map(ToString::to_string).collect();
            LoadError::Format(format!("the model does not fit the slot: {}", list.join("; ")))
        })?;
        // Which slot port feeds each model input, and the model's name for each slot output. The names matched above,
        // so every lookup succeeds.
        let feeds = named
            .inputs
            .iter()
            .map(|port| slot.inputs.iter().position(|s| s.name == port.name))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| LoadError::Format("a model input has no slot input".to_owned()))?;
        let picks = slot
            .outputs
            .iter()
            .map(|port| named.outputs.iter().position(|m| m.name == port.name).map(|i| model.outputs[i].name.clone()))
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| LoadError::Format("a slot output has no model output".to_owned()))?;
        let info = SessionInfo { model: source.model.clone(), provider, sandboxed: false, fallback_for: None };
        Ok(OrtModelSession::new(session, slot.clone(), feeds, picks, info, skipped))
    }
}

omuzaki_core::define_link!();
