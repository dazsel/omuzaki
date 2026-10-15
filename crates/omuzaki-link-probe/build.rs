//! Writes this App's `link()` calls from the `registers` flags of its dependencies (ADR 0054 §6).
//!
//! With the `unlinked` feature it writes an empty list instead, as the generator would if `omuzaki-char` lost its
//! `registers` flag: the negative case of `cargo xtask wasm-link-proof`, which must then find no registration.

use std::path::PathBuf;
use std::{env, fs};

#[allow(clippy::disallowed_methods, reason = "a build script writes its generated file at build time")]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    if env::var_os("CARGO_FEATURE_UNLINKED").is_none() {
        omuzaki_link_build::emit();
        return Ok(());
    }
    let out = PathBuf::from(env::var("OUT_DIR")?).join(omuzaki_link_build::OUT_FILE);
    // The block the generator writes for an App with no flagged dependency.
    fs::write(out, "{}\n")?;
    Ok(())
}
