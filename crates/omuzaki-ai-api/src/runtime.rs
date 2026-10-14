//! Runtimes: what turns a model's files into a [`Session`]. ONNX Runtime (`onnx`, in `omuzaki-ai-ort`) is the main one,
//! and runs multi-graph models ([`Stage`]s) as one session too; formats that aren't ONNX bring their own
//! (`whisper-ggml` for whisper.cpp files). Runtimes self-register with
//! [`register_runtime!`](crate::register_runtime).
//!
//! A runtime is native code that parses untrusted files when a person adds a model, so for user models the registry
//! calls it only inside the sandboxed worker process (ADR 0016 rule 4); the worker binary links the same runtimes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{FetchError, FileSource, ModelRef, RuntimeId, Session, SessionOptions, Signature, Stage};

/// One verified local file of a model, by the name its manifest gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocalFile {
    /// The manifest's file name.
    pub name: String,
    /// Where the verified bytes are (the registry's cache).
    pub path: PathBuf,
}

/// What a runtime gets to load: the model's files and a source for the runtime's own native libraries.
pub struct ModelSource<'a> {
    /// Which model, for [`SessionInfo`](crate::SessionInfo).
    pub model: &'a ModelRef,
    /// Its files, verified, in manifest order; the first is the main one.
    pub files: &'a [LocalFile],
    /// Its graphs in run order, for a multi-graph model; empty for one graph (the main file).
    pub stages: &'a [Stage],
    /// Where a runtime gets its pinned native libraries (ONNX Runtime per platform and provider) on first use.
    pub natives: &'a dyn FileSource,
}

impl ModelSource<'_> {
    /// The file the manifest calls `name`.
    pub fn file(&self, name: &str) -> Option<&Path> {
        self.files.iter().find(|f| f.name == name).map(|f| f.path.as_path())
    }
}

/// What a runtime reads from a model without running it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Probe {
    /// The model's own inputs and outputs, with its own names. For a multi-graph model: the slot inputs its stages
    /// read, and every stage output as `<stage>.<port>`.
    pub signature: Signature,
    /// The ONNX opset it needs, when the format has one.
    pub opset: Option<u32>,
}

/// Why a model could not be probed or loaded.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum LoadError {
    /// A file the runtime needs isn't in the model.
    #[error("the model has no file `{0}`")]
    MissingFile(String),
    /// The bytes aren't a model this runtime reads.
    #[error("not a model this runtime can read: {0}")]
    Format(String),
    /// The model asks for something we never allow: custom-op libraries, other native code, or files outside its own
    /// (ONNX external data by path) (ADR 0016 rule 4).
    #[error("refused: {0}")]
    Refused(String),
    /// The runtime's native library could not be had.
    #[error("the runtime's library: {0}")]
    Native(#[from] FetchError),
    /// The runtime failed to create the session.
    #[error("loading failed: {0}")]
    Runtime(String),
}

/// Loads models of one format. Implementations are stateless handles over a process-wide engine (one ONNX Runtime
/// environment per process, created on first load from its pinned library).
pub trait Runtime: Send + Sync {
    /// Its id, which manifests name in `runtime`.
    fn id(&self) -> RuntimeId;

    /// Reads the model's own signature and opset without running it, refusing what [`LoadError::Refused`] lists.
    ///
    /// # Errors
    /// [`LoadError`].
    fn probe(&self, source: &ModelSource<'_>) -> Result<Probe, LoadError>;

    /// Loads a session that presents exactly `slot` (the slot's signature, in its order): `io` maps a slot port name
    /// to the model's own name where they differ (the manifest's `io` table). Tries `options.providers` in order and
    /// falls back to the CPU; [`Session::info`] reports which one it got. Cold: it may take seconds.
    ///
    /// # Errors
    /// [`LoadError`].
    fn load(
        &self,
        source: &ModelSource<'_>,
        slot: &Signature,
        io: &BTreeMap<String, String>,
        options: &SessionOptions,
    ) -> Result<Box<dyn Session>, LoadError>;
}

/// One registered runtime; build with [`RuntimeRegistration::of`] through
/// [`register_runtime!`](crate::register_runtime).
pub struct RuntimeRegistration {
    /// Builds an instance.
    pub create: fn() -> Box<dyn Runtime>,
}

fn make_runtime<T: Runtime + Default + 'static>() -> Box<dyn Runtime> {
    Box::new(T::default())
}

impl RuntimeRegistration {
    /// The registration for `T`. A `const fn` so [`register_runtime!`](crate::register_runtime) works at link time.
    pub const fn of<T: Runtime + Default + 'static>() -> Self {
        Self { create: make_runtime::<T> }
    }
}

inventory::collect!(RuntimeRegistration);

/// Registers a runtime type: `register_runtime!(OrtRuntime);`. Put it next to the type.
#[macro_export]
macro_rules! register_runtime {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::RuntimeRegistration::of::<$ty>() }
        $crate::__record_registration!("runtime", $ty);
    };
}

/// Every registered runtime, in link order.
pub fn runtimes() -> impl Iterator<Item = Box<dyn Runtime>> {
    inventory::iter::<RuntimeRegistration>.into_iter().map(|r| (r.create)())
}

/// The runtime registered as `id`.
pub fn find_runtime(id: &RuntimeId) -> Option<Box<dyn Runtime>> {
    runtimes().find(|r| &r.id() == id)
}
