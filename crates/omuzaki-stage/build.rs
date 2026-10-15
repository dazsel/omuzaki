//! Declares the crate root's feature modules and the ops folder's op modules from their folders (ADR 0054 §1), so a
//! feature is a new file and no `mod` list is edited. The lists are written to `OUT_DIR` and `include!`d by `lib.rs`
//! and `ops/mod.rs`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    let src = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("src");
    omuzaki_embed::modules(&src, out.join("crate_mods.rs"), "pub")?;
    omuzaki_embed::modules(src.join("ops"), out.join("ops_mods.rs"), "pub")?;
    Ok(())
}
