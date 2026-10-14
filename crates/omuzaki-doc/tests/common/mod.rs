//! Helpers shared by the integration tests.
// Each test binary uses a different subset of these, and a helper that fails should just panic.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;

use omuzaki_doc_api::{DocPath, Patch};
use serde_json::Value;

/// A document path.
pub fn p(s: &str) -> DocPath {
    DocPath::parse(s).unwrap()
}

/// Writes a file under `dir`, making folders as needed.
pub fn write(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// Reads a file under `dir`.
pub fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// True if `rel` exists under `dir`.
pub fn exists(dir: &Path, rel: &str) -> bool {
    dir.join(rel).exists()
}

/// A patch with one change.
pub fn change(path: &str, before: Option<Value>, after: Option<Value>) -> Patch {
    Patch::new().with(p(path), before, after)
}

/// Every file under `dir` (relative paths, sorted) with its bytes, except the store's own bookkeeping in `.omuzaki/`
/// (the files a save replaced are kept there).
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    snapshot_except(dir, &[])
}

/// Like [`snapshot`], but never enters the top-level folders named in `skip`: `.git`, for one, is git's to change
/// while a test looks.
pub fn snapshot_except(dir: &Path, skip: &[&str]) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, skip: &[&str], out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.strip_prefix(base).is_ok_and(|rel| rel == Path::new(".omuzaki")) {
                continue;
            }
            if path.is_dir() {
                let skipped = dir == base && path.file_name().is_some_and(|name| skip.iter().any(|s| name == *s));
                if !skipped {
                    walk(&path, base, skip, out);
                }
            } else {
                let rel = path.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, skip, &mut out);
    out.sort();
    out
}

/// Writes a project of `files` realistic files (about 1.5 KB each: comments, nested tables, lists) into `dir`:
/// characters with the spec's `character`, `face`, `rig`, `look` files and variants, then scenes to fill up.
pub fn write_big_project(dir: &Path, files: usize) {
    write(dir, "project.toml", "# a show\nname = \"Big\"\nschema = 1\n");
    let mut written = 1;
    let mut n = 0;
    while written < files {
        let id = format!("char{n:03}");
        for name in ["character", "face", "rig", "look", "variants/game", "variants/closeup"] {
            if written == files {
                break;
            }
            write(dir, &format!("characters/{id}/{name}.toml"), &realistic_file(n));
            written += 1;
        }
        n += 1;
    }
}

fn realistic_file(seed: usize) -> String {
    let mut parts = vec![
        format!("# generated file {seed}\nprofile = \"oval-long\"  # face shape\nblend = 0.{}\n", seed % 10),
        "tags = [\"a\", \"b\", \"c\"]\n".to_owned(),
    ];
    for section in ["eyes", "brows", "mouth", "blush"] {
        parts.push(format!("\n# {section}\n[{section}]\nsize = {seed}\nspacing = 0.25\nanchor = {{ x = 1, y = 2 }}\n"));
        for i in 0..4 {
            parts.push(format!(
                "\n[{section}.part{i}]\nweight = {i}.5\nname = \"{section}-{i}\"\nvalues = [1, 2, 3, 4, 5, 6]\n"
            ));
        }
    }
    parts.concat()
}

/// Runs `f` with `dir` read-only, so that creating or deleting anything inside it fails the way a crash or a full
/// disk would, then restores the folder.
#[cfg(unix)]
pub fn with_readonly<T>(dir: &Path, f: impl FnOnce() -> T) -> T {
    use std::os::unix::fs::PermissionsExt;
    let mode = fs::metadata(dir).unwrap().permissions().mode();
    fs::set_permissions(dir, fs::Permissions::from_mode(0o555)).unwrap();
    let out = f();
    fs::set_permissions(dir, fs::Permissions::from_mode(mode)).unwrap();
    out
}

/// False when read-only folders don't stop writes (running as root), so tests that rely on a failing write skip.
#[cfg(unix)]
pub fn readonly_is_enforced() -> bool {
    let tmp = tempfile::TempDir::new().unwrap();
    with_readonly(tmp.path(), || fs::write(tmp.path().join("probe"), "x").is_err())
}
