//! A crate added to an App edits nothing shared (ADR 0054 section 6): the App's generated `link()` list picks up a
//! registering dependency from the dependency line alone, and `lint-links` accepts the result.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Section::Normal};

const OP_MACRO: &str = "#[macro_export]\nmacro_rules! register_op { ($ty:ty) => {}; }\n";
const LINK: &str = "omuzaki_core::define_link!();\n";
const FLAG: &str = "\n[package.metadata.omuzaki]\nregisters = true\n";

fn append(fixture: &Fixture, krate: &str, text: &str) {
    let path = format!("crates/{krate}/Cargo.toml");
    let manifest = std::fs::read_to_string(fixture.root().join(&path)).unwrap();
    fixture.write(&path, &(manifest + text));
}

/// Adds `line` under `header` in the manifest of fixture crate `krate`.
fn add_under(fixture: &Fixture, krate: &str, header: &str, line: &str) {
    let path = format!("crates/{krate}/Cargo.toml");
    let manifest = std::fs::read_to_string(fixture.root().join(&path)).unwrap();
    fixture.write(&path, &manifest.replacen(&format!("[{header}]\n"), &format!("[{header}]\n{line}\n"), 1));
}

/// A contract, one registering crate, and an App whose link list is generated and whose sources name no crate.
fn workspace() -> Fixture {
    let fixture = Fixture::new()
        .krate("omuzaki-ops-api", &[])
        .krate("omuzaki-link-build", &[])
        .krate("omuzaki-char", &[("omuzaki-ops-api", Normal)])
        .krate("omuzaki-cli", &[("omuzaki-char", Normal)]);
    fixture.write("crates/omuzaki-ops-api/src/lib.rs", OP_MACRO);
    fixture.write("crates/omuzaki-char/src/lib.rs", &format!("omuzaki_ops_api::register_op!(Create);\n{LINK}"));
    append(&fixture, "omuzaki-char", FLAG);
    fixture.write(
        "crates/omuzaki-cli/src/lib.rs",
        "omuzaki_core::define_link! {\n    include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n",
    );
    fixture.write("crates/omuzaki-cli/build.rs", "fn main() {\n    omuzaki_link_build::emit();\n}\n");
    add_under(
        &fixture,
        "omuzaki-cli",
        "build-dependencies",
        "omuzaki-link-build = { path = \"../omuzaki-link-build\" }",
    );
    fixture
}

fn generated(fixture: &Fixture) -> String {
    omuzaki_link_build::generate(&fixture.root().join("crates/omuzaki-cli")).unwrap().source
}

#[test]
fn a_registering_crate_added_to_an_app_is_linked_from_the_dependency_line_alone() {
    let fixture = workspace();
    assert!(generated(&fixture).contains("    omuzaki_char::link();\n"), "{}", generated(&fixture));
    let before = fixture.xtask(&["lint-links"]);
    assert!(before.success, "{}{}", before.stdout, before.stderr);

    // A new crate that registers, and one dependency line in the App: nothing else is written.
    let fixture = fixture.krate("omuzaki-anim", &[("omuzaki-ops-api", Normal)]);
    fixture.write("crates/omuzaki-anim/src/lib.rs", &format!("omuzaki_ops_api::register_op!(Blink);\n{LINK}"));
    append(&fixture, "omuzaki-anim", FLAG);
    add_under(&fixture, "omuzaki-cli", "dependencies", "omuzaki-anim = { path = \"../omuzaki-anim\" }");

    let source = generated(&fixture);
    assert!(source.contains("    omuzaki_anim::link();\n    omuzaki_char::link();\n"), "{source}");
    let after = fixture.xtask(&["lint-links"]);
    assert!(after.success, "{}{}", after.stdout, after.stderr);
}

#[test]
fn a_registering_dependency_without_the_flag_is_not_generated_and_lint_links_says_so() {
    let fixture = workspace();
    let manifest = std::fs::read_to_string(fixture.root().join("crates/omuzaki-char/Cargo.toml")).unwrap();
    fixture.write("crates/omuzaki-char/Cargo.toml", &manifest.replace(FLAG, ""));
    assert!(!generated(&fixture).contains("omuzaki_char"), "{}", generated(&fixture));
    let run = fixture.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls `omuzaki_char::link()`"), "{}", run.stdout);
}

#[test]
fn listing_the_generator_is_not_enough_the_build_script_must_call_it_and_the_library_include_its_output() {
    let no_emit = workspace();
    no_emit.write("crates/omuzaki-cli/build.rs", "fn main() {}\n");
    let run = no_emit.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls"), "{}", run.stdout);

    let no_include = workspace();
    no_include.write("crates/omuzaki-cli/src/lib.rs", "omuzaki_core::define_link!();\n");
    let run = no_include.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls"), "{}", run.stdout);
}
