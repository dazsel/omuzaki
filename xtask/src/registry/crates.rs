//! The workspace's crates as the gate needs them, read from the manifests and `src/` trees on disk.
//!
//! Not `cargo metadata`: the gate also runs inside test binaries, including Windows ones started through WSL interop,
//! where there is no cargo to ask. The workspace follows the conventions this relies on (`crates/README.md`): members
//! are globbed, and internal dependencies are `path = "../<crate>"` entries under `[dependencies]`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use super::Error;

/// One workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crate {
    /// The package name (`omuzaki-char`).
    pub name: String,
    /// Its directory, relative to the workspace root.
    pub dir: PathBuf,
    /// The library: its name in paths (`omuzaki_char`) and its root file relative to `dir`, if the package has one.
    pub lib: Option<(String, PathBuf)>,
    /// The members it depends on under `[dependencies]`, by package name. Optional ones are left out: a default
    /// build doesn't link them.
    pub deps: Vec<String>,
    /// Its binaries: name and root file relative to `dir`.
    pub bins: Vec<(String, PathBuf)>,
}

/// Every member of the workspace at `root`, sorted by name.
///
/// # Errors
/// A manifest that can't be read or isn't valid TOML, or a member without a package name.
pub fn load(root: &Path) -> Result<Vec<Crate>, Error> {
    let workspace = read_toml(&root.join("Cargo.toml"))?;
    let mut dirs = Vec::new();
    for member in strings(workspace.get("workspace").and_then(|w| w.get("members"))) {
        match member.strip_suffix("/*") {
            Some(parent) => dirs.extend(subdirs_with_manifest(&root.join(parent))?),
            None => dirs.push(root.join(member)),
        }
    }
    let mut crates = Vec::new();
    let mut by_dir = Vec::new();
    for dir in &dirs {
        let manifest = read_toml(&dir.join("Cargo.toml"))?;
        let name = manifest
            .get("package")
            .and_then(|p| p.get("name"))
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Manifest { path: dir.join("Cargo.toml"), message: "no package name".into() })?
            .to_owned();
        by_dir.push((normalise(dir), name.clone()));
        crates.push((dir.clone(), name, manifest));
    }
    let mut out: Vec<Crate> = crates
        .into_iter()
        .map(|(dir, name, manifest)| {
            let deps = path_deps(&dir, &manifest)
                .filter_map(|target| by_dir.iter().find(|(d, _)| *d == target).map(|(_, n)| n.clone()))
                .collect();
            Crate {
                lib: library(&dir, &name, &manifest),
                bins: binaries(&dir, &name, &manifest),
                dir: dir.strip_prefix(root).unwrap_or(&dir).to_path_buf(),
                name,
                deps,
            }
        })
        .collect();
    out.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(out)
}

fn read_toml(path: &Path) -> Result<Table, Error> {
    let text = fs::read_to_string(path).map_err(|source| Error::Read { path: path.to_owned(), source })?;
    toml::from_str(&text).map_err(|e| Error::Manifest { path: path.to_owned(), message: e.to_string() })
}

fn strings(value: Option<&Value>) -> impl Iterator<Item = &str> {
    value.and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str)
}

fn subdirs_with_manifest(parent: &Path) -> Result<Vec<PathBuf>, Error> {
    let entries = fs::read_dir(parent).map_err(|source| Error::Read { path: parent.to_owned(), source })?;
    let mut dirs: Vec<PathBuf> =
        entries.filter_map(Result::ok).map(|e| e.path()).filter(|p| p.join("Cargo.toml").is_file()).collect();
    dirs.sort();
    Ok(dirs)
}

/// The directories of the non-optional `path` dependencies under `[dependencies]`.
fn path_deps<'a>(dir: &'a Path, manifest: &'a Table) -> impl Iterator<Item = PathBuf> + 'a {
    manifest.get("dependencies").and_then(Value::as_table).into_iter().flatten().filter_map(move |(_, spec)| {
        let spec = spec.as_table()?;
        let optional = spec.get("optional").and_then(Value::as_bool).unwrap_or(false);
        let path = spec.get("path").and_then(Value::as_str).filter(|_| !optional)?;
        Some(normalise(&dir.join(path)))
    })
}

/// `[lib]` as cargo reads it: a declared path or `src/lib.rs`, named after the package unless `[lib] name` says.
fn library(dir: &Path, package: &str, manifest: &Table) -> Option<(String, PathBuf)> {
    let section = manifest.get("lib").and_then(Value::as_table);
    let root = section
        .and_then(|lib| lib.get("path"))
        .and_then(Value::as_str)
        .map_or_else(|| PathBuf::from("src/lib.rs"), PathBuf::from);
    if !dir.join(&root).is_file() {
        return None;
    }
    let name = section.and_then(|lib| lib.get("name")).and_then(Value::as_str).map(str::to_owned);
    Some((name.unwrap_or_else(|| package.replace('-', "_")), root))
}

/// `[[bin]]` entries plus the ones cargo finds by itself (`src/main.rs`, `src/bin/*.rs`, `src/bin/*/main.rs`).
fn binaries(dir: &Path, package: &str, manifest: &Table) -> Vec<(String, PathBuf)> {
    let mut bins: Vec<(String, PathBuf)> = Vec::new();
    for bin in manifest.get("bin").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_table) {
        let Some(name) = bin.get("name").and_then(Value::as_str) else { continue };
        let path = bin.get("path").and_then(Value::as_str).map_or_else(
            || if name == package { "src/main.rs".to_owned() } else { format!("src/bin/{name}.rs") },
            str::to_owned,
        );
        bins.push((name.to_owned(), PathBuf::from(path)));
    }
    let autobins = manifest.get("package").and_then(|p| p.get("autobins")).and_then(Value::as_bool).unwrap_or(true);
    if autobins {
        let mut found = vec![(package.to_owned(), PathBuf::from("src/main.rs"))];
        if let Ok(entries) = fs::read_dir(dir.join("src/bin")) {
            for path in entries.filter_map(Result::ok).map(|e| e.path()) {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let root = if path.is_dir() { path.join("main.rs") } else { path.clone() };
                let relative = root.strip_prefix(dir).unwrap_or(&root).to_path_buf();
                found.push((stem, relative));
            }
        }
        for (name, root) in found {
            let declared = bins.iter().any(|(n, r)| *n == name || *r == root);
            if !declared && dir.join(&root).is_file() {
                bins.push((name, root));
            }
        }
    }
    bins.sort();
    bins
}

/// `a/b/../c` as `a/c`, without touching the disk: dependency paths are compared, not opened.
pub(super) fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalise_resolves_parent_and_current_dirs() {
        assert_eq!(normalise(Path::new("/w/crates/a/../b/./src")), PathBuf::from("/w/crates/b/src"));
    }
}
