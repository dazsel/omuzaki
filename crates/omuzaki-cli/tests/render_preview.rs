//! A headless render as a script sees it: `omuzaki run render.preview` renders a kit character on this machine's GPU,
//! stores the PNG in the person's asset store and prints its address, and `omuzaki store export` writes it to a file.
//! Skips where no GPU adapter exists (lavapipe is one, in CI and the dev VMs).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::disallowed_methods, reason = "tests set up their fixtures on disk; the fence is for ops")]

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

/// `omuzaki <args>` with its data and config folders in `home`, so the asset store is the test's own.
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

/// The adapter the binary opens is the default one; a machine without any skips.
fn has_gpu() -> bool {
    omuzaki_render::WgpuRenderer::headless(&omuzaki_render::HeadlessOptions::default()).is_ok()
}

#[test]
fn a_preview_rendered_from_the_shell_exports_as_a_png_of_its_size() {
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
    let preview = ok(
        home.path(),
        &[
            "--project",
            project_arg,
            "--json",
            "run",
            "render.preview",
            "--args",
            r#"{"character":"lumi","framing":"face","size":[64,96]}"#,
        ],
    );
    let preview: Value = serde_json::from_str(&preview).expect("JSON on stdout");
    let uri = preview["plan"]["output"]["uri"].as_str().unwrap_or_else(|| panic!("no uri in {preview:#}"));
    let hash = uri.strip_prefix("omuzaki://render/").and_then(|rest| rest.strip_suffix(".png"));
    let hash = hash.unwrap_or_else(|| panic!("{uri} is not a render's address"));
    assert_eq!(hash.len(), 64, "{uri}");

    let file = home.path().join("lumi.png");
    let exported = ok(home.path(), &["store", "export", uri, file.to_str().expect("utf-8 path")]);
    assert!(exported.starts_with("wrote "), "{exported}");
    let png = std::fs::read(&file).expect("the PNG was written");
    let image = image::load_from_memory_with_format(&png, image::ImageFormat::Png).expect("a PNG");
    assert_eq!((image.width(), image.height()), (64, 96));

    // The same object from a store named outright, and its hash alone, give the same bytes.
    let again = home.path().join("again.png");
    let store = home.path().join("data").join("store");
    ok(
        home.path(),
        &["store", "export", hash, again.to_str().expect("utf-8 path"), "--store", store.to_str().expect("utf-8 path")],
    );
    assert_eq!(std::fs::read(&again).expect("written"), png);
}

#[test]
fn exporting_an_object_the_store_lacks_is_not_found() {
    let home = tempfile::tempdir().expect("temp dir");
    std::fs::create_dir_all(home.path().join("data").join("store").join("objects")).expect("an empty store");
    let missing = omuzaki_store_api::ContentHash::of(b"never rendered");
    let file = home.path().join("x.png");
    let out = omuzaki(
        home.path(),
        &["store", "export", &format!("omuzaki://render/{missing}.png"), file.to_str().expect("utf-8 path")],
    );
    assert_eq!(out.status.code(), Some(4), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(String::from_utf8_lossy(&out.stderr).contains(&missing.to_string()));
    assert!(!file.exists(), "nothing is written for an object that isn't there");
}
