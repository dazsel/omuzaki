//! `xtask lint-net` against fixture workspaces: the capture-crate rules of SE-04, end to end through real `cargo metadata`.
//!
//! External crates are stand-ins under `vendor/`, which the workspace excludes: the lint sees them by name, as it would
//! `reqwest` itself, and nothing is downloaded.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::path::Path;
use std::process::Command;

use support::Fixture;

/// A stand-in for the external network crate `reqwest`, and a crate that wraps it.
const REQWEST: (&str, &str) = ("reqwest", "../../vendor/reqwest");
const WRAPPER: (&str, &str) = ("wrapper", "../../vendor/wrapper");

/// A manifest for `name` with normal dependencies `dependencies` (name, path), then `extra` lines.
fn manifest(name: &str, dependencies: &[(&str, &str)], extra: &str) -> String {
    let mut text = format!("[package]\nname = \"{name}\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[dependencies]\n");
    for (dependency, path) in dependencies {
        text += &[dependency, " = { path = \"", path, "\" }\n"].concat();
    }
    text + extra
}

/// A workspace whose members are `crates/*`, with `vendor/` outside them.
fn workspace() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("Cargo.toml", "[workspace]\nresolver = \"3\"\nmembers = [\"crates/*\"]\nexclude = [\"vendor\"]\n");
    fixture.write("vendor/reqwest/Cargo.toml", &manifest("reqwest", &[], ""));
    fixture.write("vendor/reqwest/src/lib.rs", "");
    fixture.write("vendor/wrapper/Cargo.toml", &manifest("wrapper", &[REQWEST], ""));
    fixture.write("vendor/wrapper/src/lib.rs", "");
    fixture
}

/// Writes the member `crates/<name>` with normal dependencies `dependencies` and manifest lines `extra`.
fn member(fixture: &Fixture, name: &str, dependencies: &[(&str, &str)], extra: &str) {
    fixture.write(&format!("crates/{name}/Cargo.toml"), &manifest(name, dependencies, extra));
    fixture.write(&format!("crates/{name}/src/lib.rs"), "");
}

const CAPTURE: &str = "\n[package.metadata.omuzaki]\nprivacy = \"capture\"\n";

#[test]
fn a_clean_workspace_passes_and_says_what_it_checked() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[], CAPTURE);
    member(&fixture, "omuzaki-cli", &[("omuzaki-audio", "../omuzaki-audio")], "");
    let run = fixture.xtask(&["lint-net"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert!(run.stdout.contains("lint-net: ok (1 capture crate(s), 0 crate(s) declare net)"), "{}", run.stdout);
}

#[test]
fn a_capture_crate_depending_on_reqwest_fails() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[REQWEST], CAPTURE);
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-audio -> reqwest: privacy = \"capture\" but it reaches a network crate, via omuzaki-audio -> reqwest"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("2 violation(s)"), "{}", run.stdout);
}

#[test]
fn a_capture_crate_reaches_a_network_crate_through_a_workspace_crate_that_declares_net() {
    let fixture = workspace();
    member(&fixture, "omuzaki-link", &[REQWEST], "\n[package.metadata.omuzaki]\nnet = \"the uplink to the relay\"\n");
    member(&fixture, "omuzaki-audio", &[("omuzaki-link", "../omuzaki-link")], CAPTURE);
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-audio -> omuzaki-link: privacy = \"capture\" but it reaches a network crate, via omuzaki-audio -> omuzaki-link"),
        "{}",
        run.stdout
    );
    assert!(!run.stdout.contains("-> reqwest"), "the search stops at the declared network crate:\n{}", run.stdout);
}

#[test]
fn a_capture_crate_reaches_a_network_crate_through_an_external_crate() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[WRAPPER], CAPTURE);
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-audio -> reqwest: privacy = \"capture\" but it reaches a network crate, via omuzaki-audio -> wrapper -> reqwest"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("1 violation(s)"), "{}", run.stdout);
}

#[test]
fn reqwest_only_in_dev_dependencies_passes() {
    let fixture = workspace();
    let dev = format!("\n[dev-dependencies]\nreqwest = {{ path = \"{}\" }}\n{CAPTURE}", REQWEST.1);
    member(&fixture, "omuzaki-audio", &[], &dev);
    let run = fixture.xtask(&["lint-net"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn a_crate_depending_directly_on_reqwest_must_declare_net() {
    let fixture = workspace();
    member(&fixture, "omuzaki-cli", &[REQWEST], "");
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success);
    assert!(
        run.stdout.starts_with("omuzaki-cli -> reqwest: depends directly on a network crate, so it must declare"),
        "{}",
        run.stdout
    );
    member(&fixture, "omuzaki-cli", &[REQWEST], "\n[package.metadata.omuzaki]\nnet = \"fetches the model index\"\n");
    let run = fixture.xtask(&["lint-net"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn an_empty_net_reason_fails() {
    let fixture = workspace();
    member(&fixture, "omuzaki-cli", &[REQWEST], "\n[package.metadata.omuzaki]\nnet = \"  \"\n");
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success);
    assert!(run.stdout.starts_with("omuzaki-cli -> [package.metadata.omuzaki]: net is empty"), "{}", run.stdout);
}

#[test]
fn an_unknown_privacy_value_fails_rather_than_exempting_the_crate() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[REQWEST], "\n[package.metadata.omuzaki]\nprivacy = \"Capture\"\n");
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success);
    assert!(
        run.stdout.contains("omuzaki-audio -> [package.metadata.omuzaki]: privacy = \"Capture\" is unknown"),
        "{}",
        run.stdout
    );
}

#[test]
fn a_capture_crate_naming_a_socket_in_its_source_fails() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[], CAPTURE);
    fixture.write("crates/omuzaki-audio/src/lib.rs", "use std::net::TcpStream;\n");
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success, "{}", run.stdout);
    assert!(
        run.stdout.contains("omuzaki-audio -> crates/omuzaki-audio/src/lib.rs:1: names `std::net`"),
        "{}",
        run.stdout
    );
    assert!(run.stdout.contains("names `TcpStream`"), "{}", run.stdout);
}

#[test]
fn socket_words_in_a_comment_or_a_string_do_not_count() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[], CAPTURE);
    fixture.write(
        "crates/omuzaki-audio/src/lib.rs",
        "// use std::net::TcpStream;\nconst NOTE: &str = \"tokio::net::UdpSocket\";\n",
    );
    let run = fixture.xtask(&["lint-net"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
}

#[test]
fn list_prints_the_crates_that_declare_net_in_name_order() {
    let fixture = workspace();
    member(&fixture, "omuzaki-zeta", &[], "\n[package.metadata.omuzaki]\nnet = \"z reason\"\n");
    member(&fixture, "omuzaki-alpha", &[], "\n[package.metadata.omuzaki]\nnet = \"a reason\"\n");
    member(&fixture, "omuzaki-cli", &[], "");
    let run = fixture.xtask(&["lint-net", "--list"]);
    assert!(run.success, "{}{}", run.stdout, run.stderr);
    assert_eq!(run.stdout, "omuzaki-alpha: a reason\nomuzaki-zeta: z reason\n");
}

#[test]
fn a_manifest_it_cannot_read_fails_closed() {
    let fixture = workspace();
    member(&fixture, "omuzaki-audio", &[], CAPTURE);
    fixture.write("crates/omuzaki-audio/Cargo.toml", "this is [not a manifest\n");
    let run = fixture.xtask(&["lint-net"]);
    assert!(!run.success, "{}", run.stdout);
}

#[test]
fn the_real_workspace_passes_and_lists_the_net_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let run = |args: &[&str]| {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask")).current_dir(root).args(args).output().unwrap();
        (output.status.success(), String::from_utf8_lossy(&output.stdout).into_owned())
    };
    let (passed, stdout) = run(&["lint-net"]);
    assert!(passed, "{stdout}");
    let (_, listed) = run(&["lint-net", "--list"]);
    assert!(listed.lines().any(|line| line.starts_with("omuzaki-mcp: ")), "{listed}");
}
