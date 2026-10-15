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

    let commented = workspace();
    commented.write(
        "crates/omuzaki-cli/src/lib.rs",
        "omuzaki_core::define_link! {\n    // include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n",
    );
    let run = commented.xtask(&["lint-links"]);
    assert!(!run.success, "a commented-out include is not wiring: {}", run.stdout);

    let conditional = workspace();
    conditional.write(
        "crates/omuzaki-cli/src/lib.rs",
        "omuzaki_core::define_link! {\n    #[cfg(any())]\n    include!(concat!(env!(\"OUT_DIR\"), \"/link_calls.rs\"));\n}\n",
    );
    let run = conditional.xtask(&["lint-links"]);
    assert!(!run.success, "a cfg-disabled include is not wiring: {}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-cli: lists `omuzaki-link-build`, but its generated link list is not wired"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls"), "{}", run.stdout);

    let unused_emit = workspace();
    unused_emit
        .write("crates/omuzaki-cli/build.rs", "fn main() {}\nfn unused() {\n    omuzaki_link_build::emit();\n}\n");
    let run = unused_emit.xtask(&["lint-links"]);
    assert!(!run.success, "an emit() nobody calls is not wiring: {}", run.stdout);

    let disabled_main = workspace();
    disabled_main.write(
        "crates/omuzaki-cli/build.rs",
        "#[cfg(any())]\nfn main() {\n    omuzaki_link_build::emit();\n}\nfn main() {}\n",
    );
    let run = disabled_main.xtask(&["lint-links"]);
    assert!(!run.success, "a cfg-disabled main is not wiring: {}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-cli: lists `omuzaki-link-build`, but its generated link list is not wired"),
        "{}",
        run.stdout
    );

    let no_include = workspace();
    no_include.write("crates/omuzaki-cli/src/lib.rs", "omuzaki_core::define_link!();\n");
    let run = no_include.xtask(&["lint-links"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls"), "{}", run.stdout);
}

/// The same App, but its dependencies are inherited from `[workspace.dependencies]`.
fn inheriting_workspace() -> Fixture {
    let fixture = workspace();
    fixture.write(
        "Cargo.toml",
        "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n[workspace.dependencies]\nomuzaki-char = { path = \"crates/omuzaki-char\" }\n",
    );
    let path = "crates/omuzaki-cli/Cargo.toml";
    let manifest = std::fs::read_to_string(fixture.root().join(path)).unwrap();
    fixture.write(
        path,
        &manifest.replace("omuzaki-char = { path = \"../omuzaki-char\" }", "omuzaki-char.workspace = true"),
    );
    fixture
}

#[test]
fn a_workspace_inherited_registering_dependency_is_generated_and_seen_by_the_gate() {
    let fixture = inheriting_workspace();
    assert!(generated(&fixture).contains("    omuzaki_char::link();\n"), "{}", generated(&fixture));
    let run = fixture.xtask(&["lint-links"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);

    // With the wiring broken the scanner must still see the dependency, so the gate names the missing call.
    fixture.write("crates/omuzaki-cli/build.rs", "fn main() {}\n");
    let run = fixture.xtask(&["lint-links"]);
    assert!(!run.success, "an inherited dependency must not hide from the scan: {}", run.stdout);
    assert!(run.stdout.contains("omuzaki-cli → omuzaki-char: never calls"), "{}", run.stdout);
}

#[test]
fn an_inherited_dependency_the_workspace_does_not_define_fails_the_scan_loudly() {
    let fixture = inheriting_workspace();
    fixture.write("Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\n");
    // Cargo itself rejects this workspace; the scan used by the Apps' link gates (which never run cargo) must too.
    let error = xtask::registry::Sources::scan(fixture.root()).unwrap_err().to_string();
    assert!(error.contains("has no `omuzaki-char`"), "{error}");
}
