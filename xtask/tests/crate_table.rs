//! `xtask docs` and `xtask lint-crate-table` against fixture workspaces: the crate table comes from each crate's own
//! manifest (ADR 0053 §3).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Section::Normal};

const RULES: &str = "# Rust workspace\n\n| Kind | Names |\n|---|---|\n| **API** | `omuzaki-core`, `*-api` |\n";

fn described() -> Fixture {
    let fixture = Fixture::new()
        .krate("omuzaki-core", &[])
        .describe("omuzaki-core", "API", "ids and `glam`")
        .krate("omuzaki-doc", &[("omuzaki-core", Normal)])
        .describe("omuzaki-doc", "Implementation", "the project store");
    fixture.write("crates/README.md", RULES);
    fixture
}

#[test]
fn adding_a_crate_changes_the_table_without_editing_a_shared_file() {
    let before = described();
    let first = before.xtask(&["docs"]);
    assert!(first.success, "{}", first.stderr);
    assert_eq!(
        first.stdout,
        "| Crate | Kind | What it is |\n|---|---|---|\n\
         | `omuzaki-core` | API | ids and `glam` |\n| `omuzaki-doc` | Implementation | the project store |\n"
    );

    // Only the new crate's own directory is written: no README, no workspace manifest, no list.
    let after = before.krate("omuzaki-budget", &[("omuzaki-core", Normal)]).describe(
        "omuzaki-budget",
        "Implementation",
        "the frame-budget scheduler",
    );
    let second = after.xtask(&["docs"]);
    assert!(second.success, "{}", second.stderr);
    let added: Vec<&str> = second.stdout.lines().filter(|line| !first.stdout.lines().any(|old| old == *line)).collect();
    assert_eq!(added, ["| `omuzaki-budget` | Implementation | the frame-budget scheduler |"]);
    assert_eq!(second.stdout.lines().count(), first.stdout.lines().count() + 1);

    let lint = after.xtask(&["lint-crate-table"]);
    assert!(lint.success, "{}", lint.stdout);
    assert!(lint.stdout.contains("lint-crate-table: ok (3 crates)"), "{}", lint.stdout);
}

#[test]
fn a_crate_without_metadata_and_a_table_in_the_readme_fail_the_lint() {
    let fixture = described().krate("omuzaki-rig", &[("omuzaki-core", Normal)]);
    fixture.write("crates/README.md", &format!("{RULES}\n| Crate | Kind |\n|---|---|\n| `omuzaki-rig` | x |\n"));
    let run = fixture.xtask(&["lint-crate-table"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-rig: no `kind` under [package.metadata.omuzaki]"), "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-rig: no `summary` under [package.metadata.omuzaki]"), "{}", run.stdout);
    assert!(run.stdout.contains("crates/README.md:9: a crate table row"), "{}", run.stdout);
    assert!(run.stdout.contains("3 violation(s)"), "{}", run.stdout);
}
