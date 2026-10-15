//! Declares the crate root's feature modules from their folders (ADR 0054 §1), so a feature is a new file or folder
//! and no `mod` list is edited. The list is written to `OUT_DIR` and `include!`d by `lib.rs`.

use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    let src = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?).join("src");
    omuzaki_embed::modules(&src, out.join("crate_mods.rs"), "pub")?;
    Ok(())
}
