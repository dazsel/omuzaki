//! A short rendered as a script sees it: `omuzaki run short.render` performs a kit character through a two-line short
//! on this machine's GPU and writes a WebM into the project, the same bytes on every run. Skips where no GPU adapter
//! exists (lavapipe is one, in CI and the dev VMs).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// `omuzaki <args>` with its data and config folders in `home`.
fn omuzaki(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omuzaki"))
        .args(args)
        .env("OMUZAKI_DATA_DIR", home.join("data"))
        .env("OMUZAKI_CONFIG_DIR", home.join("config"))
        .output()
        .expect("run omuzaki")
}

/// `omuzaki` that must succeed; its stdout.
fn ok(home: &Path, args: &[&str]) -> String {
    let out = omuzaki(home, args);
    let stdout = String::from_utf8(out.stdout).expect("utf-8 stdout");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "omuzaki {args:?}\nstdout: {stdout}\nstderr: {stderr}");
    stdout
}

fn has_gpu() -> bool {
    omuzaki_render::WgpuRenderer::headless(&omuzaki_render::HeadlessOptions::default()).is_ok()
}

const SHORT: &str = r#"{"short": {"character": "lumi", "fps": 12, "seed": 7, "line": [
    {"text": "You found me!", "face": "happy", "camera": "close+tilt-left+punch"},
    {"text": "Now go away.", "face": "angry", "camera": "bust"}
]}, "size": [72, 128], "out": "shorts/OUT.webm"}"#;

/// Renders the short to `shorts/<name>.webm` and returns the run's JSON and the file's bytes.
fn render(home: &Path, project: &str, name: &str) -> (Value, Vec<u8>) {
    let args = SHORT.replace("OUT", name);
    let out = ok(home, &["--project", project, "--json", "run", "short.render", "--args", &args]);
    let report: Value = serde_json::from_str(&out).expect("JSON on stdout");
    let bytes = std::fs::read(Path::new(project).join("shorts").join(format!("{name}.webm"))).expect("the video");
    (report, bytes)
}

#[test]
fn a_short_rendered_from_the_shell_is_a_webm_and_the_same_on_every_run() {
    if !has_gpu() {
        eprintln!("skipped: no GPU adapter");
        return;
    }
    let home = tempfile::tempdir().expect("temp dir");
    let project = home.path().join("project");
    let project_arg = project.to_str().expect("utf-8 path");
    ok(home.path(), &["new", project_arg]);
    ok(
        home.path(),
        &[
            "--project",
            project_arg,
            "run",
            "character.create",
            "--args",
            r#"{"id":"lumi","name":"Lumi","template":"standard"}"#,
        ],
    );

    let (report, first) = render(home.path(), project_arg, "a");
    assert_eq!(&first[..4], &[0x1A, 0x45, 0xDF, 0xA3], "an EBML (WebM) header: {report:#}");
    let text = report.to_string();
    assert!(text.contains("shorts/a.webm"), "the report names the video: {report:#}");
    assert!(first.len() > 1000, "frames were written: {} bytes", first.len());

    let (_, second) = render(home.path(), project_arg, "b");
    assert_eq!(first, second, "a short renders the same bytes on every run");
}
