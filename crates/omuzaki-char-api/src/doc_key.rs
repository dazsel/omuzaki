//! Document keys: the keys of a character node in the project (`characters/<id>/<key>`), as an open registry (ADR 0054
//! §5).
//!
//! A character's node holds the keys of its own definition (node keys: exactly the fields of
//! [`CharacterDef`](crate::CharacterDef), read into it) and the roots of its other files (sibling keys, each a
//! sub-tree with its own contract). A new file of the character is a new sibling key in the file that owns it, plus
//! [`register_doc_key!`]; nobody edits a central list. A sibling key's path is `characters/<id>/<key>`, so the key is
//! also the name of a file or folder in the project, and must be a slug.
//! [`CharacterDef::from_node`](crate::CharacterDef::from_node) and [`to_node`](crate::CharacterDef::to_node) leave
//! sibling keys alone and read [`sibling_keys`] to accept them.
//!
//! A key can be registered once, and it must be a slug. A registration that breaks either rule is refused, with an error
//! that names it ([`refused`]), so no binary picks a winner by link order. Only the refused registration is discarded: a
//! built-in key that another crate registered under its name stays a sibling key, listed once by [`sibling_keys`]. The
//! refused registrations share one list because they share one outcome: each has a reason, and none adds a sibling key.
//!
//! The keys this crate's own types own, and the sibling keys whose owners are not linked by every binary that parses a
//! character, are the built-in table ([`BUILT_IN_NODE_KEYS`], [`BUILT_IN_SIBLING_KEYS`]), so validation never depends
//! on what a binary linked (ADR 0015). Registered keys add to it. The built-in keys are also recorded for the listing of
//! registrations (`omuzaki registrations`), so one list shows every key; that record needs this crate's `link()`, the
//! table does not.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use omuzaki_core::id::{Slug, SlugError};
use omuzaki_doc_api::DocPath;

use crate::ids::CharId;
use crate::paths;

/// One registered document key, collected by [`register_doc_key!`]. A registration that is not refused is a sibling key.
pub struct DocKeyRegistration {
    /// The key, as it is spelt in the project and in saved files. Its path is `characters/<id>/<key>`.
    pub key: &'static str,
}

inventory::collect!(DocKeyRegistration);

/// Why a registered key cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DocKeyError {
    /// The key is registered more than once, or has the name of a built-in key.
    #[error("document key `{0}` is registered more than once or collides with a built-in key")]
    Collision(String),
    /// The key isn't a slug, so the project could not store it as a file or folder name.
    #[error("document key `{key}` can't name a file or folder: {why}")]
    NotASlug {
        /// The key.
        key: String,
        /// The slug rule it breaks.
        why: SlugError,
    },
}

/// The keys of `characters/<id>` that make up a [`CharacterDef`](crate::CharacterDef) (`character.toml` and
/// `face.toml`): its fields. A new field is added here beside the struct.
pub const BUILT_IN_NODE_KEYS: [&str; 8] =
    ["name", "template", "seed", "palette", "skin_tone", "parts", "face", "weight_brushes"];

/// The built-in sibling keys: the roots of the other files of the character. They are `variants`, `colourways`, `rig`,
/// `look`, `licence`, `sources`, `applied`, `poses`, `emotes`, `avatar`, `voice`, `voice-assignment` and `likeness`.
///
/// Nine are owned by other crates: `licence` by `omuzaki-io-ops`, `sources` by `omuzaki-char`, `applied` and `emotes`
/// by `omuzaki-library-api`, `poses` by `omuzaki-anim`, `avatar` (an imported character's `AvatarDef`) by
/// `omuzaki-avatar-api`, `voice` (the effect presets, `voice.toml`) by `omuzaki-audio-fx`, `voice-assignment` (which
/// voice the character speaks in) by `omuzaki-speech`, and `likeness` (the character's likeness targets, linked by
/// hash) by `omuzaki-measure`. They stay in this table because no crate that parses a character links all of those
/// owners, so they cannot register.
pub const BUILT_IN_SIBLING_KEYS: [&str; 13] = [
    "variants",
    "colourways",
    "rig",
    "look",
    "licence",
    "sources",
    "applied",
    "poses",
    "emotes",
    "avatar",
    "voice",
    "voice-assignment",
    "likeness",
];

/// Registers a sibling document key next to the file of the character that owns it (ADR 0054 §5). The key is the last
/// segment of the path, `characters/<id>/<key>`, so it must be a slug.
///
/// ```ignore
/// omuzaki_char_api::register_doc_key!("hats");
/// ```
///
/// The marker form `register_doc_key!("name", @built_in)` is for this module only: it records the key for the listing and
/// registers nothing. Its second argument is `@built_in`, which the one-argument form never matches.
#[macro_export]
macro_rules! register_doc_key {
    ($key:literal, @built_in) => {
        $crate::__record_registration!("doc_key", $key);
    };
    ($key:literal) => {
        $crate::__inventory::submit! {
            $crate::DocKeyRegistration { key: $key }
        }
        $crate::__record_registration!("doc_key", $key);
    };
}

// The built-in keys, listed beside the registered ones (ADR 0054 §5). The record is in a binary exactly when this crate's
// `link()` is called; the tables above are what validation reads, so it never depends on that. The test below keeps the
// two in step. `@built_in` is a second argument, so no one-argument registration can match it.
register_doc_key!("name", @built_in);
register_doc_key!("template", @built_in);
register_doc_key!("seed", @built_in);
register_doc_key!("palette", @built_in);
register_doc_key!("skin_tone", @built_in);
register_doc_key!("parts", @built_in);
register_doc_key!("face", @built_in);
register_doc_key!("weight_brushes", @built_in);
register_doc_key!("variants", @built_in);
register_doc_key!("colourways", @built_in);
register_doc_key!("rig", @built_in);
register_doc_key!("look", @built_in);
register_doc_key!("licence", @built_in);
register_doc_key!("sources", @built_in);
register_doc_key!("applied", @built_in);
register_doc_key!("poses", @built_in);
register_doc_key!("emotes", @built_in);
register_doc_key!("avatar", @built_in);
register_doc_key!("voice", @built_in);
register_doc_key!("voice-assignment", @built_in);
register_doc_key!("likeness", @built_in);

/// The registered keys, resolved once: the usable ones, and the refused ones with the reason for each.
struct Resolved {
    usable: BTreeSet<&'static str>,
    sibling_keys: Vec<&'static str>,
    refused: BTreeMap<&'static str, DocKeyError>,
}

fn resolved() -> &'static Resolved {
    static RESOLVED: OnceLock<Resolved> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        let mut by_key: BTreeMap<&'static str, usize> = BTreeMap::new();
        for registration in inventory::iter::<DocKeyRegistration> {
            *by_key.entry(registration.key).or_default() += 1;
        }
        let (mut usable, mut refused) = (BTreeSet::new(), BTreeMap::new());
        for (key, count) in by_key {
            let built_in = BUILT_IN_NODE_KEYS.contains(&key) || BUILT_IN_SIBLING_KEYS.contains(&key);
            // A key that isn't a slug gives its own reason first: the project can't store it whatever else is true.
            let refusal = match Slug::new(key) {
                Err(why) => Some(DocKeyError::NotASlug { key: key.to_owned(), why }),
                Ok(_) if built_in || count > 1 => Some(DocKeyError::Collision(key.to_owned())),
                Ok(_) => None,
            };
            match refusal {
                Some(refusal) => {
                    refused.insert(key, refusal);
                }
                None => {
                    usable.insert(key);
                }
            }
        }
        // Sorted after the built-ins: the same list, and the same errors, in every binary.
        let sibling_keys = BUILT_IN_SIBLING_KEYS.iter().copied().chain(usable.iter().copied()).collect();
        Resolved { usable, sibling_keys, refused }
    })
}

/// Every sibling key: the built-in ones, then the usable registered ones in sorted order. A refused registration adds
/// nothing, and a built-in key stays listed once whatever is registered under its name.
pub fn sibling_keys() -> &'static [&'static str] {
    &resolved().sibling_keys
}

/// True if `key` is a node key (a field of [`CharacterDef`](crate::CharacterDef)).
pub fn is_node_key(key: &str) -> bool {
    BUILT_IN_NODE_KEYS.contains(&key)
}

/// True if `key` is a usable sibling key.
pub fn is_sibling_key(key: &str) -> bool {
    BUILT_IN_SIBLING_KEYS.contains(&key) || resolved().usable.contains(key)
}

/// The registered keys that are refused, sorted by key, each with its reason. Empty in a healthy binary.
pub fn refused() -> &'static BTreeMap<&'static str, DocKeyError> {
    &resolved().refused
}

/// The error for `key` if it is refused.
///
/// # Errors
/// The [`DocKeyError`] for the reason `key` is refused.
pub fn check(key: &str) -> Result<(), DocKeyError> {
    match resolved().refused.get(key) {
        Some(refusal) => Err(refusal.clone()),
        None => Ok(()),
    }
}

/// The path of the registered key `key` for character `id`: `characters/<id>/<key>`. `None` if no crate registered it.
/// A built-in name with a colliding registration is refused here, so it gets the error, not a path. A built-in name with
/// no registration gives `None`, like any other unregistered key; the built-in keys have their own functions in
/// [`paths`](crate::paths).
///
/// # Errors
/// The [`DocKeyError`] if the key is refused.
pub fn path(key: &str, id: &CharId) -> Result<Option<DocPath>, DocKeyError> {
    check(key)?;
    Ok(resolved().usable.contains(key).then(|| paths::character(id).join(key)))
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
        assert!(refused().is_empty());
    }
}
