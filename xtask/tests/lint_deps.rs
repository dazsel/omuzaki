//! `xtask lint-deps` against fixture workspaces: the layering table from `crates/README.md`, end to end.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use support::{
    Fixture,
    Section::{Build, Dev, Normal},
};

/// core <- doc-api <- doc-mock; two implementations against doc-api; an app wiring them.
fn well_layered() -> Fixture {
    Fixture::new()
        .krate("omuzaki-core", &[])
        .krate("omuzaki-doc-api", &[("omuzaki-core", Normal)])
        .krate("omuzaki-doc-mock", &[("omuzaki-doc-api", Normal)])
        .krate("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Dev)])
        .krate("omuzaki-audio", &[("omuzaki-doc-api", Normal), ("omuzaki-core", Normal)])
        .krate("omuzaki-cli", &[("omuzaki-render", Normal), ("omuzaki-audio", Normal), ("omuzaki-doc-mock", Normal)])
}

#[test]
fn a_well_layered_workspace_passes_and_a_dev_dependency_on_a_mock_is_allowed() {
    let run = well_layered().xtask(&["lint-deps"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("lint-deps: ok (6 crates)"), "{}", run.stdout);
}

#[test]
fn an_implementation_depending_on_another_implementation_is_caught() {
    let fixture = well_layered().krate("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-audio", Normal)]);
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success, "{}", run.stdout);
    let line = run.stdout.lines().next().unwrap();
    assert!(
        line.starts_with("omuzaki-render → omuzaki-audio: implementation crate may not depend on implementation crate"),
        "{line}"
    );
    assert!(run.stdout.contains("1 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_mock_under_normal_or_build_dependencies_is_caught() {
    let fixture = well_layered()
        .krate("omuzaki-render", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Normal)])
        .krate("omuzaki-audio", &[("omuzaki-doc-api", Normal), ("omuzaki-doc-mock", Build)]);
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success);
    assert!(run.stdout.contains("omuzaki-render → omuzaki-doc-mock"), "{}", run.stdout);
    assert!(run.stdout.contains("omuzaki-audio → omuzaki-doc-mock"), "{}", run.stdout);
    assert!(run.stdout.contains("2 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_contract_depending_on_an_implementation_is_caught() {
    let fixture = well_layered().krate("omuzaki-doc-api", &[("omuzaki-core", Normal), ("omuzaki-render", Normal)]);
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success);
    assert!(run.stdout.contains("omuzaki-doc-api → omuzaki-render: API crate may not depend on implementation crate"));
}

/// The format-library layer: core <- io-api <- io-mock; `omuzaki-gltf` (a lib) on the contracts; two implementations on
/// the lib; an app.
fn with_a_lib() -> Fixture {
    Fixture::new()
        .krate("omuzaki-core", &[])
        .krate("omuzaki-io-api", &[("omuzaki-core", Normal)])
        .krate("omuzaki-io-mock", &[("omuzaki-io-api", Normal)])
        .krate("omuzaki-gltf", &[("omuzaki-io-api", Normal), ("omuzaki-core", Normal), ("omuzaki-io-mock", Dev)])
        .layer("omuzaki-gltf", "lib")
        .krate("omuzaki-io-gltf", &[("omuzaki-gltf", Normal), ("omuzaki-io-api", Normal), ("omuzaki-io-mock", Dev)])
        .krate("omuzaki-io-vrm", &[("omuzaki-gltf", Normal), ("omuzaki-io-api", Normal)])
        .krate("omuzaki-cli", &[("omuzaki-io-gltf", Normal), ("omuzaki-io-vrm", Normal), ("omuzaki-gltf", Normal)])
}

#[test]
fn implementations_and_apps_may_depend_on_a_lib_crate() {
    let run = with_a_lib().xtask(&["lint-deps"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("lint-deps: ok (7 crates)"), "{}", run.stdout);
}

#[test]
fn without_its_layer_in_the_manifest_the_lib_is_an_implementation_and_the_dependency_is_caught() {
    let fixture = Fixture::new()
        .krate("omuzaki-core", &[])
        .krate("omuzaki-io-api", &[("omuzaki-core", Normal)])
        .krate("omuzaki-gltf", &[("omuzaki-io-api", Normal)])
        .krate("omuzaki-io-gltf", &[("omuzaki-gltf", Normal), ("omuzaki-io-api", Normal)]);
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-io-gltf → omuzaki-gltf: implementation crate may not depend on implementation")
    );
}

#[test]
fn a_lib_may_not_depend_on_an_implementation_or_an_app() {
    let fixture = with_a_lib()
        .krate("omuzaki-gltf", &[("omuzaki-io-api", Normal), ("omuzaki-io-gltf", Normal), ("omuzaki-cli", Dev)]);
    let fixture = fixture.layer("omuzaki-gltf", "lib");
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success);
    assert!(run.stdout.contains("omuzaki-gltf → omuzaki-io-gltf: lib crate may not depend on implementation crate"));
    assert!(run.stdout.contains("omuzaki-gltf → omuzaki-cli: lib crate may not depend on app crate"));
}

#[test]
fn a_contract_or_a_mock_may_not_depend_on_a_lib() {
    let fixture = with_a_lib()
        .krate("omuzaki-io-api", &[("omuzaki-core", Normal), ("omuzaki-gltf", Normal)])
        .krate("omuzaki-io-mock", &[("omuzaki-io-api", Normal), ("omuzaki-gltf", Dev)]);
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success);
    assert!(
        run.stdout.contains("omuzaki-io-api → omuzaki-gltf: API crate may not depend on lib crate"),
        "{}",
        run.stdout
    );
    assert!(
        run.stdout.contains("omuzaki-io-mock → omuzaki-gltf: mock crate may not depend on lib crate"),
        "{}",
        run.stdout
    );
}

#[test]
fn an_unknown_layer_is_reported() {
    let fixture = with_a_lib().layer("omuzaki-io-vrm", "shared");
    let run = fixture.xtask(&["lint-deps"]);
    assert!(!run.success);
    assert!(
        run.stdout.contains("omuzaki-io-vrm → [package.metadata.omuzaki]: declares unknown layer"),
        "{}",
        run.stdout
    );
}
