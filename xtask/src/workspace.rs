//! The workspace as the checks see it: member crates, where they live and what they depend on.
//!
//! Loaded once from `cargo metadata --no-deps` (members only: fast, offline, deterministic) and then queried as plain
//! data, so the logic built on top is testable without cargo.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::error::Result;
use crate::process::{capture, cargo};

/// Which manifest section a dependency was declared in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DepKind {
    /// `[dependencies]`.
    Normal,
    /// `[build-dependencies]`.
    Build,
    /// `[dev-dependencies]`.
    Dev,
}

impl DepKind {
    /// The manifest section name, for messages.
    pub fn section(self) -> &'static str {
        match self {
            Self::Normal => "[dependencies]",
            Self::Build => "[build-dependencies]",
            Self::Dev => "[dev-dependencies]",
        }
    }
}

/// One declared dependency of a workspace crate (any crate, internal or external).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Dependency {
    /// The depended-on package's real name (not a `rename`).
    pub name: String,
    /// The section it was declared in.
    pub kind: DepKind,
}

/// A workspace member.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Package {
    /// Package name.
    pub name: String,
    /// The crate's directory, relative to the workspace root.
    pub dir: PathBuf,
    /// Everything its manifest declares, for every target platform.
    pub dependencies: Vec<Dependency>,
    /// The `layer` the manifest declares under `[package.metadata.omuzaki]`, if any. The layering table classifies most
    /// crates by name; a format library (`omuzaki-gltf`) has no name to tell it apart by, so it says so here.
    pub layer: Option<String>,
    /// The dependencies this crate holds only to link them in (`link_only` under `[package.metadata.omuzaki]`, ADR 0015).
    /// A change to one of them can break this crate's link, not its behaviour, so `check --affected` runs only this
    /// crate's link gate for it. Empty for nearly every crate.
    pub link_only: Vec<String>,
    /// Whether the crate builds for `wasm32-unknown-unknown`: true unless its manifest opts out with `wasm = false` under
    /// `[package.metadata.omuzaki]` (the rule `tools/ci/crates.py` applies), or it is `xtask`, which is host-only tooling.
    pub wasm: bool,
}

/// The crates that never build for wasm whatever their manifest says (`HOST_ONLY` in `tools/ci/crates.py`).
const HOST_ONLY: [&str; 1] = ["xtask"];

/// All members of a Cargo workspace, sorted by name so every report is stable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// The directory holding the workspace `Cargo.toml`; git and cargo commands run here.
    pub root: PathBuf,
    /// The member crates.
    pub packages: Vec<Package>,
}

impl Workspace {
    /// Asks cargo for the workspace containing `manifest_path` (or the current directory when `None`).
    pub fn load(manifest_path: Option<&Path>) -> Result<Self> {
        let mut command = Command::new(cargo());
        command.args(["metadata", "--format-version", "1", "--no-deps"]);
        if let Some(path) = manifest_path {
            command.arg("--manifest-path").arg(path);
        }
        Self::parse(&capture(&mut command)?)
    }

    /// Reads `cargo metadata --format-version 1 --no-deps` output.
    pub fn parse(json: &[u8]) -> Result<Self> {
        let raw: RawMetadata = serde_json::from_slice(json)?;
        let mut packages: Vec<Package> = raw
            .packages
            .into_iter()
            .map(|package| {
                let manifest_dir = package.manifest_path.parent().unwrap_or(&package.manifest_path);
                let dir = manifest_dir.strip_prefix(&raw.workspace_root).unwrap_or(manifest_dir).to_path_buf();
                let dependencies = package
                    .dependencies
                    .into_iter()
                    .map(|dependency| Dependency { name: dependency.name, kind: dep_kind(dependency.kind.as_deref()) })
                    .collect();
                let layer = package
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("omuzaki")?.get("layer")?.as_str())
                    .map(str::to_owned);
                let link_only = package
                    .metadata
                    .as_ref()
                    .and_then(|metadata| metadata.get("omuzaki")?.get("link_only")?.as_array())
                    .map(|names| names.iter().filter_map(|name| name.as_str().map(str::to_owned)).collect())
                    .unwrap_or_default();
                let opted_out =
                    package.metadata.as_ref().and_then(|metadata| metadata.get("omuzaki")?.get("wasm")?.as_bool())
                        == Some(false);
                let wasm = !opted_out && !HOST_ONLY.contains(&package.name.as_str());
                Package { name: package.name, dir, dependencies, layer, link_only, wasm }
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Self { root: raw.workspace_root, packages })
    }

    /// The member called `name`, if any (external crates are not members).
    pub fn package(&self, name: &str) -> Option<&Package> {
        self.packages.iter().find(|package| package.name == name)
    }
}

/// Cargo reports normal dependencies as `null`; an unknown future kind is treated as normal (the strictest reading).
fn dep_kind(kind: Option<&str>) -> DepKind {
    match kind {
        Some("dev") => DepKind::Dev,
        Some("build") => DepKind::Build,
        _ => DepKind::Normal,
    }
}

#[derive(Deserialize)]
struct RawMetadata {
    workspace_root: PathBuf,
    packages: Vec<RawPackage>,
}

#[derive(Deserialize)]
struct RawPackage {
    name: String,
    manifest_path: PathBuf,
    dependencies: Vec<RawDependency>,
    /// `[package.metadata]`, free-form; `null` when the manifest has none.
    #[serde(default)]
    metadata: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct RawDependency {
    name: String,
    kind: Option<String>,
}

/// Builders for in-memory workspaces, so the check logic is unit-tested without touching cargo.
#[cfg(test)]
pub mod testing {
    use super::{DepKind, Dependency, Package, Workspace};
    use std::path::PathBuf;

    /// A workspace rooted at `/ws` laid out like the real one (`crates/<name>`, plus `xtask/`); each entry is
    /// `(name, dependencies)`.
    pub fn workspace(crates: &[(&str, &[(&str, DepKind)])]) -> Workspace {
        let mut packages: Vec<Package> = crates
            .iter()
            .map(|(name, dependencies)| Package {
                name: (*name).to_owned(),
                dir: if *name == "xtask" { PathBuf::from("xtask") } else { PathBuf::from("crates").join(name) },
                dependencies: dependencies
                    .iter()
                    .map(|(dependency, kind)| Dependency { name: (*dependency).to_owned(), kind: *kind })
                    .collect(),
                layer: None,
                link_only: Vec::new(),
                wasm: *name != "xtask",
            })
            .collect();
        packages.sort_by(|a, b| a.name.cmp(&b.name));
        Workspace { root: PathBuf::from("/ws"), packages }
    }

    /// `workspace` with `[package.metadata.omuzaki] layer = "<layer>"` on the member `name`.
    pub fn with_layer(mut workspace: Workspace, name: &str, layer: &str) -> Workspace {
        if let Some(package) = workspace.packages.iter_mut().find(|package| package.name == name) {
            package.layer = Some(layer.to_owned());
        }
        workspace
    }

    /// `workspace` with `[package.metadata.omuzaki] wasm = false` on the member `name`.
    pub fn without_wasm(mut workspace: Workspace, name: &str) -> Workspace {
        if let Some(package) = workspace.packages.iter_mut().find(|package| package.name == name) {
            package.wasm = false;
        }
        workspace
    }

    /// `workspace` with `[package.metadata.omuzaki] link_only = [..]` on the member `name`.
    pub fn with_link_only(mut workspace: Workspace, name: &str, link_only: &[&str]) -> Workspace {
        if let Some(package) = workspace.packages.iter_mut().find(|package| package.name == name) {
            package.link_only = link_only.iter().map(|dependency| (*dependency).to_owned()).collect();
        }
        workspace
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_link_only_dependencies_and_ignores_malformed_ones() {
        let json = br#"{
            "workspace_root": "/ws",
            "packages": [
                {"name": "a", "manifest_path": "/ws/crates/a/Cargo.toml", "dependencies": [],
                    "metadata": {"omuzaki": {"link_only": ["x", 3, "y"]}}},
                {"name": "b", "manifest_path": "/ws/crates/b/Cargo.toml", "dependencies": [],
                    "metadata": {"omuzaki": {"link_only": "x"}}},
                {"name": "c", "manifest_path": "/ws/crates/c/Cargo.toml", "dependencies": []}
            ]
        }"#;
        let ws = Workspace::parse(json).unwrap();
        assert_eq!(ws.package("a").unwrap().link_only, ["x", "y"]);
        assert_eq!(ws.package("b").unwrap().link_only, Vec::<String>::new());
        assert_eq!(ws.package("c").unwrap().link_only, Vec::<String>::new());
    }

    #[test]
    fn parses_members_dependency_kinds_and_relative_dirs() {
        let json = br#"{
            "workspace_root": "/ws",
            "packages": [
                {"name": "omuzaki-b", "manifest_path": "/ws/crates/omuzaki-b/Cargo.toml", "dependencies": [
                    {"name": "serde", "kind": null},
                    {"name": "omuzaki-a", "kind": "dev"},
                    {"name": "cc", "kind": "build"}]},
                {"name": "omuzaki-a", "manifest_path": "/ws/crates/omuzaki-a/Cargo.toml", "dependencies": [],
                    "metadata": {"omuzaki": {"layer": "lib"}, "docs": {"rs": true}}}
            ]
        }"#;
        let ws = Workspace::parse(json).unwrap();
        assert_eq!(ws.root, PathBuf::from("/ws"));
        assert_eq!(ws.packages.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["omuzaki-a", "omuzaki-b"]);
        let b = ws.package("omuzaki-b").unwrap();
        assert_eq!(b.dir, PathBuf::from("crates/omuzaki-b"));
        let kinds: Vec<DepKind> = b.dependencies.iter().map(|d| d.kind).collect();
        assert_eq!(kinds, [DepKind::Normal, DepKind::Dev, DepKind::Build]);
        assert!(ws.package("serde").is_none());
        assert_eq!(ws.package("omuzaki-a").unwrap().layer.as_deref(), Some("lib"));
        assert_eq!(b.layer, None, "no metadata means no layer");
        assert!(b.wasm && ws.package("omuzaki-a").unwrap().wasm, "no `wasm = false` means it builds for wasm");
    }

    #[test]
    fn reads_the_wasm_opt_out_and_treats_xtask_as_host_only() {
        let json = br#"{
            "workspace_root": "/ws",
            "packages": [
                {"name": "a", "manifest_path": "/ws/crates/a/Cargo.toml", "dependencies": [],
                    "metadata": {"omuzaki": {"wasm": false}}},
                {"name": "b", "manifest_path": "/ws/crates/b/Cargo.toml", "dependencies": [],
                    "metadata": {"omuzaki": {"wasm": "no"}}},
                {"name": "xtask", "manifest_path": "/ws/xtask/Cargo.toml", "dependencies": []}
            ]
        }"#;
        let ws = Workspace::parse(json).unwrap();
        assert!(!ws.package("a").unwrap().wasm);
        assert!(ws.package("b").unwrap().wasm, "only a literal `false` opts out, as in crates.py");
        assert!(!ws.package("xtask").unwrap().wasm);
    }

    #[test]
    fn metadata_that_is_not_ours_or_not_text_is_no_layer() {
        let json = br#"{
            "workspace_root": "/ws",
            "packages": [
                {"name": "a", "manifest_path": "/ws/crates/a/Cargo.toml", "dependencies": [], "metadata": null},
                {"name": "b", "manifest_path": "/ws/crates/b/Cargo.toml", "dependencies": [], "metadata": {"omuzaki": {}}},
                {"name": "c", "manifest_path": "/ws/crates/c/Cargo.toml", "dependencies": [], "metadata": {"omuzaki": {"layer": 3}}},
                {"name": "d", "manifest_path": "/ws/crates/d/Cargo.toml", "dependencies": []}
            ]
        }"#;
        let ws = Workspace::parse(json).unwrap();
        assert!(ws.packages.iter().all(|p| p.layer.is_none()));
    }

    #[test]
    fn rejects_output_that_is_not_metadata() {
        assert!(Workspace::parse(b"not json").is_err());
    }
}
