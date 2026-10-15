//! A document key that is registered twice, under a built-in key's name, or isn't a slug is refused by name (ADR
//! 0054 §5). This test crate registers all of them, so it is its own test binary.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used)]

use omuzaki_char_api::doc_key::{self, DocKeyError};
use omuzaki_char_api::ids::CharId;
use omuzaki_char_api::{CharacterDef, register_doc_key};
use serde_json::json;

register_doc_key!("twice");
register_doc_key!("twice");
register_doc_key!("rig");
register_doc_key!("fine");
register_doc_key!("Hat");
register_doc_key!("a/b");
register_doc_key!("con");

/// The refused keys, in the order `refused()` lists them.
const REFUSED: [&str; 5] = ["Hat", "a/b", "con", "rig", "twice"];

fn node(key: &str) -> serde_json::Value {
    json!({ "name": "Mira", "template": "kit-humanoid", key: {} })
}

#[test]
fn refused_keys_are_listed_sorted_with_their_reason() {
    assert_eq!(doc_key::refused().keys().copied().collect::<Vec<_>>(), REFUSED);
    for key in ["Hat", "a/b", "con"] {
        assert!(matches!(doc_key::refused()[key], DocKeyError::NotASlug { .. }), "`{key}` isn't a slug");
    }
    for key in ["rig", "twice"] {
        assert!(matches!(doc_key::refused()[key], DocKeyError::Collision(_)), "`{key}` collides");
    }
}

#[test]
fn refused_keys_have_no_path_and_the_others_do() {
    let id = CharId::new("mira").expect("a valid id");
    for key in REFUSED {
        let error = doc_key::path(key, &id).expect_err("a refused key has no path");
        assert!(error.to_string().contains(&format!("`{key}`")), "{error}");
    }
    assert_eq!(
        doc_key::path("fine", &id).expect("not refused").expect("registered").to_string(),
        "characters/mira/fine"
    );
    assert!(doc_key::sibling_keys().contains(&"fine"));
    for key in ["Hat", "a/b", "con", "twice"] {
        assert!(!doc_key::sibling_keys().contains(&key), "`{key}` is refused, so it isn't a sibling key");
    }
    // `rig` is built in: the refused registration of it adds nothing, so the built-in entry is the only one.
    assert_eq!(doc_key::sibling_keys().iter().filter(|key| **key == "rig").count(), 1);
}

#[test]
fn a_character_with_a_refused_key_is_refused_naming_it() {
    for key in ["twice", "rig"] {
        let message = CharacterDef::from_node(&node(key)).expect_err("a collision is refused").to_string();
        assert!(message.contains(&format!("`{key}`")) && message.contains("more than once"), "{message}");
    }
    for key in ["Hat", "a/b", "con"] {
        let message = CharacterDef::from_node(&node(key)).expect_err("a key that isn't a slug is refused").to_string();
        assert!(message.contains(&format!("`{key}`")) && message.contains("can't name a file or folder"), "{message}");
    }
    CharacterDef::from_node(&node("fine")).expect("a unique registered key is valid");
}
