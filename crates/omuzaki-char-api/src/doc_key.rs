//! Document keys: the keys of a character node in the project (`characters/<id>/<key>`), as an open registry (ADR 0054
//! §5).
//!
//! A character's node holds the keys of its own definition (node keys, read into a
//! [`CharacterDef`](crate::CharacterDef)) and the roots of its other files (sibling keys, each a sub-tree with its own
//! contract). A new file of the character is a new key in the file that owns it, plus [`register_doc_key!`]; nobody edits
//! [`CharacterDef`](crate::CharacterDef) or a central list. [`CharacterDef::from_node`](crate::CharacterDef::from_node)
//! and [`to_node`](crate::CharacterDef::to_node) read [`node_keys`] and [`sibling_keys`].
//!
//! The keys this crate's own types own are the built-in table ([`BUILT_IN_NODE_KEYS`], [`BUILT_IN_SIBLING_KEYS`]), so a
//! binary that links only this crate validates and saves them. Registered keys add to it.

use omuzaki_doc_api::DocPath;

use crate::ids::CharId;

/// Which part of a character node a key belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DocKeyKind {
    /// A key of the definition itself: read into [`CharacterDef`](crate::CharacterDef) by `from_node` and written by
    /// `to_node`. It needs a field of that name.
    Node,
    /// The root of another file of the character: `from_node` skips it and `to_node` leaves it alone.
    Sibling,
}

/// One registered document key, collected by [`register_doc_key!`].
pub struct DocKeyRegistration {
    /// The key, as it is spelt in the project and in saved files.
    pub key: &'static str,
    /// Whether it is part of the definition or the root of another file.
    pub kind: DocKeyKind,
    /// The path of the key in the document, for the character `id`.
    pub path: fn(&CharId) -> DocPath,
}

inventory::collect!(DocKeyRegistration);

/// The keys of `characters/<id>` that make up a [`CharacterDef`](crate::CharacterDef) (`character.toml` and
/// `face.toml`). Their order is the order the definition is documented in.
pub const BUILT_IN_NODE_KEYS: [&str; 8] =
    ["name", "template", "seed", "palette", "skin_tone", "parts", "face", "weight_brushes"];

/// The other keys of `characters/<id>` whose owners are types of this crate or files read by crates that do not link
/// the registering crates: `variants`, `colourways`, `rig`, `look`, `licence`, `sources`, `applied`, `poses` and
/// `emotes`.
pub const BUILT_IN_SIBLING_KEYS: [&str; 9] =
    ["variants", "colourways", "rig", "look", "licence", "sources", "applied", "poses", "emotes"];

/// Registers a document key next to the file of the character that owns it (ADR 0054 §5). `kind` is `Node` or
/// `Sibling` ([`DocKeyKind`]); `path` is a `fn(&CharId) -> DocPath`.
///
/// ```ignore
/// omuzaki_char_api::register_doc_key!("hats", Sibling, hats_path);
/// ```
#[macro_export]
macro_rules! register_doc_key {
    ($key:literal, $kind:ident, $path:path) => {
        $crate::__inventory::submit! {
            $crate::DocKeyRegistration { key: $key, kind: $crate::DocKeyKind::$kind, path: $path }
        }
        $crate::__record_registration!("doc_key", $key);
    };
}

fn registered(kind: DocKeyKind) -> impl Iterator<Item = &'static DocKeyRegistration> {
    inventory::iter::<DocKeyRegistration>.into_iter().filter(move |r| r.kind == kind)
}

fn keys(built_in: &'static [&'static str], kind: DocKeyKind) -> Vec<&'static str> {
    let mut out = built_in.to_vec();
    let mut extra: Vec<&'static str> = registered(kind).map(|r| r.key).filter(|k| !built_in.contains(k)).collect();
    // Registration order depends on the linker; sorted keys give the same list, and the same errors, in every binary.
    extra.sort_unstable();
    extra.dedup();
    out.extend(extra);
    out
}

/// Every node key: the built-in ones, then the registered ones in sorted order.
pub fn node_keys() -> Vec<&'static str> {
    keys(&BUILT_IN_NODE_KEYS, DocKeyKind::Node)
}

/// Every sibling key: the built-in ones, then the registered ones in sorted order.
pub fn sibling_keys() -> Vec<&'static str> {
    keys(&BUILT_IN_SIBLING_KEYS, DocKeyKind::Sibling)
}

/// True if `key` is a node key.
pub fn is_node_key(key: &str) -> bool {
    BUILT_IN_NODE_KEYS.contains(&key) || registered(DocKeyKind::Node).any(|r| r.key == key)
}

/// True if `key` is a sibling key.
pub fn is_sibling_key(key: &str) -> bool {
    BUILT_IN_SIBLING_KEYS.contains(&key) || registered(DocKeyKind::Sibling).any(|r| r.key == key)
}

/// The path of the registered key `key` for character `id`, or `None` if no crate registered it. (The built-in keys
/// have their own functions in [`paths`].)
pub fn path(key: &str, id: &CharId) -> Option<DocPath> {
    inventory::iter::<DocKeyRegistration>.into_iter().find(|r| r.key == key).map(|r| (r.path)(id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn built_in_lists_do_not_overlap() {
        assert!(BUILT_IN_SIBLING_KEYS.iter().all(|k| !BUILT_IN_NODE_KEYS.contains(k)));
    }

    #[test]
    fn built_in_keys_come_first_and_unregistered_keys_are_unknown() {
        assert_eq!(&node_keys()[..BUILT_IN_NODE_KEYS.len()], &BUILT_IN_NODE_KEYS);
        assert!(!is_sibling_key("nope") && !is_node_key("nope"));
        assert!(path("nope", &CharId::new("mira").expect("a valid id")).is_none());
    }
}
