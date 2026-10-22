//! The app core: one in-process core per open project, shared by the studio, every MCP client and the CLI, so that a
//! person at a panel and an agent over MCP work on the same character, see each other's changes at once and undo their
//! own.
//!
//! [`AppCore::open`] does, in order:
//!
//! 1. **Takes the folder's lock** (`.omuzaki/lock`): one writer per project. In the studio it then writes the session's
//!    crash-loop guard ([`AppOptions::remember`]), so a load that kills the process is not retried at every start;
//!    [`startup::plan`] reads it, the journals and `startup.reopen` to decide what the next start does.
//! 2. **Reads the journal** ([`omuzaki_doc::journal::reopen`]): after a clean close the files are the truth; after a
//!    crash, the newest snapshot plus the journal records after it.
//! 3. **Builds the executor** with the history restored (every actor's undo and redo stacks, the next transaction and
//!    op ids) and replays the records a crash left, then closes every transaction left open: rolled back, or committed
//!    when later work depends on it. What it did is in [`AppCore::recovery`].
//! 4. **Starts the session's journal** ([`ProjectJournal`]): its base snapshot, then every change the executor makes is
//!    durable within milliseconds and the files follow within two seconds.
//! 5. **Builds the MCP server** over that executor ([`AppCore::mcp`]): every client is a registered session of the same
//!    executor, its changes reach the event stream the UI reads, and the person's changes reach its heads-up. A host
//!    opens the loopback HTTP port with [`AppCore::serve_http`] and the built-in assistant's link with
//!    [`AppCore::connect_in_app`]; `omuzaki mcp` serves it on stdio.
//!
//! While the HTTP port is open, the core publishes how to reach it ([`studio`]), so `omuzaki mcp` started by a coding
//! agent bridges into this core ([`bridge`]) instead of opening the project a second time; the person pairs each new
//! client once, on a card.
//!
//! [`AppCore::close`] ends the session cleanly: the HTTP port closes (its sessions end), jobs are cancelled, and the
//! journal writes the files, a close snapshot carrying the history, and its `close` record. Dropping the core closes
//! it too; [`AppCore::abandon`] stops without closing, as a crash would, so the next open recovers.
#![deny(missing_docs)]

pub mod branches;
pub mod bridge;
mod core;
mod error;
pub mod history;
pub mod ops;
mod recover;
pub mod startup;
pub mod studio;

pub use core::{AppCore, AppOptions, FailpointChoice, JournalFsChoice, with_resource_providers};
pub use error::AppError;
pub use omuzaki_doc::journal::Recover;
pub use recover::RecoveryReport;

// The MCP server registers ops (`agent.connection`), the library contract and the settings store register their own
// (the settings and session ops, `startup.reopen`), and so does first-run setup (the `setup.*` ops), so an App that
// links the core links them through this (ADR 0015).
omuzaki_core::define_link! {
    omuzaki_mcp::link();
    omuzaki_library_api::link();
    omuzaki_settings::link();
    omuzaki_setup::link();
}
