//! The link gate for `omuzaki`, which is also the MCP server (`omuzaki mcp`): the binary contains every self-registration
//! of the crates it links (ADR 0015). It runs the real binary, so it checks exactly what ships.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use std::path::Path;
use std::process::Command;

use xtask::registry::{self, Registration};

/// What `omuzaki` ships: each registering crate and the kind of registration it must contribute. The test
/// below compares the binary with the source of the crates it links, so it cannot notice a crate dropped from the
/// dependencies and from `link()` together; this list can.
const SHIPPED: [(&str, &str); 7] = [
    ("omuzaki-anim", "op"),
    ("omuzaki-char", "op"),
    ("omuzaki-face", "op"),
    ("omuzaki-io-ops", "op"),
    ("omuzaki-io-gltf", "importer"),
    ("omuzaki-io-vrm", "exporter"),
    ("omuzaki-char", "template"),
];

fn linked_registrations() -> Vec<Registration> {
    let output = Command::new(env!("CARGO_BIN_EXE_omuzaki")).arg("registrations").output().expect("omuzaki runs");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().map(|line| Registration::parse(line).unwrap_or_else(|| panic!("bad line {line:?}"))).collect()
}

#[test]
fn omuzaki_ships_the_products_ops_importers_exporters_and_templates() {
    let linked = linked_registrations();
    for (package, kind) in SHIPPED {
        assert!(
            linked.iter().any(|r| r.package == package && r.kind == kind),
            "`omuzaki` has no {kind} from {package}: is the crate linked in `omuzaki_cli::link`?"
        );
    }
}

#[test]
fn omuzaki_contains_every_registration_of_the_crates_it_links() {
    let linked = linked_registrations();
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected = registry::expected(&root, "omuzaki-cli", Some("omuzaki")).expect("the workspace scans");
    match registry::verify("omuzaki (CLI and MCP server)", &expected, &linked) {
        Ok(summary) => println!("{summary}"),
        Err(report) => panic!("{report}"),
    }
}
