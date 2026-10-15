//! Writes this App's `link()` calls from the `registers` flags of its dependencies (ADR 0054 §6).
//!
//! With the `unlinked` feature it then replaces them with the empty list the generator writes for an App with no
//! flagged dependency, as it would if `omuzaki-char` lost its `registers` flag: the negative case of
//! `cargo xtask wasm-link-proof`, which must then find no registration.

use std::path::PathBuf;
use std::{env, fs};

// `main` carries no attribute: `lint-links` only reads an unattributed `main` as running the generator.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    omuzaki_link_build::emit();
    if env::var_os("CARGO_FEATURE_UNLINKED").is_some() {
        unlink()?;
    }
    Ok(())
}

/// Replaces the generated calls with the generator's empty block.
#[allow(clippy::disallowed_methods, reason = "a build script writes its generated file at build time")]
fn unlink() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(env::var("OUT_DIR")?).join(omuzaki_link_build::OUT_FILE);
    fs::write(out, "{}\n")?;
    Ok(())
}
