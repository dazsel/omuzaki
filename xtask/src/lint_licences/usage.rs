//! What the repository uses that needs a licence file: the workspace's direct dependencies, the tools CI installs and
//! the committed fonts and notices.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::process::Command;

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
    version: String,
    /// Absent for path packages: the workspace members and any external path dependency.
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    dependencies: Vec<String>,
}

impl RawPackage {
    /// Whether this package is a workspace member: a path package (no `source`) with the name and version of one of
    /// `members`. A registry or git package, or an external path package of another version, that shares a member's
    /// name is a different package.
    fn is_member(&self, members: &Members) -> bool {
        self.source.is_none() && members.contains(&(self.name.clone(), self.version.clone()))
    }

    /// Whether the lock entry `dep` (`name`, `name version` or `name version (source)`) names this package.
    fn is_named_by(&self, dep: &str) -> bool {
        let mut parts = dep.splitn(3, ' ');
        let name = parts.next().unwrap_or_default();
        let version = parts.next();
        let source = parts.next().map(|source| source.trim_matches(['(', ')']));
        name == self.name
            && version.is_none_or(|version| version == self.version)
            && source.is_none_or(|source| self.source.as_deref() == Some(source))
    }
}

/// The workspace's own crates, each as `(name, version)`.
pub type Members = BTreeSet<(String, String)>;

impl Lock {
    /// Reads a `Cargo.lock`. `members` are the workspace's own crates, matched by identity: a path package (no
    /// `source`) with a member's name and version. A package that is not one of them is external, even if it shares a
    /// member's name (from a registry, or a path package of another version); it needs a file like any other.
    pub fn parse(text: &str, members: &Members) -> Result<Self, String> {
        let raw: RawLock = toml::from_str(text).map_err(|e: toml::de::Error| e.message().to_owned())?;
        let mut lock = Self::default();
        for package in &raw.package {
            lock.packages.insert(package.name.clone());
            if !package.is_member(members) {
                continue;
            }
            for dep in &package.dependencies {
                // A name the lock cannot resolve counts as external: the stricter reading.
                let internal = raw.package.iter().any(|target| target.is_named_by(dep) && target.is_member(members));
                if !internal {
                    let name = dep.split(' ').next().unwrap_or(dep);
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

/// File extensions of binary assets: fonts, images, models (`.onnx`, `.tflite`, `.task`, ...), audio, video and meshes. Source, config and text files are
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
    "svg",
    "hdr",
    "exr",
    "dds",
    "ktx",
    "ktx2",
    "rgb",
    "onnx",
    "tflite",
    "task",
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
/// everything under `.github/assets/`. Paths are relative to `root`, `/`-separated and sorted. In a git checkout only
/// tracked files count, so an ignored local file never fails the lint; outside one the files on disk are walked.
pub fn assets(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    if let Some(files) = tracked_files(root) {
        found.extend(files.into_iter().filter(|file| is_scanned_asset(Path::new(file))));
    } else {
        walk(root, Path::new(CRATES_DIR), &mut found, &is_asset);
        walk(root, Path::new(GITHUB_ASSETS_DIR), &mut found, &|_| true);
    }
    found.sort();
    found
}

/// The files git tracks under the scanned folders, or `None` when `root` is not the top of a git checkout (or git is
/// missing). Files deleted from the working tree but still in the index are left out.
fn tracked_files(root: &Path) -> Option<Vec<String>> {
    let git = |args: &[&str]| {
        let out = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
        out.status.success().then_some(out.stdout)
    };
    // A project nested in some other checkout is not a checkout of its own.
    if !git(&["rev-parse", "--show-prefix"])?.trim_ascii().is_empty() {
        return None;
    }
    let listed = git(&["ls-files", "-z", "--", CRATES_DIR, GITHUB_ASSETS_DIR])?;
    let files = listed.split(|byte| *byte == 0).filter(|name| !name.is_empty());
    Some(files.map(|name| String::from_utf8_lossy(name).into_owned()).filter(|f| root.join(f).is_file()).collect())
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
            &members(&[("app", "0.1.0"), ("core", "0.1.0")]),
        )
        .unwrap_or_default();
        assert_eq!(lock.direct.keys().collect::<Vec<_>>(), ["serde", "toml", "vendored"]);
        assert!(lock.packages.contains("core") && !lock.direct.contains_key("serde_derive"));
    }

    fn members(list: &[(&str, &str)]) -> Members {
        list.iter().map(|(name, version)| ((*name).to_owned(), (*version).to_owned())).collect()
    }

    #[test]
    fn an_external_path_package_of_another_version_is_not_the_member() {
        let lock = Lock::parse(
            r#"
            [[package]]
            name = "app"
            version = "0.1.0"
            dependencies = ["foo 0.2.0"]
            [[package]]
            name = "foo"
            version = "0.1.0"
            dependencies = ["member-only"]
            [[package]]
            name = "foo"
            version = "0.2.0"
            dependencies = ["transitive"]
            [[package]]
            name = "member-only"
            version = "1.0.0"
            source = "registry+https://example.test/index"
            [[package]]
            name = "transitive"
            version = "1.0.0"
            source = "registry+https://example.test/index"
            "#,
            &members(&[("app", "0.1.0"), ("foo", "0.1.0")]),
        )
        .unwrap_or_default();
        let direct: Vec<_> =
            lock.direct.iter().map(|(k, v)| (k.as_str(), v.iter().map(String::as_str).collect::<Vec<_>>())).collect();
        assert_eq!(direct, [("foo", vec!["app"]), ("member-only", vec!["foo"])], "no `transitive`: {direct:?}");
    }

    #[test]
    fn an_external_package_sharing_a_members_name_is_not_a_member() {
        let lock = Lock::parse(
            r#"
            version = 4
            [[package]]
            name = "app"
            version = "0.1.0"
            dependencies = ["core 0.1.0", "foo 2.0.0", "foo 1.0.0 (registry+https://example.test/index)"]
            [[package]]
            name = "foo"
            version = "1.0.0"
            source = "registry+https://example.test/index"
            [[package]]
            name = "foo"
            version = "2.0.0"
            [[package]]
            name = "core"
            version = "0.1.0"
            "#,
            &members(&[("app", "0.1.0"), ("core", "0.1.0"), ("foo", "0.1.0")]),
        )
        .unwrap_or_default();
        assert_eq!(lock.direct.keys().collect::<Vec<_>>(), ["foo"], "the registry `foo` is external");
        let only_member = Lock::parse(
            "[[package]]\nname = \"app\"\nversion = \"0.1.0\"\ndependencies = [\"foo\"]\n[[package]]\nname = \"foo\"\nversion = \"0.1.0\"\n",
            &members(&[("app", "0.1.0"), ("foo", "0.1.0")]),
        )
        .unwrap_or_default();
        assert!(only_member.direct.is_empty());
    }

    #[test]
    fn a_registry_package_named_like_a_member_does_not_make_a_member_of_it() {
        let lock = Lock::parse(
            r#"
            [[package]]
            name = "foo"
            version = "1.0.0"
            source = "registry+https://example.test/index"
            dependencies = ["bar"]
            [[package]]
            name = "bar"
            version = "1.0.0"
            "#,
            &members(&[("foo", "0.1.0")]),
        )
        .unwrap_or_default();
        assert!(lock.direct.is_empty(), "the external `foo` is no workspace crate: {:?}", lock.direct);
    }

    fn git(root: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(root).args(args).status().is_ok_and(|status| status.success());
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn only_tracked_assets_count_in_a_git_checkout() {
        let root = tempfile::tempdir().expect("temp dir");
        git(root.path(), &["init", "-q"]);
        fs::create_dir_all(root.path().join("crates/a")).expect("mkdir");
        for file in ["crates/a/tracked.onnx", "crates/a/untracked.onnx"] {
            fs::write(root.path().join(file), "").expect("write");
        }
        git(root.path(), &["add", "crates/a/tracked.onnx"]);
        assert_eq!(assets(root.path()), ["crates/a/tracked.onnx"]);
    }

    #[test]
    fn model_formats_tflite_and_task_are_assets() {
        for file in ["crates/a/m.tflite", "crates/a/m.TASK"] {
            assert!(is_scanned_asset(Path::new(file)), "{file}");
        }
        assert!(!is_scanned_asset(Path::new("crates/a/m.taskx")));
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
