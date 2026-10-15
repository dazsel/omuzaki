//! The built-in facts are listed once `link()` is called (ADR 0054 section 5), and nothing that builds or validates
//! depends on that: this binary reads and merges facts first without ever calling it.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::{PartId, Published, Publishes, Stature, StatureFact, facts};

const BUILT_IN: [&str; 6] = ["anchors", "colliders", "face", "fit", "head", "stature"];

#[test]
fn facts_merge_and_read_in_a_binary_that_never_calls_link() {
    let part = PartId::new("body").unwrap();
    let stature = Stature { scale: 1.0, height_m: 1.7 };
    let mut published = Published::default();
    published.absorb(&part, Publishes::default().with::<StatureFact>(stature)).unwrap();
    assert_eq!(*published.get::<StatureFact>(), Some(stature));
}

#[test]
fn link_lists_the_six_built_in_facts_once_each() {
    omuzaki_char_api::link();
    let listed = facts().expect("no two facts share a name");
    for name in BUILT_IN {
        assert_eq!(listed.iter().filter(|f| f.name == name).count(), 1, "fact '{name}' in {:?}", names(&listed));
    }
    let recorded: Vec<_> = omuzaki_core::link::registered()
        .into_iter()
        .filter(|r| r.kind == "fact" && r.package == "omuzaki-char-api")
        .collect();
    assert_eq!(recorded.len(), BUILT_IN.len(), "{recorded:?}");
}

fn names(listed: &[&omuzaki_char_api::FactRegistration]) -> Vec<&'static str> {
    listed.iter().map(|f| f.name).collect()
}
