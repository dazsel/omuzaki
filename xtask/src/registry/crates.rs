//! The workspace's crates as the gate needs them, read from the manifests and `src/` trees on disk.
//!
//! Not `cargo metadata`: the gate also runs inside test binaries, including Windows ones started through WSL interop,
//! where there is no cargo to ask. The workspace follows the conventions this relies on (`crates/README.md`): members
//! are globbed, and internal dependencies are `path = "../<crate>"` entries under `[dependencies]`.

use std::fs;
use std::path::{Component, Path, PathBuf};

use toml::{Table, Value};

use super::{Error, wiring};

/// The build-dependency that generates a crate's `link()` calls.
const LINK_BUILD: &str = "omuzaki-link-build";

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
    /// Whether its manifest sets `registers = true` under `[package.metadata.omuzaki]`: Apps generate a `link()` call
    /// for it (ADR 0054 section 6).
    pub registers: bool,
    /// Whether its `link()` calls are generated from the `registers` flags of its dependencies and appear in no source
    /// file: it lists `omuzaki-link-build`, its `build.rs` calls `emit` from `main`, and its library's `define_link!`
    /// includes `link_calls.rs` (read as tokens, so a commented-out include does not count).
    pub generates_links: bool,
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
    let workspace_deps = workspace.get("workspace").and_then(|w| w.get("dependencies")).and_then(Value::as_table);
    let mut out: Vec<Crate> = Vec::new();
    for (dir, name, manifest) in crates {
        let mut deps = Vec::new();
        for target in path_deps(root, &dir, &manifest, workspace_deps)? {
            deps.extend(by_dir.iter().find(|(d, _)| *d == target).map(|(_, n)| n.clone()));
        }
        out.push(Crate {
            lib: library(&dir, &name, &manifest),
            bins: binaries(&dir, &name, &manifest),
            registers: manifest
                .get("package")
                .and_then(|p| p.get("metadata")?.get("omuzaki")?.get("registers")?.as_bool())
                .unwrap_or(false),
            generates_links: generates_links(&dir, &manifest),
            dir: dir.strip_prefix(root).unwrap_or(&dir).to_path_buf(),
            name,
            deps,
        });
    }
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

/// The directories of the non-optional workspace dependencies under `[dependencies]`: `path` entries, and
/// `workspace = true` entries resolved through `[workspace.dependencies]` (their `path` is relative to the workspace
/// root). A crate from a registry or git is not a member and is left out. A dependency in a form this does not know is an
/// error: skipping it would hide a member from the link gate.
fn path_deps(root: &Path, dir: &Path, manifest: &Table, workspace_deps: Option<&Table>) -> Result<Vec<PathBuf>, Error> {
    let manifest_path = dir.join("Cargo.toml");
    let fail = |message: String| Error::Manifest { path: manifest_path.clone(), message };
    let mut found = Vec::new();
    for (key, entry) in manifest.get("dependencies").and_then(Value::as_table).into_iter().flatten() {
        let Some(member) = entry.as_table() else { continue }; // `name = "1.2"`
        let inherits = member.get("workspace").and_then(Value::as_bool) == Some(true);
        let mut spec = Table::new();
        if inherits {
            match workspace_deps.and_then(|deps| deps.get(key)) {
                Some(Value::Table(inherited)) => spec = inherited.clone(),
                Some(_) => {}
                None => {
                    return Err(fail(format!(
                        "`{key}` says `workspace = true`, but [workspace.dependencies] has no `{key}`"
                    )));
                }
            }
        }
        let base = if inherits && !member.contains_key("path") { root } else { dir };
        for (name, value) in member {
            if name != "workspace" {
                spec.insert(name.clone(), value.clone());
            }
        }
        let optional = spec.get("optional").and_then(Value::as_bool).unwrap_or(false);
        match spec.get("path").and_then(Value::as_str) {
            Some(path) if !optional => found.push(normalise(&base.join(path))),
            Some(_) => {}
            None if inherits || ["version", "git", "registry"].iter().any(|k| spec.contains_key(*k)) => {}
            None => {
                return Err(fail(format!(
                    "`{key}` is a dependency form the link gate does not read (no `path`, `version`, `git` or `workspace`)"
                )));
            }
        }
    }
    Ok(found)
}

/// Whether the crate's `link()` calls really are generated: it lists `omuzaki-link-build` under `[build-dependencies]`,
/// its build script calls `omuzaki_link_build::emit`, and its library includes the generated `link_calls.rs`. Any one
/// missing leaves the list empty or unused, so the crate must not be credited with calls it does not make.
fn generates_links(dir: &Path, manifest: &Table) -> bool {
    let listed = manifest
        .get("build-dependencies")
        .and_then(Value::as_table)
        .is_some_and(|table| table.contains_key(LINK_BUILD));
    let build = manifest.get("package").and_then(|p| p.get("build")).and_then(Value::as_str).unwrap_or("build.rs");
    let lib = library(dir, "", manifest).map_or_else(|| PathBuf::from("src/lib.rs"), |(_, root)| root);
    let holds =
        |file: &Path, check: fn(&str) -> bool| fs::read_to_string(dir.join(file)).is_ok_and(|text| check(&text));
    listed && holds(Path::new(build), wiring::build_script_emits) && holds(&lib, wiring::library_includes_generated)
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
