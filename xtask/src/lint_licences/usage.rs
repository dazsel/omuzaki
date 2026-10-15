//! What the repository uses that needs a licence file: the workspace's direct dependencies, the tools CI installs and
//! the committed fonts and notices.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Deserialize;

/// The packages of `Cargo.lock`, and which external ones each workspace member depends on directly.
#[derive(Debug, Default)]
pub struct Lock {
    /// Every package name in the lockfile.
    pub packages: BTreeSet<String>,
    /// External package -> the workspace members that depend on it directly.
    pub direct: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Deserialize)]
struct RawLock {
    #[serde(default)]
    package: Vec<RawPackage>,
}

#[derive(Deserialize)]
struct RawPackage {
    name: String,
    source: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

impl Lock {
    /// Reads a `Cargo.lock`. Packages without a `source` are the workspace's own.
    pub fn parse(text: &str) -> Result<Self, String> {
        let raw: RawLock = toml::from_str(text).map_err(|e: toml::de::Error| e.message().to_owned())?;
        let own: BTreeSet<&str> = raw.package.iter().filter(|p| p.source.is_none()).map(|p| p.name.as_str()).collect();
        let mut lock = Self::default();
        for package in &raw.package {
            lock.packages.insert(package.name.clone());
            if package.source.is_some() {
                continue;
            }
            // A dependency is `name`, `name version` or `name version (source)`.
            for name in package.dependencies.iter().filter_map(|dep| dep.split(' ').next()) {
                if !own.contains(name) {
                    lock.direct.entry(name.to_owned()).or_default().insert(package.name.clone());
                }
            }
        }
        Ok(lock)
    }
}

/// The tools the workflows install: every name in a `tools:` input (`tools: cargo-deny,cargo-about@0.9.2`).
pub fn workflow_tools(root: &Path) -> BTreeSet<String> {
    let Ok(read) = fs::read_dir(root.join(".github/workflows")) else { return BTreeSet::new() };
    let texts = read.filter_map(|entry| fs::read_to_string(entry.ok()?.path()).ok()).collect::<Vec<_>>();
    texts.iter().flat_map(|text| tools_in(text)).collect()
}

fn tools_in(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix("tools:"))
        .flat_map(|list| list.trim().trim_matches(['"', '\'']).split(','))
        .map(|tool| tool.split('@').next().unwrap_or(tool).trim().to_owned())
        .filter(|tool| !tool.is_empty())
        .collect()
}

/// The committed third-party material: font files and `NOTICES.md` under `crates/`, and everything under
/// `.github/assets/`. Paths are relative to `root`, `/`-separated and sorted.
pub fn assets(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, Path::new("crates"), &mut found, &|name| {
        let ext = Path::new(name).extension().map(std::ffi::OsStr::to_ascii_lowercase);
        name == "NOTICES.md" || ext.is_some_and(|ext| ext == "ttf" || ext == "otf")
    });
    walk(root, Path::new(".github/assets"), &mut found, &|_| true);
    found.sort();
    found
}

fn walk(root: &Path, dir: &Path, found: &mut Vec<String>, wanted: &dyn Fn(&str) -> bool) {
    let Ok(read) = fs::read_dir(root.join(dir)) else { return };
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let relative = dir.join(&name);
        match entry.file_type() {
            Ok(kind) if kind.is_dir() && name != "target" => walk(root, &relative, found, wanted),
            Ok(kind) if kind.is_file() && wanted(&name) => {
                found
                    .push(relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"));
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_dependencies_are_the_external_ones_members_name() {
        let lock = Lock::parse(
            r#"
            version = 4
            [[package]]
            name = "app"
            version = "0.1.0"
            dependencies = ["core", "serde 1.0.0", "toml"]
            [[package]]
            name = "core"
            version = "0.1.0"
            [[package]]
            name = "serde"
            version = "1.0.0"
            source = "registry+https://github.com/rust-lang/crates.io-index"
            dependencies = ["serde_derive"]
            "#,
        )
        .unwrap_or_default();
        assert_eq!(lock.direct.keys().collect::<Vec<_>>(), ["serde", "toml"]);
        assert!(lock.packages.contains("core") && !lock.direct.contains_key("serde_derive"));
    }

    #[test]
    fn tools_inputs_are_split_and_versions_dropped() {
        let text = "      with:\n          tools: cargo-deny,cargo-about@0.9.2\n          tools: \"cargo-nextest\"\n";
        assert_eq!(tools_in(text), ["cargo-deny", "cargo-about", "cargo-nextest"]);
    }
}
