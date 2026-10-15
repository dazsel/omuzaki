//! The gate's expected set follows Cargo features through the dependency graph: `--features omuzaki-stage/wgpu` on
//! the CLI switches on the stage's optional renderer, whose registrations then belong in the CLI binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{Fixture, Section::Normal};

const OP_MACRO: &str = "#[macro_export]\nmacro_rules! register_op { ($ty:ty) => {}; }\n";

/// cli -> stage -(optional, `wgpu`)-> render, which registers one op.
fn workspace() -> Fixture {
    let fixture = Fixture::new()
        .krate("omuzaki-ops-api", &[])
        .krate("omuzaki-render", &[("omuzaki-ops-api", Normal)])
        .krate("omuzaki-stage", &[])
        .krate("omuzaki-cli", &[("omuzaki-stage", Normal)]);
    fixture.write("crates/omuzaki-ops-api/src/lib.rs", OP_MACRO);
    fixture.write("crates/omuzaki-render/src/lib.rs", "omuzaki_ops_api::register_op!(Shade);\n");
    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        "[package]\nname = \"omuzaki-stage\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
         omuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
         [features]\nwgpu = [\"omuzaki-render\"]\n",
    );
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        "[package]\nname = \"omuzaki-cli\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
         omuzaki-stage = { path = \"../omuzaki-stage\" }\n\
         [features]\ngpu = [\"omuzaki-stage/wgpu\"]\nweak = [\"omuzaki-stage?/wgpu\"]\n",
    );
    fixture
}

fn names(fixture: &Fixture, app: &str, features: &[&str]) -> Vec<String> {
    xtask::registry::expected_with_features(fixture.root(), app, None, features)
        .unwrap()
        .into_iter()
        .map(|r| r.name)
        .collect()
}

#[test]
fn a_feature_of_a_dependency_switches_on_its_optional_dependency() {
    let fixture = workspace();
    assert!(names(&fixture, "omuzaki-cli", &[]).is_empty());
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/wgpu"]), ["Shade"]);
    // The implicit feature of the optional dependency, named through the dependency that declares it.
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/omuzaki-render"]), ["Shade"]);
}

#[test]
fn a_feature_of_the_app_that_forwards_to_the_dependency_does_the_same() {
    let fixture = workspace();
    assert_eq!(names(&fixture, "omuzaki-cli", &["gpu"]), ["Shade"]);
    // `dep?/feature` enables the feature only when the dependency is active anyway, which the stage is.
    assert_eq!(names(&fixture, "omuzaki-cli", &["weak"]), ["Shade"]);
}

#[test]
fn a_weak_feature_does_not_activate_a_dependency_nothing_else_needs() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        "[package]\nname = \"omuzaki-cli\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
         omuzaki-stage = { path = \"../omuzaki-stage\", optional = true }\n\
         [features]\nweak = [\"omuzaki-stage?/wgpu\"]\n",
    );
    assert!(names(&fixture, "omuzaki-cli", &["weak"]).is_empty());
}
