//! Writes the list of io op modules (`src/ops/`): one `pub mod` per file and per sub-folder that holds a `mod.rs`, so an
//! op is added by adding a file and no list in code names it (spec v3 §39 rules 2 and 5; ADR 0054). `src/ops/mod.rs`
//! includes the result.

use std::{env, path::PathBuf, process};

fn main() {
    let out = match env::var_os("OUT_DIR") {
        Some(out) => PathBuf::from(out),
        None => fail("OUT_DIR isn't set"),
    };
    if let Err(error) = omuzaki_embed::modules("src/ops", out.join("ops_mods.rs"), "pub") {
        fail(&format!("can't generate the op modules from src/ops: {error}"));
    }
}

/// Reports `message` as a build error and stops the build.
fn fail(message: &str) -> ! {
    println!("cargo::error={message}");
    process::exit(1);
}
