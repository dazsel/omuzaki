//! The workspace-scan tests that run on every change, whatever crates it touches.
//!
//! Some integration tests read the whole workspace rather than their own crate: `omuzaki-ops-api`'s `fs_fence` checks
//! every crate's `clippy.toml`, and the Apps' link gates read every crate's manifest and sources. A change in one
//! crate can break them without touching their crate, so the affected-crate scope would never run them. This module
//! lists those test targets explicitly. `check` runs them, and `Report::scans` carries the list for the CI plan to read
//! (the CI wiring is a separate change), and
//! [`unlisted`] fails when a new test reads the workspace without being listed here or exempted with a reason.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::workspace::Workspace;

/// One integration test target that scans the workspace: `cargo test -p <krate> --test <test>`.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub struct Scan {
    /// The package that owns the test target.
    pub krate: &'static str,
    /// The test target's name (`tests/<test>.rs`).
    pub test: &'static str,
}

/// Every workspace-scanning test target. Together they run in well under two minutes once built; keep it that way by
/// listing targets, never whole crates.
pub const SCANS: [Scan; 4] = [
    // Every crate that registers ops carries the file fence in its own `clippy.toml`.
    Scan { krate: "omuzaki-ops-api", test: "fs_fence" },
    // The link gates compare an App's registrations with those of every crate it links (ADR 0015).
    Scan { krate: "omuzaki-cli", test: "registrations" },
    Scan { krate: "omuzaki-stage", test: "registrations" },
    // The eval lint runs over the real workspace's task files.
    Scan { krate: "omuzaki-test", test: "eval_lint" },
];

/// Test targets that reach for the workspace root but do not scan it, each with the reason. `lint_seed` is a library
/// module run in process by `check`, so it is not a test target at all.
const NOT_SCANS: [(&str, &str, &str); 3] = [
    ("omuzaki-link-build", "command_line_feature", "copies one file (Cargo.lock) into a throwaway project"),
    ("omuzaki-settings-api", "link_wiring", "copies one file (Cargo.lock) into a throwaway workspace it builds itself"),
    ("omuzaki-stage", "sheet_layout", "reads its own font licence files, not the workspace"),
];

/// What marks a test source as walking the workspace: a helper that lists the crates, or a path built from the crate's
/// own directory going up to the root. `include_str!("../../..")` of one doc is not that, and does not match.
fn reads_workspace(text: &str) -> bool {
    text.contains("crates_dir(")
        || text.contains("read_dir(\"crates\"")
        || (text.contains("CARGO_MANIFEST_DIR") && (text.contains("../..") || text.contains(".parent()")))
}

impl Scan {
    /// The cargo arguments that run this target.
    pub fn cargo_args(&self) -> Vec<String> {
        ["test", "-p", self.krate, "--test", self.test].map(str::to_owned).to_vec()
    }

    /// The target's source: `tests/<test>.rs`, or `tests/<test>/main.rs` for a directory target.
    fn path(&self, root: &Path) -> PathBuf {
        let tests = root.join("crates").join(self.krate).join("tests");
        let file = tests.join(format!("{}.rs", self.test));
        if file.is_file() { file } else { tests.join(self.test).join("main.rs") }
    }
}

/// Problems with the list: a listed target whose file is gone, and a test that reads the workspace without being
/// listed or exempted. Each is one line naming the file.
pub fn problems(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = SCANS
        .iter()
        .filter(|scan| !scan.path(root).is_file())
        .map(|scan| format!("{}: listed in scans.rs but the file does not exist", display(root, &scan.path(root))))
        .collect();
    let Ok(crates) = fs::read_dir(root.join("crates")) else { return found };
    // (crate, test target, its source files): `tests/<test>.rs`, or every `.rs` file in `tests/<test>/`.
    let mut tests: Vec<(String, String, Vec<PathBuf>)> = Vec::new();
    for entry in crates.flatten() {
        let krate = entry.file_name().to_string_lossy().into_owned();
        let Ok(files) = fs::read_dir(entry.path().join("tests")) else { continue };
        for path in files.flatten().map(|file| file.path()) {
            let Some(test) = path.file_stem().map(|name| name.to_string_lossy().into_owned()) else { continue };
            if path.extension().is_some_and(|e| e == "rs") {
                tests.push((krate.clone(), test, vec![path]));
            } else if path.join("main.rs").is_file() {
                let mut sources: Vec<PathBuf> =
                    fs::read_dir(&path).map(|dir| dir.flatten().map(|file| file.path()).collect()).unwrap_or_default();
                sources.retain(|source| source.extension().is_some_and(|e| e == "rs"));
                tests.push((krate.clone(), test, sources));
            }
        }
    }
    tests.sort();
    for (krate, test, sources) in tests {
        let listed = SCANS.iter().any(|scan| scan.krate == krate && scan.test == test)
            || NOT_SCANS.iter().any(|(k, t, _)| *k == krate && *t == test);
        if listed {
            continue;
        }
        for path in sources {
            match fs::read_to_string(&path) {
                Ok(text) if reads_workspace(&text) => {
                    found.push(format!(
                        "{}: reads the workspace; add it to SCANS (or NOT_SCANS with a reason) in xtask/src/scans.rs",
                        display(root, &path)
                    ));
                    break;
                }
                Ok(_) => {}
                Err(error) => found.push(format!("{}: cannot be read: {error}", display(root, &path))),
            }
        }
    }
    found
}

fn display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root).unwrap_or(path).display().to_string().replace('\\', "/")
}

/// The `lint-scans` step: prints each problem; true when there is none.
pub fn report(workspace: &Workspace) -> bool {
    let found = problems(&workspace.root);
    for line in &found {
        eprintln!("lint-scans: {line}");
    }
    found.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
    }

    #[test]
    fn the_real_workspace_lists_every_scan_test() {
        assert_eq!(problems(&root()), Vec::<String>::new());
    }

    #[test]
    fn an_unlisted_test_that_reads_the_workspace_is_found() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let tests = dir.path().join("crates/omuzaki-new/tests");
        fs::create_dir_all(&tests).unwrap_or_else(|error| panic!("{error}"));
        fs::write(tests.join("scan.rs"), "fn root() { crates_dir(); }\n").unwrap_or_else(|error| panic!("{error}"));
        fs::write(tests.join("own.rs"), "fn own() {}\n").unwrap_or_else(|error| panic!("{error}"));
        let found = problems(dir.path());
        assert!(found.iter().any(|line| line.starts_with("crates/omuzaki-new/tests/scan.rs: reads the workspace")));
        assert!(!found.iter().any(|line| line.contains("own.rs")), "{found:?}");
        // Every listed target is also missing from this throwaway tree.
        assert_eq!(found.iter().filter(|line| line.contains("does not exist")).count(), SCANS.len());
    }

    #[test]
    fn a_directory_target_and_an_unreadable_source_are_found() {
        let dir = tempfile::tempdir().unwrap_or_else(|error| panic!("{error}"));
        let target = dir.path().join("crates/omuzaki-new/tests/walk");
        fs::create_dir_all(&target).unwrap_or_else(|error| panic!("{error}"));
        fs::write(target.join("main.rs"), "mod helper;\n").unwrap_or_else(|error| panic!("{error}"));
        fs::write(target.join("helper.rs"), "fn root() { crates_dir(); }\n").unwrap_or_else(|error| panic!("{error}"));
        let bad = dir.path().join("crates/omuzaki-new/tests/bad.rs");
        fs::write(bad, [0xff, 0xfe]).unwrap_or_else(|error| panic!("{error}"));
        let found = problems(dir.path());
        assert!(
            found.iter().any(|line| line.starts_with("crates/omuzaki-new/tests/walk/helper.rs: reads")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|line| line.starts_with("crates/omuzaki-new/tests/bad.rs: cannot be read")),
            "{found:?}"
        );
    }
}
