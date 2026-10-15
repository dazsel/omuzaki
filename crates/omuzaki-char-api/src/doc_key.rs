//! Document keys: the keys of a character node in the project (`characters/<id>/<key>`), as an open registry (ADR 0054
//! §5).
//!
//! A character's node holds the keys of its own definition (node keys: exactly the fields of
//! [`CharacterDef`](crate::CharacterDef), read into it) and the roots of its other files (sibling keys, each a
//! sub-tree with its own contract). A new file of the character is a new sibling key in the file that owns it, plus
//! [`register_doc_key!`]; nobody edits a central list. [`CharacterDef::from_node`](crate::CharacterDef::from_node) and
//! [`to_node`](crate::CharacterDef::to_node) leave sibling keys alone and read [`sibling_keys`] to accept them.
//!
//! A key can be registered once. A key registered twice, or under the name of a built-in key, is a collision: it is
//! refused everywhere, with an error that names it ([`collisions`]), so no binary picks a winner by link order.
//!
//! The keys this crate's own types own, and the sibling keys whose owners are not linked by every binary that parses a
//! character, are the built-in table ([`BUILT_IN_NODE_KEYS`], [`BUILT_IN_SIBLING_KEYS`]), so validation never depends
//! on what a binary linked (ADR 0015). Registered keys add to it. The built-in keys are also recorded for the listing of
//! registrations (`omuzaki registrations`), so one list shows every key; that record needs this crate's `link()`, the
//! table does not.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use omuzaki_doc_api::DocPath;

use crate::ids::CharId;

/// One registered document key, collected by [`register_doc_key!`]. A registered key is always a sibling key.
pub struct DocKeyRegistration {
    /// The key, as it is spelt in the project and in saved files.
    pub key: &'static str,
    /// The path of the key in the document, for the character `id`.
    pub path: fn(&CharId) -> DocPath,
}

inventory::collect!(DocKeyRegistration);

/// Why a registered key cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocKeyError {
    /// The key is registered more than once, or has the name of a built-in key.
    #[error("document key `{0}` is registered more than once or collides with a built-in key")]
    Collision(String),
}

/// The keys of `characters/<id>` that make up a [`CharacterDef`](crate::CharacterDef) (`character.toml` and
/// `face.toml`): its fields. A new field is added here beside the struct.
pub const BUILT_IN_NODE_KEYS: [&str; 8] =
    ["name", "template", "seed", "palette", "skin_tone", "parts", "face", "weight_brushes"];

/// The built-in sibling keys: the roots of the other files of the character. They are `variants`, `colourways`, `rig`,
/// `look`, `licence`, `sources`, `applied`, `poses` and `emotes`; some are owned by crates that not every binary which
/// parses a character links, so they cannot register.
pub const BUILT_IN_SIBLING_KEYS: [&str; 9] =
    ["variants", "colourways", "rig", "look", "licence", "sources", "applied", "poses", "emotes"];

/// Registers a sibling document key next to the file of the character that owns it (ADR 0054 §5). `path` is a
/// `fn(&CharId) -> DocPath`.
///
/// ```ignore
/// omuzaki_char_api::register_doc_key!("hats", hats_path);
/// ```
///
/// The form `register_doc_key!("name", built_in)` is for this module only: it records the key for the listing
/// ([`omuzaki_core::link::registered`], `omuzaki registrations`) and registers nothing, because the built-in keys are the
/// table above and validation never reads the registry for them.
#[macro_export]
macro_rules! register_doc_key {
    ($key:literal, built_in) => {
        $crate::__record_registration!("doc_key", $key);
    };
    ($key:literal, $path:path) => {
        $crate::__inventory::submit! {
            $crate::DocKeyRegistration { key: $key, path: $path }
        }
        $crate::__record_registration!("doc_key", $key);
    };
}

// The built-in keys, listed beside the registered ones (ADR 0054 §5). The record is in a binary exactly when this crate's
// `link()` is called; the tables above are what validation reads, so it never depends on that. The test below keeps the
// two in step.
register_doc_key!("name", built_in);
register_doc_key!("template", built_in);
register_doc_key!("seed", built_in);
register_doc_key!("palette", built_in);
register_doc_key!("skin_tone", built_in);
register_doc_key!("parts", built_in);
register_doc_key!("face", built_in);
register_doc_key!("weight_brushes", built_in);
register_doc_key!("variants", built_in);
register_doc_key!("colourways", built_in);
register_doc_key!("rig", built_in);
register_doc_key!("look", built_in);
register_doc_key!("licence", built_in);
register_doc_key!("sources", built_in);
register_doc_key!("applied", built_in);
register_doc_key!("poses", built_in);
register_doc_key!("emotes", built_in);

/// The registered keys, resolved once: usable ones by key, and the collisions.
struct Resolved {
    usable: BTreeMap<&'static str, &'static DocKeyRegistration>,
    sibling_keys: Vec<&'static str>,
    collisions: Vec<&'static str>,
}

fn resolved() -> &'static Resolved {
    static RESOLVED: OnceLock<Resolved> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let mut by_key: BTreeMap<&'static str, Vec<&'static DocKeyRegistration>> = BTreeMap::new();
        for registration in inventory::iter::<DocKeyRegistration> {
            by_key.entry(registration.key).or_default().push(registration);
        }
        let (mut usable, mut collisions) = (BTreeMap::new(), Vec::new());
        for (key, registrations) in by_key {
            let built_in = BUILT_IN_NODE_KEYS.contains(&key) || BUILT_IN_SIBLING_KEYS.contains(&key);
            match registrations.as_slice() {
                [only] if !built_in => {
                    usable.insert(key, *only);
                }
                _ => collisions.push(key),
            }
        }
        // Sorted after the built-ins: the same list, and the same errors, in every binary.
        let sibling_keys = BUILT_IN_SIBLING_KEYS.iter().copied().chain(usable.keys().copied()).collect();
        Resolved { usable, sibling_keys, collisions }
    })
}

/// Every sibling key: the built-in ones, then the usable registered ones in sorted order. Collisions are not in it.
pub fn sibling_keys() -> &'static [&'static str] {
    &resolved().sibling_keys
}

/// True if `key` is a node key (a field of [`CharacterDef`](crate::CharacterDef)).
pub fn is_node_key(key: &str) -> bool {
    BUILT_IN_NODE_KEYS.contains(&key)
}

/// True if `key` is a usable sibling key.
pub fn is_sibling_key(key: &str) -> bool {
    BUILT_IN_SIBLING_KEYS.contains(&key) || resolved().usable.contains_key(key)
}

/// The keys that are registered more than once or under a built-in key's name, sorted. Empty in a healthy binary.
pub fn collisions() -> &'static [&'static str] {
    &resolved().collisions
}

/// The error for `key` if it is a collision.
///
/// # Errors
/// [`DocKeyError::Collision`] naming the key.
pub fn check(key: &str) -> Result<(), DocKeyError> {
    if collisions().contains(&key) { Err(DocKeyError::Collision(key.to_owned())) } else { Ok(()) }
}

/// The path of the registered key `key` for character `id`: `None` if no crate registered it (the built-in keys have
/// their own functions in [`paths`](crate::paths)).
///
/// # Errors
/// [`DocKeyError::Collision`] if the key is a collision.
pub fn path(key: &str, id: &CharId) -> Result<Option<DocPath>, DocKeyError> {
    check(key)?;
    Ok(resolved().usable.get(key).map(|r| (r.path)(id)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_lists_do_not_overlap() {
        assert!(BUILT_IN_SIBLING_KEYS.iter().all(|k| !BUILT_IN_NODE_KEYS.contains(k)));
    }

    #[test]
    fn the_listing_records_each_built_in_key_once() {
        let mut recorded: Vec<String> = omuzaki_core::link::registered()
            .into_iter()
            .filter(|r| r.kind == "doc_key" && r.package == "omuzaki-char-api")
            .map(|r| r.name.trim_matches('"').to_owned())
            .collect();
        recorded.sort();
        let mut table: Vec<String> =
            BUILT_IN_NODE_KEYS.iter().chain(&BUILT_IN_SIBLING_KEYS).map(|k| (*k).to_owned()).collect();
        table.sort();
        assert_eq!(recorded, table);
    }

    #[test]
    fn built_in_keys_come_first_and_unregistered_keys_are_unknown() {
        assert_eq!(&sibling_keys()[..BUILT_IN_SIBLING_KEYS.len()], &BUILT_IN_SIBLING_KEYS);
        assert!(!is_sibling_key("nope") && !is_node_key("nope"));
        assert_eq!(path("nope", &CharId::new("mira").expect("a valid id")), Ok(None));
        assert_eq!(collisions(), [] as [&str; 0]);
    }
}
