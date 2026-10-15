//! The `io.*` ops and the `licence.*` ops that fix what `io.check_export` reports. Each op lives in its own file and
//! registers itself, so nothing here lists them.

// The folder's op modules, declared from the folder by the build script (ADR 0054 §1). The shared error mapping sits
// beside this folder, in `error`, because the generated list makes every module here `pub`.
include!(concat!(env!("OUT_DIR"), "/ops_mods.rs"));
