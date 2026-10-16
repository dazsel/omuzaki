//! Finds the migration files in `src/migrations/` and writes the `mod` lines that compile them.
//!
//! Shared by `build.rs`, which uses it, and the crate's tests, which check it (both `#[path]` this one file).

use std::fmt::Write as _;
use std::path::Path;

/// The `mod` declarations for every `m<NNNN>_<what>.rs` in `dir`, oldest first.
///
/// `mod.rs` and files that aren't Rust are skipped.
///
/// # Errors
/// A Rust file that doesn't match the naming pattern, or two files that produce the same schema number.
pub fn module_decls(dir: &Path) -> Result<String, String> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("can't read {}: {e}", dir.display()))?;
    let mut found: Vec<(u32, String)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("can't read {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(stem) = name.strip_suffix(".rs").filter(|s| *s != "mod") else { continue };
        let number = schema_number(stem).ok_or_else(|| {
            format!(
                "src/migrations/{name} must be named m<NNNN>_<what>.rs: four digits, then lowercase words joined by '_'"
            )
        })?;
        if let Some((_, other)) = found.iter().find(|(n, _)| *n == number) {
            return Err(format!("src/migrations/{name} and {other}.rs both produce schema {number}"));
        }
        found.push((number, stem.to_owned()));
    }
    found.sort();
    let mut decls = String::new();
    for (_, stem) in found {
        // An absolute path: this text is `include!`d from another file, where relative paths would mean something else.
        let path = dir.join(format!("{stem}.rs"));
        let literal = format!("{:?}", path.to_string_lossy()); // a Rust string literal, escaped
        writeln!(decls, "#[path = {literal}]\nmod {stem};").map_err(|e| e.to_string())?;
    }
    Ok(decls)
}

/// The `NNNN` of `m<NNNN>_<what>`, if `stem` has that shape.
fn schema_number(stem: &str) -> Option<u32> {
    let (digits, what) = stem.strip_prefix('m')?.split_once('_')?;
    let well_formed = digits.len() == 4
        && digits.bytes().all(|b| b.is_ascii_digit())
        && !what.is_empty()
        && what.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if well_formed { digits.parse().ok() } else { None }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    fn dir_with(files: &[&str]) -> TempDir {
        let dir = TempDir::new().unwrap();
        for f in files {
            fs::write(dir.path().join(f), "").unwrap();
        }
        dir
    }

    #[test]
    fn files_become_modules_in_schema_order() {
        let dir = dir_with(&["mod.rs", "m0003_rename_things.rs", "m0002_add_field.rs", "README.md"]);
        let decls = module_decls(dir.path()).unwrap();
        let mods: Vec<&str> = decls.lines().filter(|l| l.starts_with("mod ")).collect();
        assert_eq!(mods, ["mod m0002_add_field;", "mod m0003_rename_things;"]);
        // The path is written as an escaped Rust literal (`{:?}`), so on Windows its backslashes are doubled (#1431).
        let escaped = format!("{:?}", dir.path().to_string_lossy());
        assert!(decls.contains(escaped.trim_matches('"')));
    }

    #[test]
    fn badly_named_and_duplicate_files_are_errors_naming_the_file() {
        for bad in ["m2_x.rs", "m0002.rs", "m0002_.rs", "x0002_a.rs", "m0002_Add.rs", "m00a2_x.rs", "m00002_x.rs"] {
            let err = module_decls(dir_with(&[bad]).path()).unwrap_err();
            assert!(err.contains(bad), "{err}");
        }
        let err = module_decls(dir_with(&["m0002_a.rs", "m0002_b.rs"]).path()).unwrap_err();
        assert!(err.contains("both produce schema 2"), "{err}");
    }

    #[test]
    fn no_migrations_is_fine() {
        assert_eq!(module_decls(dir_with(&["mod.rs"]).path()).unwrap(), "");
    }
}
