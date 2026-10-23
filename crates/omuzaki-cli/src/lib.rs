//! The `omuzaki` command (spec v3 §6, "one registry, five surfaces"): the CLI surface of the op registry.
//!
//! The CLI knows no op. `omuzaki ops` lists whatever [`Runner::ops`](omuzaki_ops_api::Runner::ops) returns,
//! `omuzaki help <op>` documents an op from its JSON Schema ([`schema_doc`]), and `omuzaki run <op> --args <json>` sends
//! the arguments to [`Runner::run`](omuzaki_ops_api::Runner::run). A new op appears here by registering itself, with no
//! change to this crate.
//!
//! The flow, from `main` ([`app::run`]): [`cli::parse`] → [`session::open_runner`] → [`commands::execute`] (a function
//! from a runner to the text to print) → [`output::finish`] (prints it and picks the [`exit`] code).
//!
//! # What runs behind it
//!
//! A command on a project opens it through the app core ([`omuzaki_app::AppCore`], via [`app::run`]), so a `run` is
//! planned, permission-checked, applied and logged exactly as an agent's call over MCP is, and kept:
//!
//! - **Journalled as you go.** Each applied change (and each undo and redo) is in the project's journal before the
//!   command answers, and the files are written before it exits. A dry run writes nothing.
//! - **History is kept with the project.** `omuzaki undo` in a new process undoes what the last one did, and the files
//!   follow. A crash is recovered on the next open (`--recover` says how), and the notice goes to stderr.
//! - **`omuzaki mcp`** serves the same runner to an AI agent on stdin and stdout ([`mcp`]).
//! - **No project** is fine for `ops` and `help`, which only read the registry; every other command needs `--project`.
//!
//! Exit codes are documented in `omuzaki --help` ([`exit`]); an unknown op name is `invalid` (exit 3, field `op`) from
//! `run` because the executor says so, and `not_found` (exit 4) from `help`.
#![deny(missing_docs)]

pub mod app;
pub mod args;
pub mod branch;
pub mod cli;
pub mod commands;
pub mod credential;
pub mod exit;
pub mod json;
pub mod mcp;
pub mod output;
pub mod project;
pub mod render;
pub mod schema_doc;
pub mod session;
pub mod store;

// `omuzaki` ships the product's ops, importers and exporters: the stage (which links the character, animation and face
// crates) and the I/O crates. `main` calls this at startup (ADR 0015). `build.rs` writes the calls: one for every
// dependency that sets `registers = true`, so adding a crate here is one dependency line (ADR 0054 section 6).
// Any binary that links this library carries the crates, whether or not it calls `link()`.
omuzaki_core::define_link! {
    include!(concat!(env!("OUT_DIR"), "/link_calls.rs"));
}
