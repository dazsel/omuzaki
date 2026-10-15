//! `xtask check --affected --list` and the nothing-to-run path against fixture git repositories.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{
    Fixture,
    Section::{Dev, Normal},
};

/// A committed workspace on `main`, with the commit tagged as the base a PR branches from.
fn committed() -> Fixture {
    let fixture = Fixture::new()
        .krate("omuzaki-core", &[])
        .krate("omuzaki-doc-api", &[("omuzaki-core", Normal)])
        .krate("omuzaki-doc-mock", &[("omuzaki-doc-api", Normal)])
        .krate("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Dev)])
        .krate("omuzaki-audio", &[("omuzaki-doc-api", Normal)])
        .krate("omuzaki-cli", &[("omuzaki-render", Normal)]);
    fixture.write("docs/README.md", "docs\n");
    fixture.commit_all("base");
    fixture.git(&["branch", "base"]);
    fixture
}

fn listed(fixture: &Fixture) -> Vec<String> {
    let run = fixture.xtask(&["check", "--affected", "--list", "--base", "base"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    run.stdout.lines().map(str::to_owned).collect()
}

#[test]
fn a_docs_only_change_runs_only_the_models_gate() {
    let fixture = committed();
    fixture.write("docs/README.md", "docs, edited\n");
    fixture.write("docs/new-note.md", "untracked\n");
    assert_eq!(listed(&fixture), Vec::<String>::new());

    // No crate is affected, so no build starts: the one step is the repo-wide models gate, which reads the licence files.
    fixture.write(LICENCE_FILE, LICENCE);
    let run = fixture.xtask(&["check", "--affected", "--base", "base"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("==> models"), "{}", run.stdout);
    assert!(!run.stdout.contains("==> fmt"), "{}", run.stdout);
}

/// The licence file that allows the shipped `tiny` model, and the model's manifest citing it.
const LICENCE_FILE: &str = "docs/licensing/deps/tiny-model.toml";
const LICENCE: &str = "section = \"models\"\nitem = \"Tiny model\"\nkind = \"model\"\nused_for = \"a test\"\nstatus = \"yes\"\n\
condition = \"MIT OR Apache-2.0; keep notice\"\naudit = \"A1\"\n";
const MANIFEST: &str = r#"id = "tiny"
version = "1.0.0"
name = "Tiny"
slot = "test_echo@1"
runtime = "fake"
providers = ["cpu"]

[[files]]
name = "tiny.fake.toml"
sha256 = "e5b9c1f03be8153f32f0df28a9196840741698957730b384a262d2b9517bb99c"
size = 226
urls = ["https://models.example/tiny/1.0.0/tiny.fake.toml"]

[card]
register = "Tiny model"
authors = "Omuzaki"

[card.licence]
spdx = "MIT OR Apache-2.0"
"#;

/// A committed workspace whose base already ships `tiny` with its licence file, so a later change to only the licence files
/// affects no crate.
fn shipping() -> Fixture {
    let fixture = Fixture::new().krate("omuzaki-tiny", &[]);
    fixture.write("crates/omuzaki-tiny/models/tiny.model.toml", MANIFEST);
    fixture.write(LICENCE_FILE, LICENCE);
    fixture.commit_all("base");
    fixture.git(&["branch", "base"]);
    fixture
}

#[test]
fn a_register_only_change_that_breaks_a_shipped_model_fails_the_check() {
    let fixture = shipping();
    fixture.write(LICENCE_FILE, &LICENCE.replace("\"yes\"", "\"NO\""));
    let run = fixture.xtask(&["check", "--affected", "--base", "base"]);
    assert!(!run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("models: 1 problem(s)"), "{}", run.stdout);
    assert!(!run.stdout.contains("==> fmt"), "{}", run.stdout);
}

#[test]
fn a_committed_change_selects_the_crate_and_its_dependents() {
    let fixture = committed();
    fixture.write("crates/omuzaki-render/src/lib.rs", "// changed\n");
    fixture.commit_all("touch render");
    assert_eq!(listed(&fixture), ["omuzaki-cli", "omuzaki-render"]);
}

#[test]
fn uncommitted_and_untracked_files_count_too() {
    let fixture = committed();
    fixture.write("crates/omuzaki-doc-api/src/lib.rs", "// staged\n");
    fixture.git(&["add", "-A"]);
    fixture.write("crates/omuzaki-audio/src/extra.rs", "// untracked\n");
    assert_eq!(
        listed(&fixture),
        ["omuzaki-audio", "omuzaki-cli", "omuzaki-doc-api", "omuzaki-doc-mock", "omuzaki-render"]
    );
}

#[test]
fn a_root_config_change_means_the_whole_workspace() {
    let fixture = committed();
    fixture.write("rust-toolchain.toml", "[toolchain]\nchannel = \"stable\"\n");
    let run = fixture.xtask(&["check", "--affected", "--list", "--base", "base"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.lines().count(), 6, "{}", run.stdout);
    assert!(run.stderr.contains("rust-toolchain.toml changed"), "{}", run.stderr);
}

#[test]
fn all_lists_every_crate_even_with_no_change() {
    let fixture = committed();
    let run = fixture.xtask(&["check", "--all", "--list"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout.lines().count(), 6);
}

#[test]
fn an_unknown_base_is_an_error_that_says_how_to_fix_it() {
    let fixture = committed();
    let run = fixture.xtask(&["check", "--affected", "--list", "--base", "no-such-ref"]);
    assert!(!run.success);
    assert!(run.stderr.contains("git fetch origin"), "{}", run.stderr);
}

#[test]
fn json_reports_the_changed_direct_and_full_sets_with_shards() {
    let fixture = committed();
    fixture.write("crates/omuzaki-doc-api/src/lib.rs", "// changed\n");
    fixture.commit_all("touch doc-api");
    let run = fixture.xtask(&["check", "--format", "json", "--shards", "2", "--base", "base"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    let report: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(report["mode"], "crates");
    assert_eq!(report["changed"], serde_json::json!(["omuzaki-doc-api"]));
    assert_eq!(
        report["direct"],
        serde_json::json!(["omuzaki-audio", "omuzaki-doc-api", "omuzaki-doc-mock", "omuzaki-render"])
    );
    assert_eq!(report["full"].as_array().unwrap().len(), 6 - 1, "everything but omuzaki-core: {report}");
    assert_eq!(report["shards_changed"], serde_json::json!([["omuzaki-doc-api"]]));
    assert_eq!(report["shards_full"].as_array().unwrap().len(), 2);
}

#[test]
fn json_for_a_docs_only_change_is_empty_with_no_shards() {
    let fixture = committed();
    fixture.write("docs/README.md", "edited\n");
    let run = fixture.xtask(&["check", "--format", "json", "--base", "base"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    let report: serde_json::Value = serde_json::from_str(run.stdout.trim()).unwrap();
    assert_eq!(report["empty"], true);
    assert_eq!(report["shards_full"], serde_json::json!([]));
}
