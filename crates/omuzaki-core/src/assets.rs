//! Where heavy assets (models, goldens, datasets) live: outside git, in one folder every test and tool agrees on.
//!
//! The folder is `$OMUZAKI_ASSETS`, or `assets-local` in the workspace root when that is unset or empty. Tests that need
//! files under it check [`require`] and skip, saying why, when the files are absent, so a fresh checkout stays green.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable naming the assets folder.
pub const ASSETS_ENV: &str = "OMUZAKI_ASSETS";

/// The folder used when [`ASSETS_ENV`] is unset, relative to the workspace root.
pub const DEFAULT_ASSETS_DIR: &str = "assets-local";

/// The assets folder for the current process: `$OMUZAKI_ASSETS`, else `<workspace root>/assets-local`.
///
/// The workspace root is the nearest folder at or above the current directory whose `Cargo.toml` has a `[workspace]`
/// table (tests run with their crate's folder as the working directory). Outside any workspace the default is
/// relative to the current directory.
#[must_use]
pub fn assets_root() -> PathBuf {
    let start = std::env::current_dir().unwrap_or_default();
    assets_root_with(std::env::var_os(ASSETS_ENV), &start)
}

/// [`assets_root`] with the variable's value and the starting directory passed in, so tests never touch the real
/// environment.
#[must_use]
pub fn assets_root_with(env: Option<OsString>, start: &Path) -> PathBuf {
    if let Some(dir) = env.filter(|v| !v.is_empty()) {
        return PathBuf::from(dir);
    }
    let workspace = start.ancestors().find(|dir| declares_workspace(&dir.join("Cargo.toml")));
    workspace.unwrap_or(Path::new("")).join(DEFAULT_ASSETS_DIR)
}

fn declares_workspace(manifest: &Path) -> bool {
    std::fs::read_to_string(manifest).is_ok_and(|text| text.lines().any(|line| line.trim() == "[workspace]"))
}

/// `path` if it exists; otherwise prints that the test is skipping, and why, and returns `None`.
#[must_use]
pub fn require(path: &Path) -> Option<&Path> {
    if path.exists() {
        Some(path)
    } else {
        eprintln!("skipped: {} not found (set {ASSETS_ENV} to your assets folder)", path.display());
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_variable_wins_and_empty_counts_as_unset() {
        let here = Path::new("/nowhere");
        assert_eq!(assets_root_with(Some("/data/a".into()), here), Path::new("/data/a"));
        assert_eq!(assets_root_with(Some(OsString::new()), here), Path::new("assets-local"));
        assert_eq!(assets_root_with(None, here), Path::new("assets-local"));
    }

    #[test]
    fn the_default_sits_in_the_workspace_root() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let root = assets_root_with(None, dir);
        assert_eq!(root.file_name().and_then(|n| n.to_str()), Some("assets-local"));
        assert!(root.parent().is_some_and(|p| p.join("crates").is_dir()), "{root:?}");
    }
}
