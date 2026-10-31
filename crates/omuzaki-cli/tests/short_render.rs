//! A short rendered as a script sees it: `omuzaki run short.render` performs a kit character through a two-line short
//! on this machine's GPU and writes the video into the project, the same bytes on every run. Skips where no GPU adapter
//! exists (lavapipe is one, in CI and the dev VMs), or without libvpx, which the encoder loads at run time.

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

/// The encoder loads libvpx at run time; a machine without it can't write a video.
fn has_libvpx() -> bool {
    omuzaki_stream::vp9::available().is_ok()
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
    if !has_gpu() || !has_libvpx() {
        eprintln!("skipped: no GPU adapter or no libvpx");
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

#[test]
fn a_short_can_report_where_the_face_landed_in_every_frame() {
    if !has_gpu() || !has_libvpx() {
        eprintln!("skipped: no GPU adapter or no libvpx");
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
    let args = SHORT.replace("OUT", "pts").replace(r#""size""#, r#""face_points": true, "size""#);
    let out = ok(home.path(), &["--project", project_arg, "--json", "run", "short.render", "--args", &args]);
    let report: Value = serde_json::from_str(&out).expect("JSON on stdout");
    let text = report.to_string();
    assert!(text.contains("shorts/pts.points.json"), "the report names the points: {report:#}");
    let frames = report.pointer("/plan/output/frames").and_then(Value::as_u64).expect("frames");
    let points: Vec<serde_json::Map<String, Value>> =
        serde_json::from_slice(&std::fs::read(project.join("shorts").join("pts.points.json")).expect("the points"))
            .expect("a list of point maps");
    assert_eq!(points.len() as u64, frames, "one map per frame");
    let spacing = |p: &serde_json::Map<String, Value>| {
        let at = |k: &str| [p[k][0].as_f64().unwrap(), p[k][1].as_f64().unwrap()];
        let (r, l) = (at("eye_right"), at("eye_left"));
        for xy in [r, l, at("mouth"), at("chin")] {
            assert!((0.0..=72.0).contains(&xy[0]) && (0.0..=128.0).contains(&xy[1]), "{xy:?} in the frame");
        }
        assert!(r[0] < l[0], "the right eye is on the picture's left: {p:?}");
        (l[0] - r[0]).hypot(l[1] - r[1])
    };
    let shown: Vec<f64> = points.iter().filter(|p| !p.is_empty()).map(spacing).collect();
    assert!(shown.len() as u64 > frames / 2, "the face shows in most frames: {points:?}");
    let (first, last) = (shown[1], shown[shown.len() - 2]);
    assert!(first > last * 1.3, "the close shot's eyes are farther apart than the bust shot's: {first} and {last}");
}
