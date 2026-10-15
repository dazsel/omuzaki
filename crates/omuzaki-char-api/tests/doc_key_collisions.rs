//! A document key registered twice, or under a built-in key's name, is refused by name (ADR 0054 §5). This test crate
//! registers both, so it is its own test binary.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use omuzaki_char_api::ids::CharId;
use omuzaki_char_api::{CharacterDef, doc_key, paths, register_doc_key};
use serde_json::json;

fn path(id: &CharId) -> omuzaki_doc_api::DocPath {
    paths::character(id).join("x")
}

register_doc_key!("twice", path);
register_doc_key!("twice", path);
register_doc_key!("rig", path);
register_doc_key!("fine", path);

fn node(key: &str) -> serde_json::Value {
    json!({ "name": "Mira", "template": "kit-humanoid", key: {} })
}

#[test]
fn colliding_keys_are_listed_and_have_no_path() {
    assert_eq!(doc_key::collisions(), ["rig", "twice"]);
    let id = CharId::new("mira").expect("a valid id");
    for key in ["twice", "rig"] {
        let error = doc_key::path(key, &id).expect_err("a collision has no path");
        assert!(error.to_string().contains(&format!("`{key}`")), "{error}");
    }
    assert!(doc_key::path("fine", &id).expect("no collision").is_some());
    assert!(!doc_key::sibling_keys().contains(&"twice") && doc_key::sibling_keys().contains(&"fine"));
}

#[test]
fn a_character_with_a_colliding_key_is_refused_naming_it() {
    for key in ["twice", "rig"] {
        let message = CharacterDef::from_node(&node(key)).expect_err("a collision is refused").to_string();
        assert!(message.contains(&format!("`{key}`")) && message.contains("more than once"), "{message}");
    }
    CharacterDef::from_node(&node("fine")).expect("a unique registered key is valid");
}
