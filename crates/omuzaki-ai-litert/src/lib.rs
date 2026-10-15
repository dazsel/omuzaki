//! The `tflite` runtime: LiteRT's TFLite interpreter with the XNNPACK delegate (ADR 0061, `docs/design/litert-runtime.md`),
//! behind `omuzaki-ai-api`'s [`Runtime`].
//!
//! - **Builds** ([`builds`]): one archive per platform, built in CI from `tools/litert/pins.toml`; no published pin
//!   yet (#1748), so the archive comes with the SHA-256 of the run that built it.
//! - **The library** ([`load_archive`]): unpacked through `omuzaki-ai-native`, loaded once per process, every C
//!   function of design §3.2 resolved, and its version, schema version and shim ABI checked before anything runs.
//! - **Not yet:** the model scan (design §5) arrives with the second part of #1749 and sessions with #1750; until then
//!   `probe` and `load` refuse.
//!
//! The runtime registers itself as `tflite`; link this crate and [`omuzaki_ai_api::find_runtime`] finds it.
#![deny(missing_docs)]
// Product names (LiteRT, TFLite, XNNPACK, FlatBuffers) are prose here, not code.
#![allow(clippy::doc_markdown)]

pub mod builds;
mod library;

use std::collections::BTreeMap;

use omuzaki_ai_api::{LoadError, ModelSource, Probe, Runtime, RuntimeId, SessionOptions, Signature, register_runtime};

pub use library::{Loaded, MAX_VERSION_BYTES, check_library, load_archive, loaded};

/// The runtime id manifests name in `runtime`.
pub const RUNTIME_ID: &str = "tflite";

/// LiteRT as a [`Runtime`]. A stateless handle over the process-wide library.
#[derive(Default)]
pub struct LiteRtRuntime;

register_runtime!(LiteRtRuntime);

/// Why `probe` and `load` refuse for now.
const NOT_YET: &str =
    "the `tflite` runtime cannot read models yet: its model scan arrives with #1749 and its session with #1750";

impl Runtime for LiteRtRuntime {
    fn id(&self) -> RuntimeId {
        RuntimeId::new(RUNTIME_ID).unwrap_or_else(|_| unreachable!("a literal slug"))
    }

    fn probe(&self, _source: &ModelSource<'_>) -> Result<Probe, LoadError> {
        Err(LoadError::Runtime(NOT_YET.to_owned()))
    }

    fn load(
        &self,
        _source: &ModelSource<'_>,
        _slot: &Signature,
        _io: &BTreeMap<String, String>,
        _options: &SessionOptions,
    ) -> Result<Box<dyn omuzaki_ai_api::Session>, LoadError> {
        Err(LoadError::Runtime(NOT_YET.to_owned()))
    }
}

omuzaki_core::define_link!();
