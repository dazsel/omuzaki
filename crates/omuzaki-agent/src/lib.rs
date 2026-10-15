//! The in-app assistant's host (AG-01, ADR 0040, `docs/design/agent-runtime.md`).
//!
//! The crate depends on API crates and rmcp's client only. It never links `omuzaki-mcp`, `omuzaki-ops` or
//! `omuzaki-llm`: the app hands it an in-process MCP link, an `Arc<dyn LlmClient>` and an `Arc<dyn Sessions>`.
#![deny(missing_docs)]

pub mod sink;
pub mod store;
