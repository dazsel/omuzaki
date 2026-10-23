//! The `library.*` ops (design D9). Each op lives in its own file and registers itself, so nothing here lists them.

// The folder's op modules, declared from the folder by the build script (ADR 0054 section 1).
include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
