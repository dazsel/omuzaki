//! The sandbox for user models (ADR 0016 rule 4, design `docs/design/ai-api.md` section 9): every model the person
//! adds is probed, loaded, evaluated and run in a separate worker process, `omuzaki-model-worker`, never in the app.
//!
//! - **The worker** is a thin App binary that links the runtime crates and calls [`serve`]: it reads requests on stdin
//!   and answers on stdout (`wire`). It links no network stack: a runtime's pinned native library is fetched by the
//!   registry through its verified cache, at the worker's request.
//! - **Limits** (`limits`): a memory cap, a lower CPU priority, and death with the registry's process.
//! - **[`SandboxedSession`]** is just another `Session`: inputs are copied into the pipe and outputs back into the
//!   caller's buffers, allocation-free once they have grown. A run past the slot's deadline, or a worker that dies, is
//!   `RunError::Crashed` with the worker killed; the slot session then falls back and the health meter says why.
//! - **Refusals** are the runtime's own (`LoadError::Refused`: custom ops, external data, native code a model names);
//!   the worker adds no custom-op registration and loads nothing a model names.
//! - **On the web** there are no processes, so user models are refused there.

mod limits;
mod session;
mod wire;
mod worker;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use omuzaki_ai_api::{FileSource, LoadError, ModelRef, Probe, RuntimeId, SessionOptions, Signature, Stage};

pub use session::SandboxedSession;
pub use worker::serve;

/// How long a worker may take to start and say so.
const START_LIMIT: Duration = Duration::from_secs(30);

/// How long a probe or a load may take in the worker, not counting the native libraries the registry fetches for it.
const LOAD_LIMIT: Duration = Duration::from_secs(120);

/// The memory cap of a worker unless the app sets another: room for the large speech models, not for a runaway.
pub const DEFAULT_MEMORY_MIB: u64 = 8 * 1024;

/// How the registry starts a model worker, and the limits it runs under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SandboxSettings {
    /// The worker program: by default `omuzaki-model-worker` next to the running executable.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// Variables added to its environment.
    pub env: Vec<(OsString, OsString)>,
    /// The memory cap, in MiB.
    pub memory_mib: u64,
}

impl Default for SandboxSettings {
    fn default() -> Self {
        let name = format!("omuzaki-model-worker{}", std::env::consts::EXE_SUFFIX);
        let beside = std::env::current_exe().ok().and_then(|exe| exe.parent().map(|dir| dir.join(&name)));
        Self {
            program: beside.unwrap_or_else(|| name.into()),
            args: Vec::new(),
            env: Vec::new(),
            memory_mib: DEFAULT_MEMORY_MIB,
        }
    }
}

/// A model for the worker: which one, its runtime and its verified files.
pub struct Model<'a> {
    /// The model, for its sessions' info.
    pub model: &'a ModelRef,
    /// The runtime the manifest names.
    pub runtime: &'a RuntimeId,
    /// `(manifest name, verified path)`, main file first.
    pub files: Vec<(String, PathBuf)>,
    /// Its graphs, for a multi-graph model.
    pub stages: &'a [Stage],
}

impl Model<'_> {
    fn job(&self) -> wire::Job {
        wire::Job {
            model: self.model.clone(),
            runtime: self.runtime.clone(),
            files: self.files.clone(),
            stages: self.stages.to_vec(),
        }
    }
}

/// What a session loads to.
pub struct LoadSpec<'a> {
    /// The slot's signature, which the session presents.
    pub slot: &'a Signature,
    /// The manifest's renames, slot port to model port.
    pub io: &'a BTreeMap<String, String>,
    /// Providers and threads.
    pub options: SessionOptions,
    /// A run that takes longer is a stall: the worker is killed and the run is `RunError::Crashed`.
    pub stall: Duration,
}

/// Probes `model` in a fresh worker, which ends with the probe. The worker asks `natives` for the runtime's pinned
/// libraries.
///
/// # Errors
/// The runtime's [`LoadError`]; a worker that couldn't start, died or stalled is a `LoadError::Runtime` that says so.
pub fn probe(settings: &SandboxSettings, model: &Model<'_>, natives: &dyn FileSource) -> Result<Probe, LoadError> {
    session::probe(settings, model, natives)
}

/// Loads `model` in a fresh worker, which then serves the session until it is dropped.
///
/// # Errors
/// As [`probe`].
pub fn load(
    settings: &SandboxSettings,
    model: &Model<'_>,
    spec: LoadSpec<'_>,
    natives: &dyn FileSource,
) -> Result<SandboxedSession, LoadError> {
    SandboxedSession::load(settings, model, spec, natives)
}
