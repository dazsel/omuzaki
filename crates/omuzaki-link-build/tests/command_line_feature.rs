//! A registering optional dependency switched on from the command line (`--features <name>/<feature>`) must still get
//! its generated `link()` call: the call is gated on the dependency's implicit feature, which Cargo turns on whenever
//! the dependency is active. Runs real `cargo` on a throwaway workspace.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// `reg` registers (it has a flag and records that its `link()` ran); `app` depends on it optionally and its `link()`
/// is generated. `reg_feature` is how `app` exposes `reg`'s activation, if at all.
fn workspace(root: &Path, app_features: &str) {
    let link_build = Path::new(env!("CARGO_MANIFEST_DIR")).display().to_string().replace('\\', "/");
    write(root, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"reg\", \"app\"]\n");
    // The same lockfile as the repository, so an offline build resolves the helper's dependencies from the cache.
    fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"), root.join("Cargo.lock")).unwrap();
    write(
        root,
        "reg/Cargo.toml",
        "[package]\nname = \"reg\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[features]\nextra = []\n\
         [package.metadata.omuzaki]\nregisters = true\n",
    );
    write(
        root,
        "reg/src/lib.rs",
        "use std::sync::atomic::{AtomicBool, Ordering};\nstatic LINKED: AtomicBool = AtomicBool::new(false);\n\
         pub fn link() { LINKED.store(true, Ordering::SeqCst); }\n\
         pub fn linked() -> bool { LINKED.load(Ordering::SeqCst) }\n",
    );
    write(
        root,
        "app/Cargo.toml",
        &format!(
            "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
             reg = {{ path = \"../reg\", optional = true }}\n{app_features}\n\
             [build-dependencies]\nomuzaki-link-build = {{ path = \"{link_build}\" }}\n"
        ),
    );
    write(root, "app/build.rs", "fn main() {\n    omuzaki_link_build::emit();\n}\n");
    write(
        root,
        "app/src/lib.rs",
        "pub const GENERATED: &str = include_str!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n\
         pub fn link() {\n    include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n",
    );
    write(
        root,
        "app/tests/linked.rs",
        "#[test]\nfn the_dependency_is_linked_exactly_when_it_is_active() {\n    println!(\"GENERATED<<{}>>\", app::GENERATED);\n    app::link();\n    #[cfg(feature = \"reg\")]\n    assert!(reg::linked(), \"reg is active but its link() was not called\");\n}\n",
    );
}

fn cargo_test(root: &Path, features: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO"))
        .current_dir(root.join("app"))
        .env("CARGO_TARGET_DIR", root.join("target"))
        .args(["test", "--offline", "--quiet"])
        .args(features)
        .args(["--", "--nocapture"])
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned() + &String::from_utf8_lossy(&output.stdout),
    )
}

#[test]
fn a_dependency_switched_on_by_name_slash_feature_on_the_command_line_is_linked() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path(), "[features]\ngpu = [\"reg\"]\n");
    for features in [&["--features", "reg/extra"][..], &["--features", "gpu"], &["--features", "reg"], &[]] {
        let (ok, log) = cargo_test(dir.path(), features);
        assert!(ok, "cargo test {features:?}:\n{log}");
    }
}

#[test]
fn dep_syntax_fails_the_build_with_a_message_that_names_the_fix() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path(), "[features]\ngpu = [\"dep:reg\"]\n");
    let (ok, log) = cargo_test(dir.path(), &["--features", "reg/extra"]);
    assert!(!ok, "{log}");
    assert!(log.contains("`dep:reg`") && log.contains("List `\"reg\"` instead"), "{log}");
}

const FLAGGED: &str = "[package]\nname = \"reg\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[features]\nextra = []\n\
                       [package.metadata.omuzaki]\nregisters = true\n";

#[test]
fn an_incremental_build_regenerates_the_list_when_a_dependency_gains_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path(), "[features]\ngpu = [\"reg\"]\n");
    write(dir.path(), "reg/Cargo.toml", &FLAGGED.replace("registers = true", "registers = false"));
    // The fixture's own test fails here (the dependency is active but unflagged, so it is not linked); only the
    // generated list it prints matters.
    let (_, before) = cargo_test(dir.path(), &["--features", "gpu"]);
    assert!(before.contains("GENERATED<<") && !before.contains("reg::link"), "unflagged: {before}");

    // Same target directory: only the dependency's manifest changed, and the build script must notice.
    write(dir.path(), "reg/Cargo.toml", FLAGGED);
    let (ok, after) = cargo_test(dir.path(), &["--features", "gpu"]);
    assert!(ok, "{after}");
    assert!(after.contains("reg::link"), "the flag was added but the list is stale: {after}");
}
