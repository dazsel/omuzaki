//! The LLM providers (llm-api D5, D6, D11): the Anthropic Messages and OpenAI-compatible chat clients over one HTTP,
//! SSE and retry stack, on the host's tokio runtime, with the dated price data and model listing.
//!
//! - **[`anthropic`]**: `POST /v1/messages`, the hosted API.
//! - **[`runtime`]**: the runtime handle a provider is built with.
//! - **[`prices`]**: the shipped model table.
//!
//! Every request goes through the private `http` module, the one place that touches `reqwest`. Providers register
//! themselves with `register_llm_provider!`; an App links this crate with [`link`]. The MCP mapping lives in
//! `omuzaki_llm_api::mcp`, where the assistant host can reach it without linking this crate.
#![deny(missing_docs)]

pub mod anthropic;
mod chan;
mod common;
mod engine;
mod http;
pub mod prices;
mod retry;
pub mod runtime;
mod sse;

pub use anthropic::AnthropicClient;
pub use engine::Timeouts;

omuzaki_core::define_link!();
