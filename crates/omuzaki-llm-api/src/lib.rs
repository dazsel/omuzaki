//! The LLM contract (llm-api D1 to D5, D8; ADR 0032). Five features call a model through it; the real clients are in
//! `omuzaki-llm`, the fake is `omuzaki-llm-mock`.
//!
//! Callers import by module path (ADR 0054 §2): `omuzaki_llm_api::message::TurnRequest`.
//!
//! - **[`LlmClient`]** starts a turn and returns a [`stream::TurnStream`]: blocking and pull-based, so every caller is
//!   a plain thread.
//! - **[`message`]**: [`message::TurnRequest`], [`message::Message`], [`message::ContentBlock`] and friends, a
//!   provider-neutral, append-only history.
//! - **[`stream`]**: events, [`stream::TurnStream`] (`try_next` never blocks, `collect` folds a stream into a
//!   [`stream::Turn`]) and [`stream::Cancel`], which holds on the very next call.
//! - **[`usage`]**: [`usage::Usage`], integer [`usage::MicroUsd`] from a dated [`usage::PriceTable`], and the
//!   [`usage::Ledger`]'s pre-request cost bound that needs no network.
//! - **[`error`]**: [`error::LlmError`].
//! - **[`provider`]**: providers register themselves with [`register_llm_provider!`]; the secret types there are local
//!   stand-ins until `omuzaki-secrets-api` and `omuzaki-settings-api` land.
//!
//! This crate has no network stack and no async runtime. `Usage::cost` arrives with the price-table task.
#![deny(missing_docs)]

pub mod error;
pub mod message;
pub mod provider;
pub mod stream;
pub mod usage;

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

use error::LlmError;
use message::TurnRequest;
use stream::{Cancel, TurnStream};
use usage::ModelInfo;

/// A model behind a provider.
pub trait LlmClient: Send + Sync {
    /// What the model can do.
    fn info(&self) -> &ModelInfo;

    /// Validates the request and starts a turn. Does no I/O on the calling thread. The stream obeys `cancel` on every
    /// call.
    ///
    /// # Errors
    /// `Unsupported` for a request the model cannot take, `Malformed` for one the contract forbids, `Cancelled` if
    /// `cancel` is already set.
    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError>;
}
