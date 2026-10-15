//! `xtask models` and a shipped model's lineage, against fixture workspaces (ML-01, `docs/licensing/policy.md`).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Run};

/// Runs the gate on a workspace whose one shipped model is the fixture manifest `case`.
fn gate(case: &str) -> Run {
    let fixture = Fixture::new().krate("omuzaki-tiny", &[]);
    let manifest = std::fs::read_to_string(format!(
        "{}/tests/fixtures/models_lineage/{case}.model.toml",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap();
    fixture.write("crates/omuzaki-tiny/models/tiny.model.toml", &manifest);
    fixture.write(
        "docs/licensing/deps/tiny-model.toml",
        "section = \"models\"\nitem = \"Tiny model\"\nkind = \"model\"\nused_for = \"a test\"\nstatus = \"yes\"\n\
         condition = \"MIT\"\naudit = \"A1\"\n",
    );
    fixture.xtask(&["models"])
}

#[test]
fn a_fully_permissive_lineage_passes() {
    let run = gate("permissive");
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("models: ok (1 shipped model(s)"), "{}", run.stdout);
}

#[test]
fn permissive_weights_trained_on_non_commercial_data_fail() {
    let run = gate("non_commercial_data");
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("model `tiny`: lineage link `Face set 2` (training_data)"), "{}", run.stdout);
    assert!(run.stdout.contains("CC-BY-NC-4.0") && run.stdout.contains("https://data.example/faces"), "{}", run.stdout);
}

#[test]
fn an_unknown_lineage_licence_fails() {
    let run = gate("unknown_licence");
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("lineage link `Mystery set 3`") && run.stdout.contains("not an allowed SPDX licence"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_missing_lineage_fails() {
    let run = gate("missing_lineage");
    assert!(!run.success, "{}", run.stdout);
    assert!(run.stdout.contains("model `tiny` has no documented lineage"), "{}", run.stdout);
}
