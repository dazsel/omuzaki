//! The in-app assistant's contract (AG-01, ADR 0040, `docs/design/agent-runtime.md` D1, D4, D7, D10 and §7).
//!
//! The chat panel depends on this crate alone, so it builds and tests without an LLM, an MCP server or an executor:
//! `omuzaki-agent` implements [`Assistant`] for real and `omuzaki-agent-mock` plays a script.
//!
//! Callers import by module path (ADR 0054 §2): `omuzaki_agent_api::event::AssistantEvent`.
//!
//! - **[`Assistant`]** (here): what the panel calls. Approval cards resolve through `Sessions::resolve` as the person,
//!   never through this trait.
//! - **[`event`]**: [`event::AssistantEvent`], the caps and the plan.
//! - **[`hub`]**: [`hub::EventHub`], the single-consumer queue behind [`Assistant::events`]. Its `stop` is what makes
//!   the guarantee of D4: after Stop returns, the panel reads no new output.
//! - **[`message`]**: what the person sends ([`message::UserMessage`], [`message::ContextChip`]).
//! - **[`entry`]**: the kinds a conversation is stored as (D7).
//! - **[`conversations`]**: [`conversations::Conversations`] (list, open, delete, export) and
//!   [`conversations::RationaleSource`] ("Explain this", TH-08).
//! - **[`prompts`]**: the `/` menu's types (§7).
//! - **[`error`]**: [`error::HostError`].
//!
//! The crate has no async runtime and starts no thread.
#![deny(missing_docs)]

pub mod conversations;
pub mod entry;
pub mod error;
pub mod event;
pub mod hub;
pub mod message;
pub mod prompts;

use std::collections::BTreeMap;

use entry::Entry;
use error::HostError;
use event::QuestionId;
use message::UserMessage;
use prompts::PromptsState;

/// The assistant as the chat panel sees it. Every method returns at once: none waits on the model, the server or the
/// disk, except [`prompt`](Assistant::prompt), which is one bounded round trip.
pub trait Assistant: Send + Sync {
    /// Sends the person's message and starts an answer. The panel disables its box until [`event::AssistantEvent::Done`].
    ///
    /// # Errors
    /// [`HostError::Busy`] while an answer runs; [`HostError::SessionEnded`] after a revoke.
    fn send(&self, msg: UserMessage) -> Result<(), HostError>;

    /// Stops the answer: the always-visible Stop button and `Esc` (D4). After it returns, [`hub::EventReceiver::try_recv`]
    /// yields no new output; `Paused` and `Done` follow. A second call while stopping changes nothing.
    fn stop(&self);

    /// Answers an open [`event::AssistantEvent::Question`] (the `ask_person` tool, D9).
    ///
    /// # Errors
    /// [`HostError::Mcp`] if no such question is open (a stale card).
    fn answer(&self, q: QuestionId, text: String) -> Result<(), HostError>;

    /// The one consumer of the host's events. A later call detaches the earlier receiver, which then yields nothing.
    fn events(&self) -> hub::EventReceiver;

    /// The conversation so far, for a panel opened late.
    fn transcript(&self) -> Vec<Entry>;

    /// The `/` menu's prompts: the host's cached list, sorted by name, or why there is none. Never blocks (§7).
    fn prompts(&self) -> PromptsState;

    /// Marks the prompt list stale and refetches it on the host thread; [`prompts`](Assistant::prompts) reads `Loading`
    /// at once. Never blocks (§7).
    fn refresh_prompts(&self);

    /// The text of the prompt `name` filled with `args`, to put in the message box. One round trip of at most 2 s. It
    /// opens no transaction, charges nothing and sends nothing to the model (§7).
    ///
    /// # Errors
    /// [`HostError::SessionEnded`] after a revoke or close; [`HostError::Mcp`] for the server's own refusal, a text over
    /// 16 KiB, or a timeout.
    fn prompt(&self, name: &str, args: BTreeMap<String, String>) -> Result<String, HostError>;
}
