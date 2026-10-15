//! `xtask lint-licences` against fixture workspaces: one licence file per dependency, tool and asset, and the register
//! generated from them (ADR 0053 §3).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::fmt::Write as _;
use std::fs;

use support::Fixture;

const REGISTER: &str = "# Licence register\n\nProse stays.\n\n## In use\n\n<!-- licence-table: in-use -->\n";

/// A `Cargo.lock` where the member `app` depends directly on each of `deps` (all from crates.io).
fn lock(deps: &[&str]) -> String {
    let names: Vec<String> = deps.iter().map(|dep| format!("\"{dep}\"")).collect();
    let mut text = format!(
        "version = 4\n\n[[package]]\nname = \"app\"\nversion = \"0.0.0\"\ndependencies = [{}]\n",
        names.join(", ")
    );
    for dep in deps {
        write!(
            text,
            "\n[[package]]\nname = \"{dep}\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n"
        )
        .unwrap();
    }
    text
}

fn licence_file(item: &str, claims: &str) -> String {
    format!(
        "section = \"in-use\"\nitem = \"{item}\"\nkind = \"crate\"\nused_for = \"tests\"\nstatus = \"yes\"\naudit = \"(crates.io: MIT)\"\n{claims}\n"
    )
}

fn workspace() -> Fixture {
    let fixture = Fixture::new().krate("app", &[]);
    fixture.write("Cargo.lock", &lock(&["serde"]));
    fixture.write("docs/licensing/register.md", REGISTER);
    fixture.write("docs/licensing/deps/serde.toml", &licence_file("serde", "crates = [\"serde\"]"));
    fixture
}

#[test]
fn adding_a_dependency_needs_only_its_own_file_and_changes_the_generated_register() {
    let fixture = workspace();
    let before = fixture.xtask(&["lint-licences", "--print"]);
    assert!(before.success, "{}{}", before.stdout, before.stderr);
    assert!(before.stdout.starts_with("# Licence register\n\nProse stays.\n"), "{}", before.stdout);
    assert!(before.stdout.contains("| serde | crate | tests | yes | | (crates.io: MIT) |\n"), "{}", before.stdout);
    let shared = |name: &str| fs::read(fixture.root().join(name)).unwrap();
    let (register, serde) = (shared("docs/licensing/register.md"), shared("docs/licensing/deps/serde.toml"));

    fixture.write("Cargo.lock", &lock(&["serde", "toml"]));
    let missing = fixture.xtask(&["lint-licences"]);
    assert!(!missing.success);
    assert!(
        missing.stdout.contains("crate `toml` (used by app) has no file in docs/licensing/deps/"),
        "{}",
        missing.stdout
    );

    fixture.write("docs/licensing/deps/toml.toml", &licence_file("toml", "crates = [\"toml\"]"));
    let after = fixture.xtask(&["lint-licences", "--print"]);
    assert!(after.success, "{}{}", after.stdout, after.stderr);
    let added = "| toml | crate | tests | yes | | (crates.io: MIT) |\n";
    assert_eq!(after.stdout.replace(added, ""), before.stdout, "the new row is the only change");
    assert_eq!(shared("docs/licensing/register.md"), register, "no shared file was edited");
    assert_eq!(shared("docs/licensing/deps/serde.toml"), serde);
}

#[test]
fn orphaned_files_and_uncovered_tools_and_assets_are_caught() {
    let fixture = workspace();
    fixture.write("docs/licensing/deps/gone.toml", &licence_file("gone", "crates = [\"gone\"]\npaths = [\"fonts/\"]"));
    fixture.write(".github/workflows/ci.yml", "    with:\n      tools: cargo-deny,cargo-about@0.9.2\n");
    fixture.write("docs/licensing/deps/cargo-deny.toml", &licence_file("cargo-deny", "tools = [\"cargo-deny\"]"));
    fixture.write("crates/app/assets/Font.ttf", "");
    let run = fixture.xtask(&["lint-licences"]);
    assert!(!run.success, "{}", run.stdout);
    for expected in [
        "docs/licensing/deps/gone.toml: orphaned: crate `gone` is not in Cargo.lock",
        "docs/licensing/deps/gone.toml: orphaned: path `fonts/` does not exist",
        "tool `cargo-about` (installed by a workflow) has no file in docs/licensing/deps/",
        "`crates/app/assets/Font.ttf` has no file in docs/licensing/deps/ (list it under `paths`)",
        "lint-licences: 4 problem(s)",
    ] {
        assert!(run.stdout.contains(expected), "missing `{expected}` in:\n{}", run.stdout);
    }
}

#[test]
fn a_file_that_does_not_fit_the_register_is_caught() {
    let fixture = workspace();
    fixture.write("docs/licensing/deps/serde.toml", &licence_file("serde", "crates = [\"serde\"]\nlicence = \"MIT\""));
    fixture.write(
        "docs/licensing/deps/odd.toml",
        &licence_file("odd", "").replace("in-use", "elsewhere").replace("\"yes\"", "\"maybe\""),
    );
    let run = fixture.xtask(&["lint-licences"]);
    assert!(!run.success, "{}", run.stdout);
    for expected in [
        "docs/licensing/deps/serde.toml: cannot read: unknown field `licence`",
        "docs/licensing/deps/odd.toml: status `maybe` is not one of yes, conditions, external, NO, unverified, private-test",
        "docs/licensing/deps/odd.toml: section `elsewhere` has no `<!-- licence-table: elsewhere -->`",
        "crate `serde` (used by app) has no file",
    ] {
        assert!(run.stdout.contains(expected), "missing `{expected}` in:\n{}", run.stdout);
    }
}
