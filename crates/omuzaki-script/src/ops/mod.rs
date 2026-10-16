//! The `script.*` ops: check, run, save, delete and list. Each op lives in its own file and registers itself, so nothing
//! here lists them.
//!
//! These are the ops the Script console, the Macros panel, the `omuzaki script` command and an agent's `script_run`
//! call; their names, arguments and answers are their contract.

// The folder's op modules, declared from the folder by the build script (ADR 0054 section 1).
include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
