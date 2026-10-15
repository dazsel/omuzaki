//! Document keys (ADR 0054 §5): a key registered outside this crate is validated and saved without editing it, and
//! this crate alone validates and saves every built-in key.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use omuzaki_char_api::doc_key::{self, BUILT_IN_NODE_KEYS, BUILT_IN_SIBLING_KEYS};
use omuzaki_char_api::{CharacterDef, paths, register_doc_key};
use omuzaki_core::link::registered;
use serde_json::json;

fn hats_path(id: &omuzaki_char_api::ids::CharId) -> omuzaki_doc_api::DocPath {
    paths::character(id).join("hats")
}

// A key of a feature that lives outside `omuzaki-char-api`.
register_doc_key!("hats", hats_path);

fn base() -> serde_json::Value {
    json!({ "name": "Mira", "template": "kit-humanoid", "seed": 7 })
}

#[test]
fn a_registered_key_is_validated_kept_and_saved() {
    let mut node = base();
    node["hats"] = json!({ "worn": "beret" });
    let def = CharacterDef::from_node(&node).expect("a registered key is a valid key");
    let mut saved = base();
    saved["hats"] = json!({ "worn": "beret" });
    def.to_node(&mut saved).expect("saves");
    assert_eq!(saved["hats"], json!({ "worn": "beret" }), "saving leaves the registered sub-tree alone");
    assert_eq!(CharacterDef::from_node(&saved).expect("reads back"), def);
    assert!(doc_key::sibling_keys().contains(&"hats") && doc_key::collisions().is_empty());
}

#[test]
fn a_registered_key_gives_its_path_and_the_typo_hint() {
    let id = omuzaki_char_api::ids::CharId::new("mira").expect("a valid id");
    assert_eq!(
        doc_key::path("hats", &id).expect("no collision").expect("registered").to_string(),
        "characters/mira/hats"
    );
    let mut node = base();
    node["hat"] = json!({});
    let message = CharacterDef::from_node(&node).expect_err("a typo is refused").to_string();
    assert!(message.contains("did you mean `hats`"), "{message}");
}

#[test]
fn every_built_in_key_validates_and_saves_without_omuzaki_char() {
    let mut node = base();
    for key in BUILT_IN_SIBLING_KEYS {
        node[key] = json!({ "k": 1 });
    }
    let def = CharacterDef::from_node(&node).expect("every built-in sibling key is valid");
    let mut saved = node.clone();
    def.to_node(&mut saved).expect("saves");
    for key in BUILT_IN_SIBLING_KEYS {
        assert_eq!(saved[key], json!({ "k": 1 }), "`{key}` survives a save");
    }
    for key in BUILT_IN_NODE_KEYS {
        assert!(doc_key::is_node_key(key));
    }
}

#[test]
fn the_registration_is_listed_for_the_link_gate() {
    assert!(registered().iter().any(|r| r.kind == "doc_key" && r.name == "\"hats\""));
}
