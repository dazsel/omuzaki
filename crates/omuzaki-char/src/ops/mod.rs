//! The ops this crate registers (spec §6), one submodule per op family.

include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
