//! The link gate for the stage: a binary that links `omuzaki-stage` contains every self-registration of the crates the
//! stage links (part kinds, templates, grammar rules, ops, ...), by name (ADR 0015).

use std::path::Path;

use xtask::registry::{self, Registration};

#[test]
fn a_binary_linking_the_stage_contains_every_registration_of_the_crates_it_links() {
    omuzaki_stage::link(); // as every App does at startup
    let linked: Vec<Registration> = omuzaki_core::link::registered()
        .iter()
        .map(|r| Registration::parse(&r.to_string()).unwrap_or_else(|| panic!("bad record {r:?}")))
        .collect();

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    // The render backend is an optional dependency whose registrations are in the binary exactly when its implicit
    // feature is on, however it was switched on.
    let features: &[&str] = if cfg!(feature = "omuzaki-render") { &["omuzaki-render"] } else { &[] };
    let expected =
        registry::expected_with_features(&root, "omuzaki-stage", None, features).expect("the workspace scans");
    match registry::verify("omuzaki-stage", &expected, &linked) {
        Ok(summary) => println!("{summary}"),
        Err(report) => panic!("{report}"),
    }
}
