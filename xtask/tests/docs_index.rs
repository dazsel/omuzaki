//! `xtask docs-index` and `xtask lint-docs-index` against fixture workspaces: the docs index comes from each doc's own
//! front matter (ADR 0053 §3).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::Fixture;

const INTRO: &str = "# Docs index\n\nThe index is generated.\n";

fn front_matter(title: &str, summary: &str, area: &str) -> String {
    format!("---\ntitle: \"{title}\"\nsummary: \"{summary}\"\narea: {area}\n---\n\n# {title}\n")
}

fn documented() -> Fixture {
    let fixture = Fixture::new().krate("omuzaki-core", &[]);
    fixture.write("docs/README.md", INTRO);
    fixture.write("docs/design/git.md", &front_matter("Git", "the git integration", "design"));
    fixture.write("docs/decisions/0001-x.md", &front_matter("0001: X", "x is chosen", "decisions"));
    fixture
}

#[test]
fn adding_a_doc_changes_the_index_without_editing_a_shared_file() {
    let before = documented();
    let first = before.xtask(&["docs-index"]);
    assert!(first.success, "{}", first.stderr);
    assert_eq!(
        first.stdout,
        "## Design\n- [Git](design/git.md): the git integration\n\n## Decisions\n- [0001: X](decisions/0001-x.md): x is chosen\n"
    );

    // Only the new doc is written: no README, no index file, no list.
    before.write("docs/design/team.md", &front_matter("Team", "team roles", "design"));
    let second = before.xtask(&["docs-index"]);
    assert!(second.success, "{}", second.stderr);
    let added: Vec<&str> = second.stdout.lines().filter(|line| !first.stdout.lines().any(|old| old == *line)).collect();
    assert_eq!(added, ["- [Team](design/team.md): team roles"]);
    assert_eq!(std::fs::read_to_string(before.root().join("docs/README.md")).unwrap(), INTRO);

    let lint = before.xtask(&["lint-docs-index"]);
    assert!(lint.success, "{}", lint.stdout);
    assert!(lint.stdout.contains("lint-docs-index: ok (3 docs)"), "{}", lint.stdout);
}

#[test]
fn a_doc_without_front_matter_fails_the_lint() {
    let fixture = documented();
    fixture.write("docs/design/bare.md", "# Bare\n\nNo front matter.\n");
    fixture.write("docs/research/partial.md", "---\ntitle: \"P\"\narea: research\n---\n");
    let run = fixture.xtask(&["lint-docs-index"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("docs/design/bare.md: no front matter"), "{}", run.stdout);
    assert!(run.stdout.contains("docs/research/partial.md: front matter has no `summary`"), "{}", run.stdout);
    assert!(run.stdout.contains("2 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_hand_written_index_in_the_readme_fails_the_lint() {
    let fixture = documented();
    fixture.write("docs/README.md", &format!("{INTRO}\n## Design\n- `design/git.md`: the git integration\n"));
    let run = fixture.xtask(&["lint-docs-index"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("docs/README.md:6: a hand-written index entry"), "{}", run.stdout);
    assert!(run.stdout.contains("1 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_doc_with_an_uppercase_extension_is_found() {
    let fixture = documented();
    fixture.write("docs/design/Upper.MD", "# Upper\n");
    let run = fixture.xtask(&["lint-docs-index"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("docs/design/Upper.MD: no front matter"), "{}", run.stdout);
}
