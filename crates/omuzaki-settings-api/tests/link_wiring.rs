//! `check_crate!` and `check_app!` from the side of separate libraries: `check_crate!` takes the owning crate's
//! `link()`, calls it, and fails on an empty registry or on a key that clashes with a crate it links;
//! `check_app!` runs over everything an App's `link()` reaches, which is the only place two independent crates meet.
//! Runs real `cargo` on a throwaway workspace, because the point is crates other than the one under test that register
//! settings and own a `link()` (ADR 0015).

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

/// `reg` owns `mascot.size`; `kid` depends on it and owns `mascot.size.max` (a descendant of another crate's key); `idle`
/// has a `link()` and registers nothing. Each has a `tests/settings.rs` with the check, and `reg` also has the test
/// that does what the old no-argument macro let through.
///
/// Then the independent crates, which depend on nothing but the contract: `sib` owns `mascot.size.min` (under `reg`'s
/// key), `twin` owns `mascot.size` (the same key as `reg`), `calm` owns `other.thing`. `bad_app` links `reg`, `sib`
/// and `twin`; `ok_app` links `reg` and `calm`; `idle` doubles as an App that links nothing.
fn workspace(root: &Path) {
    write(
        root,
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"reg\", \"kid\", \"idle\", \"sib\", \"twin\", \"calm\", \"bad_app\", \"ok_app\"]\n",
    );
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
        &[
            ("settings", "omuzaki_settings_api::check_crate!(idle);\n"),
            ("app", "omuzaki_settings_api::check_app!(idle);\n"),
        ],
    );
    for (name, type_name, key) in
        [("sib", "Min", "mascot.size.min"), ("twin", "Twin", "mascot.size"), ("calm", "Calm", "other.thing")]
    {
        library(
            root,
            name,
            &[],
            &format!("{USES}omuzaki_core::define_link!();\n{}", setting(type_name, key)),
            &[("settings", &format!("omuzaki_settings_api::check_crate!({name});\n"))],
        );
    }
    for (name, linked) in [("bad_app", ["reg", "sib", "twin"].as_slice()), ("ok_app", ["reg", "calm"].as_slice())] {
        let calls: String = linked.iter().map(|k| format!("{k}::link(); ")).collect::<Vec<_>>().concat();
        library(
            root,
            name,
            linked,
            &format!("omuzaki_core::define_link! {{ {calls}}}\n"),
            &[("app", &format!("omuzaki_settings_api::check_app!({name});\n"))],
        );
    }
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

    // Sibling crates that never depend on each other each pass the per-crate check: it cannot see the other.
    for sibling in ["sib", "twin", "calm"] {
        let (ok, log) = cargo_test(dir.path(), sibling, "settings");
        assert!(ok, "`{sibling}` is well formed on its own:\n{log}");
    }
    let (ok, log) = cargo_test(dir.path(), "ok_app", "app");
    assert!(ok, "an App whose crates' keys are independent passes:\n{log}");

    // The App sees them all, and reports each clash once, naming both crates.
    let (ok, log) = cargo_test(dir.path(), "bad_app", "app");
    assert!(!ok, "an App that links clashing independent crates must fail:\n{log}");
    for expected in [
        "`mascot.size` is registered by both `reg` and `twin`",
        "`mascot.size` (reg) is a prefix of `mascot.size.min` (sib)",
        "`mascot.size` (twin) is a prefix of `mascot.size.min` (sib)",
    ] {
        assert!(log.contains(expected), "missing `{expected}` in:\n{log}");
    }

    let (ok, log) = cargo_test(dir.path(), "idle", "app");
    assert!(!ok && log.contains("no settings are registered"), "an App that reaches no settings must fail:\n{log}");
}
