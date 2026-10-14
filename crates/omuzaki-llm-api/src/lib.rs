//! The LLM contract (llm-api D1 to D5, D8; ADR 0032). Five features call a model through it; the real clients are in
//! `omuzaki-llm`, the fake is `omuzaki-llm-mock`.
//!
//! - **[`LlmClient`]** starts a turn and returns a [`TurnStream`]: blocking and pull-based, so every caller is a plain
//!   thread. [`TurnStream::try_next`] never blocks, [`TurnStream::collect`] folds a stream into a [`Turn`].
//! - **[`TurnRequest`], [`Message`], [`ContentBlock`]** and friends: a provider-neutral, append-only history.
//! - **[`Usage`], [`PriceTable`], [`MicroUsd`], [`Ledger`]**: integer micro-dollars from a dated table, and a
//!   pre-request cost bound that needs no network.
//! - **[`LlmError`], [`Cancel`]**: failures, and cancellation that holds on the very next call.
//! - **[`LlmProvider`], [`register_llm_provider!`]**: providers register themselves; the secret types are local
//!   stand-ins until `omuzaki-secrets-api` and `omuzaki-settings-api` land.
//!
//! This crate has no network stack and no async runtime. [`Usage::cost`] arrives with the price-table task.
#![deny(missing_docs)]

mod error;
mod message;
mod provider;
mod stream;
mod usage;

pub use error::LlmError;
pub use message::{
    CachePlan, ContentBlock, Effort, ImageMime, ImageOrigin, Message, Role, SystemBlock, ToolChoice, ToolDef,
    TurnRequest,
};
pub use provider::{
    BuildFn, CostModel, ListModelsFn, LlmCapability, LlmProvider, Privacy, ProviderConfig, ProviderEnv,
    ProviderManifest, RetryPolicy, RuntimeHandle, SecretLabel, SecretRef, SecretStore, SecretValue, provider,
    providers,
};
pub use stream::{Cancel, EventSource, LlmEvent, Next, StopReason, Turn, TurnStream};
pub use usage::{ImageCost, Ledger, MicroUsd, ModelInfo, PriceTable, PriceTier, Rates, Usage};

#[doc(hidden)]
pub use inventory as __inventory;
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

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
