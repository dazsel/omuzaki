//! Contract for the model registry and runtime (spec v3 §16 ML-01, ML-13, ML-14): every model the app runs sits in a
//! **slot**, a versioned interface with fixed inputs and outputs, and comes with a **manifest** (files pinned by
//! SHA-256, runtime, providers) and a **model card** (licence, lineage, metrics).
//!
//! ```text
//!  register_slot!  (the crate that owns the data: omuzaki-speech, omuzaki-track)   Slot: signature + evaluate
//!  register_model! (the crate that uses a shipped model)                       ModelManifest (TOML)
//!  register_runtime! (omuzaki-ai-ort for ONNX, omuzaki-speech for whisper.cpp)     Runtime: probe + load
//!                         │
//!                         ▼
//!  ModelRegistry (omuzaki-ai): choose → verify/download (FileSource) → load → Session / Stream
//!     ops (model.*), the Models page          the app wiring hands sessions and streams to the subsystems
//! ```
//!
//! | Module | What it fixes |
//! |---|---|
//! | [`id`] | slot, model and runtime ids, [`Version`], [`Sha256`], [`ModelRef`] (a model, exactly) |
//! | [`tensor`] | [`Signature`] and its matching rules, borrowed inputs ([`TensorRef`]), reusable outputs ([`TensorBuf`]) |
//! | [`session`] | the [`Session`] trait, [`Provider`]s (CPU, CUDA, `DirectML`, Core ML, WebGPU), [`SessionOptions`] |
//! | [`stream`] | streaming slots: [`StreamSpec`], the real-time [`BlockSink`] / [`BlockSource`] ends, deadlines per block |
//! | [`slot`] | the [`Slot`] trait and [`register_slot!`](crate::register_slot) |
//! | [`catalogue`] | [`CatalogueEntry`], [`SyncReport`], [`ImportError`]: models delivered between releases |
//! | [`manifest`] | [`ModelManifest`], [`ModelCard`], [`Licence`], [`Origin`], [`register_model!`](crate::register_model) |
//! | [`runtime`] | the [`Runtime`] trait (probe, load) and [`register_runtime!`](crate::register_runtime) |
//! | [`eval`] | metrics on real or synthetic test sets, floors, [`EvalReport`], the [`EvalEnv`] an evaluation uses |
//! | [`fetch`] | [`PinnedFile`] (ordered URLs), the app's resumable [`Fetcher`], the verified [`FileSource`] |
//! | [`consent`] | [`Likeness`] and the [`ConsentLookup`] for models that imitate a real person (SE-05) |
//! | [`registry`] | the [`ModelRegistry`] service, [`ModelEntry`], [`MadeWith`], the [`DISCLAIMER`] |
//!
//! ## Conventions
//!
//! - **Hot versus cold.** Hot: [`Session::run`], [`BlockSink::push`], [`BlockSource::take`] and the types they
//!   take ([`TensorRef`], [`TensorBuf`], [`RunError`], [`Block`], [`StreamStats`]); once output buffers have grown,
//!   they allocate nothing, and the stream ends never block or lock. Everything else is cold: it may allocate, lock,
//!   touch disk or take seconds, and runs on jobs and loader threads.
//! - **Threads.** The caller owns the thread that runs a session; runtimes own their maths threads
//!   ([`SessionOptions::threads`], never spinning); the registry owns stream workers and sandbox processes. The
//!   audio callback and the render thread never call [`Session::run`].
//! - **Shipped versus user models.** Shipped models are pinned, licence-registered and run in-process; user models
//!   are the person's choice and licence, run in a sandboxed worker, and pass the slot's smoke test first (ADR 0016).
//!
//! The design, the slot table and the decisions are in `docs/design/ai-api.md` and ADR 0017. `omuzaki-ai-mock` has a
//! fake runtime, sessions and fetcher for tests.
#![deny(missing_docs)]

pub mod catalogue;
pub mod consent;
pub mod eval;
pub mod fetch;
pub mod id;
pub mod manifest;
mod progress;
pub mod registry;
pub mod runtime;
pub mod session;
pub mod slot;
pub mod stream;
pub mod tensor;

pub use catalogue::{CatalogueEntry, ImportError, SyncReport};
pub use consent::{ConsentLookup, Likeness, NoConsentRecords};
pub use eval::{Better, Check, EvalEnv, EvalError, EvalLevel, EvalReport, Metric, TestSet, TestSetKind};
pub use fetch::{Body, FetchError, Fetcher, FileSource, PinnedFile, RESUME_MIN_SIZE};
pub use id::{ModelId, ModelRef, ParseError, RuntimeId, Sha256, SlotId, SlotRef, Version};
pub use manifest::{
    Licence, Lineage, LineageKind, ManifestError, ModelCard, ModelFile, ModelManifest, Origin, ShippedModel, Stage,
    shipped_models,
};
pub use progress::{NoProgress, Progress};
pub use registry::{
    AddModel, DISCLAIMER, DISCLAIMER_VERSION, MadeWith, ModelEntry, ModelRegistry, ModelStatus, RegistryError,
    Requester, SlotStatus, Stream,
};
pub use runtime::{LoadError, LocalFile, ModelSource, Probe, Runtime, RuntimeRegistration, find_runtime, runtimes};
pub use session::{Provider, Session, SessionInfo, SessionOptions};
pub use slot::{Slot, SlotInfo, SlotMode, SlotRegistration, find_slot, slots};
pub use stream::{Block, BlockSink, BlockSource, StreamSpec, StreamStats};
pub use tensor::{
    AnyDim, DType, Dim, MAX_RANK, Mismatch, Port, RunError, Signature, TensorBuf, TensorData, TensorDataMut,
    TensorProblem, TensorRef, TensorSpec,
};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;
