//! Guard: every GPU test, bench and example opens its wgpu instance through the shared helper, never wgpu's default.
//!
//! Why: wgpu's default starts every backend, GL included. On WSL with NVIDIA's D3D12 driver, starting GL loads that
//! driver through Mesa's EGL, and a thread-exit destructor it registers outlives the library, so the process dies with
//! SIGSEGV at exit after every test has passed. The product opens `Backends::PRIMARY`; the helpers do the same and let
//! `WGPU_BACKEND` opt in to GL. This test reads every crate's Rust sources and fails on `Backends::all()` or a
//! default instance descriptor outside the allowed files.
//!
//! Limits: it is textual (comments are skipped, nothing else is parsed), so a spelling it does not list can slip past.

// Helpers sit outside `#[test]` functions, where clippy.toml's unwrap exemption doesn't reach.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};

/// The files that may build an instance themselves, relative to the workspace root: the product's opener and the
/// three test-support helpers, one per crate (impl crates may not depend on the App crate `omuzaki-test`, ADR 0054).
const ALLOWED: &[&str] = &[
    "crates/omuzaki-render/src/gpu.rs",
    "crates/omuzaki-render/tests/support/mod.rs",
    "crates/omuzaki-face/tests/support/gpu_instance.rs",
    "crates/omuzaki-test-face/tests/common/mod.rs",
    // The GL opt-in regression builds a GL-only instance on purpose, and only when `WGPU_BACKEND=gl` is set.
    "crates/omuzaki-render/tests/gl_opt_in.rs",
];

/// The spellings that start every backend: wgpu's default, which includes GL.
const FORBIDDEN: &[&str] = &[
    "Backends::all()",
    "InstanceDescriptor::default(",
    "Instance::default(",
    "new_without_display_handle()",
    "_from_env(",
];

/// `Backends::all()` as the argument of `enumerate_adapters(` is only a filter over the backends the instance already
/// started, so it is fine; it must stay `all()` so that a `WGPU_BACKEND=gl` opt-in still finds its adapters.
fn is_enumeration(line: &str, pattern: &str) -> bool {
    pattern == "Backends::all()" && line.contains("enumerate_adapters(wgpu::Backends::all())")
}

/// What is wrong in one file's `source`, as `file:line: message` strings.
fn violations(file: &str, source: &str) -> Vec<String> {
    if ALLOWED.contains(&file) {
        return Vec::new();
    }
    source
        .lines()
        .enumerate()
        .filter(|(_, line)| !line.trim_start().starts_with("//"))
        .flat_map(|(i, line)| {
            FORBIDDEN.iter().filter(|p| line.contains(**p) && !is_enumeration(line, p)).map(move |p| {
                format!(
                    "{file}:{}: `{p}` starts every wgpu backend, GL included, which crashes the process at exit on WSL \
                     with NVIDIA's D3D12 driver; open the instance through the shared helper \
                     (`gpu_instance::instance()` or `support::instance()`), which uses `Backends::PRIMARY`",
                    i + 1
                )
            })
        })
        .collect()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n != "target") {
                rust_files(&path, out);
            }
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_source_in_the_workspace_opens_a_default_wgpu_instance() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut files = Vec::new();
    rust_files(&root.join("crates"), &mut files);
    rust_files(&root.join("xtask"), &mut files);
    assert!(files.len() > 100, "found only {} sources: is the scan looking in the right place?", files.len());
    let mut found = Vec::new();
    for path in files {
        let relative = path.strip_prefix(&root).unwrap().to_string_lossy().replace('\\', "/");
        // This file names the spellings it forbids.
        if relative.ends_with("omuzaki-test/tests/gpu_instances.rs") {
            continue;
        }
        found.extend(violations(&relative, &fs::read_to_string(&path).unwrap()));
    }
    assert!(found.is_empty(), "\n{}\n", found.join("\n"));
}

#[test]
fn a_default_instance_fails_with_file_line_and_the_reason() {
    let source =
        "fn f() {\n    let i = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());\n}\n";
    let found = violations("crates/x/tests/t.rs", source);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].starts_with("crates/x/tests/t.rs:2:"), "{found:?}");
    assert!(found[0].contains("crashes the process at exit on WSL"), "{found:?}");
    assert_eq!(violations("crates/x/tests/t.rs", "let a = wgpu::Backends::all();").len(), 1);
    assert_eq!(violations("crates/x/tests/t.rs", "i.enumerate_adapters(wgpu::Backends::all())").len(), 0);
    assert_eq!(violations("crates/x/tests/t.rs", "InstanceDescriptor::new_without_display_handle_from_env()").len(), 1);
}

#[test]
fn comments_and_the_allowed_helpers_pass() {
    assert_eq!(violations("crates/x/tests/t.rs", "// not Backends::all()\n").len(), 0);
    assert_eq!(violations(ALLOWED[2], "InstanceDescriptor::new_without_display_handle()").len(), 0);
}
