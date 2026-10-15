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
//! [`session::open_runner`] opens the `--project` folder with `omuzaki-doc` and builds the `omuzaki-ops` executor over it,
//! so a `run` is planned, permission-checked, applied and logged exactly as an agent's call over MCP is, and saved:
//!
//! - **Saved as you go.** Each applied change (and each undo and redo) is written to the project's files before the
//!   command returns ([`project::SavedProject`]). A dry run writes nothing.
//! - **History is per process.** The executor keeps the op log and each actor's undo and redo stacks in memory, and
//!   nothing persists them yet, so `omuzaki undo` in a fresh process has nothing to undo. Inside one
//!   process (an MCP session, where agents have `history_undo` and `history_redo`) undo and redo work and the files
//!   follow.
//! - **`omuzaki mcp`** serves the same runner to an AI agent on stdin and stdout ([`mcp`]).
//! - **No project** is fine for `ops` and `help`, which only read the registry; every other command needs `--project`.
//!
//! Exit codes are documented in `omuzaki --help` ([`exit`]); an unknown op name is `invalid` (exit 3, field `op`) from
//! `run` because the executor says so, and `not_found` (exit 4) from `help`.
#![deny(missing_docs)]

pub mod app;
pub mod args;
pub mod cli;
pub mod commands;
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
// crates) and the I/O crates. `main` calls this at startup (ADR 0015); `cargo xtask lint-links` checks the list.
// Any binary that links this library carries the crates, whether or not it calls `link()`.
omuzaki_core::define_link! {
    omuzaki_stage::link();
    omuzaki_io_ops::link();
    omuzaki_io_gltf::link();
    omuzaki_io_vrm::link();
}
