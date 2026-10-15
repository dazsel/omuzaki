//! The in-app assistant's host (AG-01, ADR 0040, `docs/design/agent-runtime.md`).
//!
//! The crate depends on API crates and rmcp's client only. It never links `omuzaki-mcp`, `omuzaki-ops` or
//! `omuzaki-llm`: the app hands it an in-process MCP link, an `Arc<dyn LlmClient>` and an `Arc<dyn Sessions>`.
#![deny(missing_docs)]

omuzaki_core::define_link!();

mod calls;
mod caps;
mod config;
mod content;
mod context;
mod fold;
mod host;
mod prompts;
mod refusal;
mod resume;
mod run;
mod shared;
mod stop;
mod tools;
mod turn;
mod tx;
mod worker;
mod link;
pub mod settings;
pub mod sink;
pub mod store;
mod wait;

pub use config::{Caps, HostConfig, Language};
pub use host::AssistantHost;
pub use link::McpClientLink;
