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
//! - **Sessions** arrive with #1750; until then `load` scans and refuses.
//!
//! The runtime registers itself as `tflite`; link this crate and [`omuzaki_ai_api::find_runtime`] finds it.
#![deny(missing_docs)]
// Product names (LiteRT, TFLite, XNNPACK, FlatBuffers) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod builds;
mod library;
pub mod scan;

use std::collections::BTreeMap;

use omuzaki_ai_api::{LoadError, ModelSource, Probe, Runtime, RuntimeId, SessionOptions, Signature, register_runtime};

pub use library::{Loaded, MAX_VERSION_BYTES, check_library, load_archive, loaded};

/// The runtime id manifests name in `runtime`.
pub const RUNTIME_ID: &str = "tflite";

/// LiteRT as a [`Runtime`]. A stateless handle over the process-wide library.
#[derive(Default)]
pub struct LiteRtRuntime;

register_runtime!(LiteRtRuntime);

/// The model's one graph, read whole and scanned: the main file of a single-graph model.
fn main_graph(source: &ModelSource<'_>) -> Result<scan::Scan, LoadError> {
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
    scan::scan_model(&std::fs::read(&file.path).map_err(|_| missing())?)
}

impl Runtime for LiteRtRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(RUNTIME_ID).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        Ok(Probe { signature: main_graph(source)?.signature, opset: None })
    }

    fn load(
        &self,
        source: &ModelSource<'_>,
        _slot: &Signature,
        _io: &BTreeMap<String, String>,
        _options: &SessionOptions,
    ) -> Result<Box<dyn omuzaki_ai_api::Session>, LoadError> {
        main_graph(source)?;
        Err(LoadError::Runtime("the `tflite` runtime cannot run models yet: its session arrives with #1750".to_owned()))
    }
}

omuzaki_core::define_link!();
