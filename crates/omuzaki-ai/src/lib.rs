//! The model registry's implementation (`omuzaki-ai-api` is the contract), starting with [`VerifiedCache`]: the one
//! path every pinned file (model weights, runtime libraries, eval fixtures, fonts) is fetched, verified and cached by.
//! The design is `docs/design/ai-api.md` section 8 and ADR 0017.
#![deny(missing_docs)]

mod fetch;
mod registry;

pub use fetch::{VerifiedCache, model_ref, sha256_file};
pub use registry::{CACHE_DIR, Registry, RegistrySettings};
