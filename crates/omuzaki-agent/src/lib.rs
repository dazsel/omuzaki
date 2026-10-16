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
mod cut;
mod fold;
mod history;
mod host;
mod link;
mod prompts;
mod refusal;
mod resume;
mod run;
pub mod settings;
mod shared;
pub mod sink;
mod stop;
pub mod store;
mod tools;
mod turn;
mod tx;
mod wait;
mod worker;

pub use config::{Caps, HostConfig, Language};
pub use host::AssistantHost;
pub use link::McpClientLink;
