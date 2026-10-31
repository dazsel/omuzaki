//! The built-in facts (the six the parts publish, the skin the body and garments publish (#2604) and the design layer's two frames) are listed once `link()` is called (ADR 0054 section 5), and nothing that builds or validates
//! depends on that (`no_link.rs` tests that in a binary that never calls it).

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use omuzaki_char_api::facts;

const BUILT_IN: [&str; 9] =
    ["anchors", "body-frame", "colliders", "face", "fit", "head", "head-frame", "skin", "stature"];

#[test]
fn link_lists_the_nine_built_in_facts_once_each() {
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
