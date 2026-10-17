//! Writes the list of built-in recipes (`src/recipes/`): one `mod` per file, so a recipe is added by adding a file and no
//! list in code names it (spec v3 §39; ADR 0054 §1). `src/recipes/mod.rs` includes the result.

use std::{env, path::PathBuf, process};

fn main() {
    let Some(out) = env::var_os("OUT_DIR").map(PathBuf::from) else { fail("OUT_DIR isn't set") };
    if let Err(error) = omuzaki_embed::modules("src/recipes", out.join("recipes_mods.rs"), "") {
        fail(&format!("can't generate the recipe list from src/recipes: {error}"));
    }
}

/// Reports `message` as a build error and stops the build.
fn fail(message: &str) -> ! {
    println!("cargo::error={message}");
    process::exit(1);
}
