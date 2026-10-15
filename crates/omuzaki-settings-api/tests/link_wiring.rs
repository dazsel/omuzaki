//! `check_crate!` from the side of a separate library: it takes the owning crate's `link()`, calls it, and fails on an
//! empty registry or on a key that clashes with another crate's. Runs real `cargo` on a throwaway workspace, because
//! the point is a crate other than the one under test that registers settings and owns a `link()` (ADR 0015).

// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

/// A library crate `name` that depends on this contract crate, on `extra` (other fixture crates) and defines `lib_rs`,
/// with `tests/settings.rs` holding `check`.
fn library(root: &Path, name: &str, extra: &[&str], lib_rs: &str, tests: &[(&str, &str)]) {
    let here = Path::new(env!("CARGO_MANIFEST_DIR")).display().to_string().replace('\\', "/");
    let core = format!("{here}/../omuzaki-core");
    let extra = extra.iter().map(|e| format!("{e} = {{ path = \"../{e}\" }}\n")).collect::<Vec<_>>().concat();
    write(
        root,
        &format!("{name}/Cargo.toml"),
        &format!(
            "[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
             omuzaki-settings-api = {{ path = \"{here}\" }}\nomuzaki-core = {{ path = \"{core}\" }}\n{extra}"
        ),
    );
    write(root, &format!("{name}/src/lib.rs"), lib_rs);
    for (file, text) in tests {
        write(root, &format!("{name}/tests/{file}.rs"), text);
    }
}

/// One setting called `key` in a type called `Type`.
fn setting(type_name: &str, key: &str) -> String {
    format!(
        "pub struct {type_name};\n\
         impl Setting for {type_name} {{\n\
         \x20   type Value = u8;\n\
         \x20   const META: SettingMeta = SettingMeta::new(\"{key}\", \"A fixture key\", SettingScope::User, \
         AgentWrite::Free, InProfile::Member, Apply::Live, 1);\n\
         \x20   fn default() -> u8 {{ 1 }}\n\
         }}\n\
         register_setting!({type_name});\n"
    )
}

const USES: &str = "use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};\n\
                    use omuzaki_settings_api::register_setting;\nuse omuzaki_settings_api::setting::Setting;\n";

/// `reg` owns `mascot.size`; `kid` owns `mascot.size.max` (a descendant of another crate's key); `idle` has a `link()`
/// and registers nothing. Each has a `tests/settings.rs` with the check, and `reg` also has the test that does what
/// the old no-argument macro let through.
fn workspace(root: &Path) {
    write(root, "Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"reg\", \"kid\", \"idle\"]\n");
    // The same lockfile as the repository, so an offline build resolves from the cache.
    fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.lock"), root.join("Cargo.lock")).unwrap();
    library(
        root,
        "reg",
        &[],
        &format!("{USES}omuzaki_core::define_link!();\n{}", setting("Size", "mascot.size")),
        &[
            ("settings", "omuzaki_settings_api::check_crate!(reg);\n"),
            (
                // Nothing in this test binary refers to `reg`, so nothing of it is linked: the shape of a crate whose
                // `link()` was never called. The check must say so instead of passing on an empty registry.
                "unlinked",
                "#[test]\n#[should_panic(expected = \"`reg` registers no settings\")]\nfn an_unlinked_crate_is_not_a_pass() {\n\
                 \x20   omuzaki_settings_api::check::assert_crate(\"reg\", || {});\n}\n",
            ),
        ],
    );
    library(
        root,
        "kid",
        &["reg"],
        &format!("{USES}omuzaki_core::define_link! {{ reg::link(); }}\n{}", setting("Max", "mascot.size.max")),
        &[("settings", "omuzaki_settings_api::check_crate!(kid);\n")],
    );
    library(
        root,
        "idle",
        &[],
        "omuzaki_core::define_link!();\n",
        &[("settings", "omuzaki_settings_api::check_crate!(idle);\n")],
    );
}

fn cargo_test(root: &Path, package: &str, test: &str) -> (bool, String) {
    let output = Command::new(env!("CARGO"))
        .current_dir(root)
        .env("CARGO_TARGET_DIR", root.join("target"))
        .args(["test", "--offline", "--quiet", "-p", package, "--test", test])
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).into_owned() + &String::from_utf8_lossy(&output.stdout),
    )
}

#[test]
fn the_check_follows_the_owning_crates_link_and_refuses_an_empty_or_clashing_registry() {
    let dir = tempfile::tempdir().unwrap();
    workspace(dir.path());

    let (ok, log) = cargo_test(dir.path(), "reg", "settings");
    assert!(ok, "a crate that registers well-formed keys passes through its own link():\n{log}");

    let (ok, log) = cargo_test(dir.path(), "reg", "unlinked");
    assert!(ok, "a no-op link() in a binary that never linked the crate sees nothing, and says so:\n{log}");

    let (ok, log) = cargo_test(dir.path(), "idle", "settings");
    assert!(!ok && log.contains("`idle` registers no settings"), "an empty registry must fail:\n{log}");

    let (ok, log) = cargo_test(dir.path(), "kid", "settings");
    assert!(
        !ok && log.contains("mascot.size.max: is under `mascot.size`, which is a setting itself"),
        "a key under another crate's key must fail, found through the forwarded link():\n{log}"
    );
}
