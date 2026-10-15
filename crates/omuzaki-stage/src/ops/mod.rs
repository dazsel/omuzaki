//! The ops the stage registers (spec v3 §6): `render.*` over a staged character and `sheet.*` over its sheets. Ops
//! register themselves next to their definition, so nothing here lists them.

// The folder's op modules, declared from the folder by the build script (ADR 0054 §1).
include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
