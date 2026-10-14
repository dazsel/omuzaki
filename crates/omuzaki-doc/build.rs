//! Generates the list of migration modules from the files in `src/migrations/`, so a migration is added by adding one
//! file and no shared list is edited (spec v3 §39 rules 2 and 5).

#[path = "build_support/migration_files.rs"]
mod migration_files;

use std::path::PathBuf;
use std::{env, fs, process};

fn main() {
    println!("cargo::rerun-if-changed=src/migrations");
    println!("cargo::rerun-if-changed=build_support");
    if let Err(message) = generate() {
        println!("cargo::error={message}");
        process::exit(1);
    }
}

fn generate() -> Result<(), String> {
    let manifest = env::var_os("CARGO_MANIFEST_DIR").ok_or("CARGO_MANIFEST_DIR isn't set")?;
    let out = env::var_os("OUT_DIR").ok_or("OUT_DIR isn't set")?;
    let decls = migration_files::module_decls(&PathBuf::from(manifest).join("src").join("migrations"))?;
    fs::write(PathBuf::from(out).join("migrations.rs"), decls).map_err(|e| format!("can't write migrations.rs: {e}"))
}
