//! Scripted stand-ins for the in-app assistant (AG-01, ADR 0040, `docs/design/agent-runtime.md` D1, D10 and §7).
//!
//! - **[`ScriptedAssistant`]** implements `Assistant`: the n-th `send` plays the n-th [`ScriptedAnswer`], event by
//!   event, as the `Clock` passes each [`TimedEvent::at`]. Nothing plays unless the test calls
//!   [`tick`](ScriptedAssistant::tick): the mock starts no thread. It records what the panel sent.
//! - **[`ScriptedConversations`]** implements `Conversations` over an in-memory list.
//!
//! Both build and test without an LLM, an MCP server or an executor.
#![deny(missing_docs)]

mod assistant;
mod conversations;

pub use assistant::{ScriptedAnswer, ScriptedAssistant, TimedEvent};
pub use conversations::ScriptedConversations;
