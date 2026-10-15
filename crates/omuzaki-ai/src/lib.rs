//! The model registry's implementation (`omuzaki-ai-api` is the contract; design `docs/design/ai-api.md`, ADR 0017):
//!
//! | Module | Holds |
//! |---|---|
//! | [`VerifiedCache`] | the one path every pinned file (model weights, runtime libraries, eval fixtures, fonts) is fetched, verified and cached by (section 8) |
//! | [`Registry`] | the `ModelRegistry`: shipped and user models, install, selection, acknowledgement, consent, and the slot sessions `open` returns (sections 4 to 7) |
//! | [`sandbox`] | the worker process user models run in, and [`sandbox::serve`], the worker's loop (section 9) |
//! | [`ops`] | the `model.*` ops (section 11) |
//!
//! The ops register themselves; a binary keeps them only if it links this crate, so an app that wants them calls
//! [`link`] at startup (ADR 0015).
#![deny(missing_docs)]

#[allow(
    clippy::disallowed_methods,
    reason = "the cache, the state file and the worker are the app's own files, never the person's; the ops stay fenced"
)]
mod fetch;
pub mod ops;
#[allow(
    clippy::disallowed_methods,
    reason = "the cache, the state file and the worker are the app's own files, never the person's; the ops stay fenced"
)]
mod registry;
#[allow(
    clippy::disallowed_methods,
    reason = "the cache, the state file and the worker are the app's own files, never the person's; the ops stay fenced"
)]
pub mod sandbox;
mod slot_session;

pub use fetch::{VerifiedCache, model_ref, sha256_file};
pub use registry::{CACHE_DIR, IMPERSONATION_WARNING, Registry, RegistrySettings};

omuzaki_core::define_link!();
