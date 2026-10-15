//! `xtask lint-registers` against fixture workspaces: a crate with a `register_*!` call must set `registers = true`
//! (ADR 0054 section 6).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Section::Normal};

const OP_MACRO: &str = "#[macro_export]\nmacro_rules! register_op { ($ty:ty) => {}; }\n";
const FLAG: &str = "\n[package.metadata.omuzaki]\nregisters = true\n";

/// A contract with the macro, and a crate that makes one registration, with or without the flag.
fn workspace(flagged: bool) -> Fixture {
    let fixture = Fixture::new().krate("omuzaki-ops-api", &[]).krate("omuzaki-char", &[("omuzaki-ops-api", Normal)]);
    fixture.write("crates/omuzaki-ops-api/src/lib.rs", OP_MACRO);
    fixture.write("crates/omuzaki-char/src/lib.rs", "omuzaki_ops_api::register_op!(Create);\n");
    if flagged {
        let manifest = std::fs::read_to_string(fixture.root().join("crates/omuzaki-char/Cargo.toml")).unwrap();
        fixture.write("crates/omuzaki-char/Cargo.toml", &(manifest + FLAG));
    }
    fixture
}

#[test]
fn a_registering_crate_with_the_flag_passes() {
    let run = workspace(true).xtask(&["lint-registers"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("lint-registers: ok (2 crates)"), "{}", run.stdout);
}

#[test]
fn a_registering_crate_without_the_flag_fails_and_is_named_with_its_first_registration() {
    let run = workspace(false).xtask(&["lint-registers"]);
    assert!(!run.success, "{}", run.stdout);
    let line = run.stdout.lines().next().unwrap();
    assert!(
        line.starts_with("omuzaki-char: makes 1 registration(s) (the first is register_op!(Create) at crates/omuzaki-char/src/lib.rs:1)"),
        "{line}"
    );
    assert!(run.stdout.contains("lint-registers: 1 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_crate_that_registers_nothing_needs_no_flag_and_registrations_in_tests_do_not_count() {
    let fixture = workspace(true).krate("omuzaki-doc", &[("omuzaki-ops-api", Normal)]);
    fixture.write("crates/omuzaki-doc/tests/ops.rs", "omuzaki_ops_api::register_op!(TestOnly);\n");
    let run = fixture.xtask(&["lint-registers"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_registration_behind_another_targets_cfg_still_needs_the_flag() {
    for gate in ["target_arch = \"wasm32\"", "windows", "not(unix)", "target_os = \"macos\""] {
        let fixture = workspace(false);
        fixture.write(
            "crates/omuzaki-char/src/lib.rs",
            &format!("#[cfg({gate})]\nomuzaki_ops_api::register_op!(Create);\n"),
        );
        let run = fixture.xtask(&["lint-registers"]);
        assert!(!run.success, "cfg({gate}): {}", run.stdout);
        assert!(run.stdout.contains("omuzaki-char: makes 1 registration(s)"), "cfg({gate}): {}", run.stdout);
    }
}

#[test]
fn test_only_registrations_never_need_the_flag_on_any_target() {
    let fixture = workspace(false);
    fixture.write(
        "crates/omuzaki-char/src/lib.rs",
        "#[cfg(test)]\nomuzaki_ops_api::register_op!(A);\n#[cfg(all(test, target_arch = \"wasm32\"))]\nomuzaki_ops_api::register_op!(B);\n",
    );
    let run = fixture.xtask(&["lint-registers"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_crate_that_registers_only_on_another_target_still_needs_a_link() {
    let fixture = workspace(true);
    fixture.write(
        "crates/omuzaki-char/src/lib.rs",
        "#[cfg(target_arch = \"wasm32\")]\nomuzaki_ops_api::register_op!(Create);\n",
    );
    let run = fixture.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-char: defines no `link()`"), "{}", run.stdout);
}

/// An App with an optional dependency on the registering crate, activated by a feature written as `feature_entry`.
fn optional_app(feature_entry: &str) -> Fixture {
    let fixture = workspace(true).krate("omuzaki-stage", &[]);
    let manifest = "[package]\nname = \"omuzaki-stage\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\
        [dependencies]\nomuzaki-char = { path = \"../omuzaki-char\", optional = true }\n\
        [features]\nwgpu = [\""
        .to_owned()
        + feature_entry
        + "\"]\n";
    fixture.write("crates/omuzaki-stage/Cargo.toml", &manifest);
    fixture
}

#[test]
fn dep_syntax_on_an_optional_registering_dependency_fails() {
    let run = optional_app("dep:omuzaki-char").xtask(&["lint-registers"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-stage: a feature lists `dep:omuzaki-char`, but `omuzaki-char` registers"),
        "{}",
        run.stdout
    );
}

#[test]
fn the_implicit_feature_on_an_optional_registering_dependency_passes() {
    let run = optional_app("omuzaki-char").xtask(&["lint-registers"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn the_gates_expected_set_includes_an_optional_registering_dependency_when_its_feature_is_on() {
    let fixture = optional_app("omuzaki-char");
    fixture.write("crates/omuzaki-stage/src/lib.rs", "");
    let off = xtask::registry::expected_with_features(fixture.root(), "omuzaki-stage", None, &[]).unwrap();
    assert!(off.is_empty(), "{off:?}");
    let on = xtask::registry::expected_with_features(fixture.root(), "omuzaki-stage", None, &["omuzaki-char"]).unwrap();
    assert_eq!(on.len(), 1, "{on:?}");
    assert_eq!((on[0].package.as_str(), on[0].name.as_str()), ("omuzaki-char", "Create"));
}
