//! A throwaway Cargo workspace on disk, so the tests drive the real `xtask` binary against real `cargo metadata` and
//! real git, not mocks of them.
//!
//! Integration-test helpers may unwrap: a panic is the failure report.
#![allow(clippy::unwrap_used, clippy::expect_used, dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

/// Where a fixture crate declares a dependency.
#[derive(Clone, Copy)]
pub enum Section {
    Normal,
    Dev,
    Build,
}

pub struct Fixture {
    dir: TempDir,
}

pub struct Run {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

impl Fixture {
    /// An empty workspace whose members are `crates/*`, like the real one.
    pub fn new() -> Self {
        let fixture = Self { dir: TempDir::new().unwrap() };
        fixture.write("Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n");
        fixture
    }

    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Adds `crates/<name>` with the given dependencies on other fixture crates.
    pub fn krate(self, name: &str, dependencies: &[(&str, Section)]) -> Self {
        let mut manifest = format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n");
        for (header, wanted) in [
            ("dependencies", Section::Normal),
            ("dev-dependencies", Section::Dev),
            ("build-dependencies", Section::Build),
        ] {
            manifest += &["\n[", header, "]\n"].concat();
            for (dependency, section) in dependencies {
                if std::mem::discriminant(section) == std::mem::discriminant(&wanted) {
                    manifest += &[dependency, " = { path = \"../", dependency, "\" }\n"].concat();
                }
            }
        }
        self.write(&format!("crates/{name}/Cargo.toml"), &manifest);
        self.write(&format!("crates/{name}/src/lib.rs"), "");
        self
    }

    /// Declares `layer = "<layer>"` under `[package.metadata.omuzaki]` in the manifest of the fixture crate `name`.
    pub fn layer(self, name: &str, layer: &str) -> Self {
        let path = self.root().join(format!("crates/{name}/Cargo.toml"));
        let manifest = fs::read_to_string(&path).unwrap();
        fs::write(path, format!("{manifest}\n[package.metadata.omuzaki]\nlayer = \"{layer}\"\n")).unwrap();
        self
    }

    pub fn write(&self, relative: &str, content: &str) {
        let path: PathBuf = self.root().join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, content).unwrap();
    }

    /// Runs git in the fixture with a fixed identity and no hooks or signing from the host's config.
    pub fn git(&self, args: &[&str]) -> String {
        let output = Command::new("git")
            .current_dir(self.root())
            .args(["-c", "user.name=fixture", "-c", "user.email=fixture@example.com", "-c", "commit.gpgsign=false"])
            .args(["-c", "core.hooksPath=/dev/null"])
            .args(args)
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Makes the fixture a git repository with everything committed on branch `main`.
    pub fn commit_all(&self, message: &str) {
        if !self.root().join(".git").exists() {
            self.git(&["init", "-q", "-b", "main"]);
        }
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "-m", message]);
    }

    /// Runs the real `xtask` binary against this workspace.
    pub fn xtask(&self, args: &[&str]) -> Run {
        let output: Output =
            Command::new(env!("CARGO_BIN_EXE_xtask")).current_dir(self.root()).args(args).output().unwrap();
        Run {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        }
    }
}
