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
    #[serde(default)]
    dependencies: Vec<String>,
}

impl Lock {
    /// Reads a `Cargo.lock`. `members` are the workspace's own crates: a package without a `source` that is not one of
    /// them is an external path dependency and needs a file like any other.
    pub fn parse(text: &str, members: &BTreeSet<String>) -> Result<Self, String> {
        let raw: RawLock = toml::from_str(text).map_err(|e: toml::de::Error| e.message().to_owned())?;
        let mut lock = Self::default();
        for package in &raw.package {
            lock.packages.insert(package.name.clone());
            if !members.contains(&package.name) {
                continue;
            }
            // A dependency is `name`, `name version` or `name version (source)`.
            for name in package.dependencies.iter().filter_map(|dep| dep.split(' ').next()) {
                if !members.contains(name) {
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

/// Where [`assets`] looks for binary assets and `NOTICES.md`.
const CRATES_DIR: &str = "crates";
/// Where [`assets`] takes every file.
const GITHUB_ASSETS_DIR: &str = ".github/assets";

/// File extensions of binary assets: fonts, images, models, audio, video and meshes. Source, config and text files are
/// code, not assets; a third-party text file is declared by a `NOTICES.md` beside it.
pub const ASSET_EXTENSIONS: &[&str] = &[
    "ttf",
    "otf",
    "ttc",
    "woff",
    "woff2",
    "png",
    "jpg",
    "jpeg",
    "gif",
    "webp",
    "bmp",
    "tga",
    "tif",
    "tiff",
    "ico",
    "hdr",
    "exr",
    "dds",
    "ktx",
    "ktx2",
    "rgb",
    "onnx",
    "safetensors",
    "gguf",
    "pt",
    "pth",
    "ckpt",
    "bin",
    "npy",
    "npz",
    "wav",
    "ogg",
    "mp3",
    "flac",
    "opus",
    "mp4",
    "mov",
    "webm",
    "mkv",
    "glb",
    "gltf",
    "vrm",
    "fbx",
    "obj",
    "blend",
];

/// Whether a file under `crates/` is third-party material that needs a licence file.
fn is_asset(name: &str) -> bool {
    let ext = Path::new(name).extension().map(std::ffi::OsStr::to_ascii_lowercase);
    name == "NOTICES.md" || ext.is_some_and(|ext| ASSET_EXTENSIONS.iter().any(|known| ext == *known))
}

/// Whether `file` (relative to the root) is something [`assets`] scans: a binary asset or `NOTICES.md` under `crates/`,
/// or anything under `.github/assets/`.
pub fn is_scanned_asset(file: &Path) -> bool {
    let named = file.file_name().is_some_and(|name| is_asset(&name.to_string_lossy()));
    (file.starts_with(CRATES_DIR) && named) || file.starts_with(GITHUB_ASSETS_DIR)
}

/// The committed third-party material: binary assets (see [`ASSET_EXTENSIONS`]) and `NOTICES.md` under `crates/`, and
/// everything under `.github/assets/`. Paths are relative to `root`, `/`-separated and sorted.
pub fn assets(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    walk(root, Path::new(CRATES_DIR), &mut found, &is_asset);
    walk(root, Path::new(GITHUB_ASSETS_DIR), &mut found, &|_| true);
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
    fn direct_dependencies_are_the_non_members_that_members_name() {
        let lock = Lock::parse(
            r#"
            version = 4
            [[package]]
            name = "app"
            version = "0.1.0"
            dependencies = ["core", "serde 1.0.0", "toml", "vendored"]
            [[package]]
            name = "core"
            version = "0.1.0"
            [[package]]
            name = "vendored"
            version = "0.1.0"
            [[package]]
            name = "serde"
            version = "1.0.0"
            source = "registry+https://github.com/rust-lang/crates.io-index"
            dependencies = ["serde_derive"]
            "#,
            &["app", "core"].map(str::to_owned).into(),
        )
        .unwrap_or_default();
        assert_eq!(lock.direct.keys().collect::<Vec<_>>(), ["serde", "toml", "vendored"]);
        assert!(lock.packages.contains("core") && !lock.direct.contains_key("serde_derive"));
    }

    #[test]
    fn models_images_and_fonts_under_crates_are_assets_but_source_is_not() {
        let root = tempfile::tempdir().expect("temp dir");
        for file in ["crates/a/assets/model.ONNX", "crates/a/img/x.png", "crates/a/f.ttf", "crates/a/NOTICES.md"] {
            let path = root.path().join(file);
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            fs::write(path, "").expect("write");
        }
        for file in ["crates/a/src/lib.rs", "crates/a/Cargo.toml", "crates/a/README.md", "crates/a/data.json"] {
            fs::write(root.path().join(file), "").ok();
        }
        assert_eq!(
            assets(root.path()),
            ["crates/a/NOTICES.md", "crates/a/assets/model.ONNX", "crates/a/f.ttf", "crates/a/img/x.png"]
        );
    }

    #[test]
    fn tools_inputs_are_split_and_versions_dropped() {
        let text = "      with:\n          tools: cargo-deny,cargo-about@0.9.2\n          tools: \"cargo-nextest\"\n";
        assert_eq!(tools_in(text), ["cargo-deny", "cargo-about", "cargo-nextest"]);
    }
}
