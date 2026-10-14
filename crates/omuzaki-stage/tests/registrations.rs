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
    let expected = registry::expected(&root, "omuzaki-stage", None).expect("the workspace scans");
    match registry::verify("omuzaki-stage", &expected, &linked) {
        Ok(summary) => println!("{summary}"),
        Err(report) => panic!("{report}"),
    }
}
