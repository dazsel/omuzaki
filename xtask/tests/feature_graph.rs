//! The gate's expected set follows Cargo features through the dependency graph: `--features omuzaki-stage/wgpu` on
//! the CLI switches on the stage's optional renderer, whose registrations then belong in the CLI binary.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeSet;
use std::path::Path;
use std::process::Command;

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
         [features]\ngpu = [\"omuzaki-stage/wgpu\"]\n",
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
    assert_eq!(names(&fixture, "omuzaki-cli", &[]), Vec::<String>::new());
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/wgpu"]), ["Shade"]);
    // The implicit feature of the optional dependency, named through the dependency that declares it.
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/omuzaki-render"]), ["Shade"]);
}

#[test]
fn a_feature_of_the_app_that_forwards_to_the_dependency_does_the_same() {
    let fixture = workspace();
    assert_eq!(names(&fixture, "omuzaki-cli", &["gpu"]), ["Shade"]);
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
    assert_eq!(names(&fixture, "omuzaki-cli", &["weak"]), Vec::<String>::new());
}

fn manifest(name: &str, rest: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n{rest}")
}

#[test]
fn a_feature_that_turns_on_another_feature_links_what_that_one_does() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        &manifest(
            "omuzaki-stage",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
             [features]\nwgpu = [\"backend\"]\nbackend = [\"dep:omuzaki-render\"]\n",
        ),
    );
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/wgpu"]), ["Shade"]);
    assert_eq!(names(&fixture, "omuzaki-cli", &["gpu"]), ["Shade"]);
}

#[test]
fn a_dependency_declaration_turns_on_its_features_and_its_default() {
    let fixture = workspace();
    let cli = |stage: &str| {
        manifest("omuzaki-cli", &format!("[dependencies]\nomuzaki-stage = {{ path = \"../omuzaki-stage\"{stage} }}\n"))
    };
    fixture.write("crates/omuzaki-cli/Cargo.toml", &cli(", features = [\"wgpu\"]"));
    assert_eq!(names(&fixture, "omuzaki-cli", &[]), ["Shade"]);

    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        &manifest(
            "omuzaki-stage",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
             [features]\ndefault = [\"wgpu\"]\nwgpu = [\"omuzaki-render\"]\n",
        ),
    );
    fixture.write("crates/omuzaki-cli/Cargo.toml", &cli(""));
    assert_eq!(names(&fixture, "omuzaki-cli", &[]), ["Shade"]);
    fixture.write("crates/omuzaki-cli/Cargo.toml", &cli(", default-features = false"));
    assert_eq!(names(&fixture, "omuzaki-cli", &[]), Vec::<String>::new());
}

#[test]
fn a_weak_feature_waits_for_the_dependency_to_be_switched_on_elsewhere() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        &manifest(
            "omuzaki-cli",
            "[dependencies]\nomuzaki-stage = { path = \"../omuzaki-stage\", optional = true }\n\
             [features]\nweak = [\"omuzaki-stage?/wgpu\"]\nstage = [\"dep:omuzaki-stage\"]\n",
        ),
    );
    assert_eq!(names(&fixture, "omuzaki-cli", &["weak"]), Vec::<String>::new());
    assert_eq!(names(&fixture, "omuzaki-cli", &["stage"]), Vec::<String>::new());
    // Listed before or after, the weak feature applies once `stage` has switched the dependency on.
    assert_eq!(names(&fixture, "omuzaki-cli", &["weak", "stage"]), ["Shade"]);
    assert_eq!(names(&fixture, "omuzaki-cli", &["stage", "weak"]), ["Shade"]);
}

#[test]
fn a_weak_feature_does_not_count_a_crate_linked_through_another_path() {
    // `omuzaki-cli` links the stage through `omuzaki-ui` too, but its own optional dependency on it stays off, so
    // its `weak` feature does nothing (Cargo's rule for `dep?/feature`).
    let fixture = workspace().krate("omuzaki-ui", &[("omuzaki-stage", Normal)]);
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        &manifest(
            "omuzaki-cli",
            "[dependencies]\nomuzaki-ui = { path = \"../omuzaki-ui\" }\n\
             omuzaki-stage = { path = \"../omuzaki-stage\", optional = true }\n\
             [features]\nweak = [\"omuzaki-stage?/wgpu\"]\n",
        ),
    );
    assert_eq!(names(&fixture, "omuzaki-cli", &["weak"]), Vec::<String>::new());
}

#[test]
fn a_feature_cargo_would_refuse_is_an_error_not_a_default_build() {
    let fixture = workspace();
    for feature in [
        "nope",
        "omuzaki-render/x",
        "omuzaki-cli-typo/gpu",
        "omuzaki-cli/nope",
        "omuzaki-stage/nope",
        "omuzaki-stage/default",
    ] {
        let error = xtask::registry::expected_with_features(fixture.root(), "omuzaki-cli", None, &[feature]);
        let message = error.unwrap_err().to_string();
        assert!(message.contains(&format!("`omuzaki-cli` has no feature `{feature}`")), "{message}");
    }
    // The package's own name prefixes its own features.
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-cli/gpu"]), ["Shade"]);
    // A dependency's implicit feature (its optional dependency's key) is a feature Cargo accepts, and so does the gate.
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/omuzaki-render"]), ["Shade"]);
    // `dep:` takes the implicit feature away, so Cargo refuses the name, and so does the gate.
    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        &manifest(
            "omuzaki-stage",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
             [features]\nwgpu = [\"dep:omuzaki-render\"]\n",
        ),
    );
    let hidden =
        xtask::registry::expected_with_features(fixture.root(), "omuzaki-cli", None, &["omuzaki-stage/omuzaki-render"]);
    assert!(hidden.unwrap_err().to_string().contains("has no feature `omuzaki-stage/omuzaki-render`"));
}

/// What the gate does with a binary's records: `linked` is what the binary would print.
fn gate(fixture: &Fixture, features: &[&str], linked: &[&str]) -> Result<String, String> {
    let expected = xtask::registry::expected_with_features(fixture.root(), "omuzaki-cli", None, features).unwrap();
    let linked: Vec<_> = linked
        .iter()
        .map(|name| xtask::registry::Registration {
            package: "omuzaki-render".into(),
            kind: "op".into(),
            name: (*name).into(),
            file: "crates/omuzaki-render/src/lib.rs".into(),
            line: 1,
        })
        .collect();
    xtask::registry::verify("omuzaki", &expected, &linked)
}

#[test]
fn the_gate_fails_when_a_transitively_enabled_crates_registration_is_missing() {
    let fixture = workspace();
    let report = gate(&fixture, &["omuzaki-stage/wgpu"], &[]).unwrap_err();
    assert!(report.contains("missing    omuzaki-render op Shade"), "{report}");
    // Through a feature of the App that forwards to the stage, too.
    assert!(gate(&fixture, &["gpu"], &[]).is_err());
}

#[test]
fn the_gate_passes_when_the_transitively_enabled_registration_is_present() {
    let fixture = workspace();
    assert!(gate(&fixture, &["omuzaki-stage/wgpu"], &["Shade"]).is_ok());
    assert!(gate(&fixture, &[], &[]).is_ok());
    // A backend linked without the features saying so is still reported: the gate fails closed.
    assert!(gate(&fixture, &[], &["Shade"]).is_err());
}

#[test]
fn the_registry_command_reads_the_builds_features_from_the_environment() {
    let fixture = workspace();
    let run = |features: Option<&str>| {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"));
        command.current_dir(fixture.root()).args(["registry", "--app", "omuzaki-cli"]);
        command.env_remove(xtask::registry::FEATURES_VAR);
        if let Some(features) = features {
            command.env(xtask::registry::FEATURES_VAR, features);
        }
        let output = command.output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        String::from_utf8(output.stdout).unwrap()
    };
    assert_eq!(run(None), "");
    assert!(run(Some("omuzaki-stage/wgpu")).contains("omuzaki-render\top\tShade"));
    assert!(run(Some("gpu, omuzaki-stage/wgpu")).contains("Shade"));
}

#[test]
fn a_registering_crate_switched_on_with_dep_syntax_is_still_refused() {
    let fixture = workspace();
    fixture.write(
        "crates/omuzaki-render/Cargo.toml",
        "[package]\nname = \"omuzaki-render\"\nversion = \"0.0.0\"\nedition = \"2024\"\n[dependencies]\n\
         omuzaki-ops-api = { path = \"../omuzaki-ops-api\" }\n\
         [package.metadata.omuzaki]\nregisters = true\n",
    );
    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        &manifest(
            "omuzaki-stage",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
             [features]\nwgpu = [\"dep:omuzaki-render\"]\n",
        ),
    );
    // The expected set still follows it ...
    assert_eq!(names(&fixture, "omuzaki-cli", &["omuzaki-stage/wgpu"]), ["Shade"]);
    // ... but the wiring lint refuses `dep:` on a registering dependency, whose generated `link()` call needs the
    // implicit feature.
    let run = fixture.xtask(&["lint-registers"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(format!("{}{}", run.stdout, run.stderr).contains("dep:"), "{}{}", run.stdout, run.stderr);
}

/// The workspace crates `cargo tree` says a build of `package` with `features` links (normal dependencies, this host).
fn cargo_links(root: &Path, package: &str, features: &[&str]) -> BTreeSet<String> {
    let members: BTreeSet<String> =
        xtask::registry::Sources::scan(root).unwrap().crates().iter().map(|c| c.krate.name.clone()).collect();
    let mut command = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    command.current_dir(root).args(["tree", "-p", package, "-e", "normal", "--prefix", "none", "--format", "{p}"]);
    if !features.is_empty() {
        command.args(["--features", &features.join(",")]);
    }
    let output = command.output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .filter_map(|line| line.split_whitespace().next())
        .filter(|name| members.contains(*name))
        .map(str::to_owned)
        .collect()
}

/// The workspace crates the gate's resolution links for the same build.
fn gate_links(root: &Path, package: &str, features: &[&str]) -> BTreeSet<String> {
    let sources = xtask::registry::Sources::scan(root).unwrap();
    sources.linked_crates(package, features).unwrap().into_iter().collect()
}

#[test]
fn the_resolution_agrees_with_cargo_on_the_fixtures() {
    let fixture = workspace();
    let agree = |features: &[&str]| {
        let root = fixture.root();
        assert_eq!(
            gate_links(root, "omuzaki-cli", features),
            cargo_links(root, "omuzaki-cli", features),
            "{features:?}"
        );
    };
    for features in [&[][..], &["omuzaki-stage/wgpu"], &["gpu"], &["omuzaki-stage/omuzaki-render"]] {
        agree(features);
    }
    // Weak features, `dep:` and a feature turning on another, behind an optional stage.
    fixture.write(
        "crates/omuzaki-stage/Cargo.toml",
        &manifest(
            "omuzaki-stage",
            "[dependencies]\nomuzaki-render = { path = \"../omuzaki-render\", optional = true }\n\
             [features]\nwgpu = [\"backend\"]\nbackend = [\"dep:omuzaki-render\"]\n",
        ),
    );
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        &manifest(
            "omuzaki-cli",
            "[dependencies]\nomuzaki-stage = { path = \"../omuzaki-stage\", optional = true }\n\
             [features]\nweak = [\"omuzaki-stage?/wgpu\"]\nstage = [\"dep:omuzaki-stage\"]\n",
        ),
    );
    for features in [&[][..], &["weak"], &["stage"], &["weak", "stage"], &["omuzaki-stage/wgpu"]] {
        agree(features);
    }
    // The stage linked through another crate doesn't switch on the App's own optional dependency on it.
    let fixture = fixture.krate("omuzaki-ui", &[("omuzaki-stage", Normal)]);
    fixture.write(
        "crates/omuzaki-cli/Cargo.toml",
        &manifest(
            "omuzaki-cli",
            "[dependencies]\nomuzaki-ui = { path = \"../omuzaki-ui\" }\n\
             omuzaki-stage = { path = \"../omuzaki-stage\", optional = true }\n\
             [features]\nweak = [\"omuzaki-stage?/wgpu\"]\n",
        ),
    );
    let root = fixture.root();
    assert_eq!(gate_links(root, "omuzaki-cli", &["weak"]), cargo_links(root, "omuzaki-cli", &["weak"]));
}

/// The product's gate configurations: if this fails, the gate's resolution and Cargo's disagree on what the binary
/// links, and the gate's resolution (`xtask/src/registry/mod.rs`, `FeatureGraph`) needs to learn the difference.
#[test]
fn the_resolution_agrees_with_cargo_on_the_workspaces_gate_configurations() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
    for (package, features) in [
        ("omuzaki-cli", &[][..]),
        ("omuzaki-cli", &["omuzaki-stage/wgpu"]),
        ("omuzaki-stage", &[]),
        ("omuzaki-stage", &["wgpu"]),
    ] {
        let ours = gate_links(&root, package, features);
        assert!(ours.contains("omuzaki-render") == features.iter().any(|f| f.ends_with("wgpu")), "{ours:?}");
        assert_eq!(ours, cargo_links(&root, package, features), "{package} {features:?}");
    }
}
