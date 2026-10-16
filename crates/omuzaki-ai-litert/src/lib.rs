//! The `tflite` runtime: LiteRT's TFLite interpreter with the XNNPACK delegate (ADR 0061, `docs/design/litert-runtime.md`),
//! behind `omuzaki-ai-api`'s [`Runtime`].
//!
//! - **Builds** ([`builds`]): one archive per platform, built in CI from `tools/litert/pins.toml`; no published pin
//!   yet (#1748), so the archive comes with the SHA-256 of the run that built it.
//! - **The library** ([`load_archive`]): unpacked through `omuzaki-ai-native`, loaded once per process, every C
//!   function of design §3.2 resolved, and its version, schema version and shim ABI checked before anything runs.
//! - **The scan** ([`scan`]): every model passes design §5's allow-list before LiteRT sees its bytes: one subgraph,
//!   builtin operators that keep no state, no variables, no data outside the file, nothing newer than the pinned
//!   schema. `probe` is the scan alone, so it runs no native code on the bytes.
//! - **Sessions** ([`LiteRtSession`]): the scan first, then one interpreter with the shim's XNNPACK delegate on the
//!   process's library, presenting exactly the slot's signature, with no allocation in a run after warm-up. There is
//!   no published pin yet (#1748), so `load` uses the library [`load_archive`] loaded and refuses when there is none.
//!
//! The runtime registers itself as `tflite`; link this crate and [`omuzaki_ai_api::find_runtime`] finds it.
#![deny(missing_docs)]
// Product names (LiteRT, TFLite, XNNPACK, FlatBuffers) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod builds;
#[cfg(test)]
mod fake;
mod library;
pub mod scan;
mod session;

use std::collections::BTreeMap;

use omuzaki_ai_api::{LoadError, ModelSource, Probe, Runtime, RuntimeId, SessionOptions, Signature, register_runtime};

pub use library::{Loaded, MAX_VERSION_BYTES, check_library, load_archive, loaded};
pub use session::{CPU_ONLY, ERROR_BYTES, LiteRtSession};

/// The runtime id manifests name in `runtime`.
pub const RUNTIME_ID: &str = "tflite";

/// LiteRT as a [`Runtime`]. A stateless handle over the process-wide library.
#[derive(Default)]
pub struct LiteRtRuntime;

register_runtime!(LiteRtRuntime);

/// Why `load` refuses before [`load_archive`] has loaded a library.
const NOT_LOADED: &str = "the LiteRT library is not loaded: there is no published build to fetch yet (#1748), so \
                          the caller loads the archive of the CI run that built it with `load_archive` first";

/// The model's one graph, read whole and scanned, with its bytes: the main file of a single-graph model.
fn main_graph(source: &ModelSource<'_>) -> Result<(Vec<u8>, scan::Scan), LoadError> {
    if !source.stages.is_empty() {
        return Err(LoadError::Format("multi-graph models are not supported yet".to_owned()));
    }
    let file = source.files.first().ok_or_else(|| LoadError::MissingFile("(the main model file)".to_owned()))?;
    let missing = || LoadError::MissingFile(file.name.clone());
    let size = std::fs::metadata(&file.path).map_err(|_| missing())?.len();
    if size > scan::MAX_MODEL_BYTES as u64 {
        return Err(LoadError::Refused(format!(
            "the model is larger than MAX_MODEL_BYTES ({} bytes)",
            scan::MAX_MODEL_BYTES
        )));
    }
    let bytes = std::fs::read(&file.path).map_err(|_| missing())?;
    let scan = scan::scan_model(&bytes)?;
    Ok((bytes, scan))
}

impl Runtime for LiteRtRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(RUNTIME_ID).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        Ok(Probe { signature: main_graph(source)?.1.signature, opset: None })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn omuzaki_ai_api::Session>, LoadError> {
        Ok(Box::new(self.load_session(source, slot, io, options)?))
    }
}

impl LiteRtRuntime {
    /// [`Runtime::load`] with the concrete session, which also reports the providers it skipped
    /// ([`LiteRtSession::skipped_providers`]).
    ///
    /// # Errors
    /// [`LoadError`]: the scan's refusals, [`LoadError::Runtime`] before [`load_archive`] has loaded a library or
    /// when LiteRT can't build the interpreter, [`LoadError::Format`] when the model doesn't fit the slot.
    pub fn load_session(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<LiteRtSession, LoadError> {
        let (bytes, scan) = main_graph(source)?;
        let library = loaded().ok_or_else(|| LoadError::Runtime(NOT_LOADED.to_owned()))?;
        session::create(library.api, bytes, &scan.signature, slot, io, options, source.model.clone())
    }
}

omuzaki_core::define_link!();
