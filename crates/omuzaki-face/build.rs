//! Writes the list of face op families (`src/ops/`): one `pub mod` per sub-folder that holds a `mod.rs`, so a family is
//! added by adding a folder and no list in code names it (spec v3 §39 rules 2 and 5; ADR 0054). `src/ops/mod.rs`
//! includes the result.

use std::{env, path::PathBuf, process};

fn main() {
    let out = match env::var_os("OUT_DIR") {
        Some(out) => PathBuf::from(out),
        None => fail("OUT_DIR isn't set"),
    };
    if let Err(error) = omuzaki_embed::modules("src/ops", out.join("ops_mods.rs"), "pub") {
        fail(&format!("can't generate the op families from src/ops: {error}"));
    }
}

/// Reports `message` as a build error and stops the build.
fn fail(message: &str) -> ! {
    println!("cargo::error={message}");
    process::exit(1);
}
