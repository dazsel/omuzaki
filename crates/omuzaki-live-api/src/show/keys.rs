//! The keys of a show node (`shows/<id>/<key>`), as an open registry (ADR 0054 §5; design D21).
//!
//! The show's own keys (`show`, `automations`) and the library's `applied` are built in. Another feature that keeps
//! files under a show (overlay layouts, outputs) registers its key with [`register_show_key!`] next to its owner,
//! which gives the key its owner's check. Three rules keep the outcome the same in every binary, whatever it links:
//! 1. **Naming decides ownership.** A key is named after its owner crate: `omuzaki-overlay` may register `overlay` or
//!    `overlay-<name>`, nothing else, checked when the macro expands. Crate names are unique, so no two crates can
//!    claim one key. `plugin` and `plugin-*` are the plugins' (`plugin-<plugin>-<name>`, given out by the plugin
//!    host).
//! 2. **Every binary keeps every feature key.** [`classify`] accepts any slug that isn't built in and isn't within two
//!    edits of a built-in key; [`ShowDef`](super::ShowDef) holds it as an opaque sub-tree and writes it back unchanged.
//! 3. **Content is checked only by its owner.** A registered key's check runs in [`ShowDef::check`](super::ShowDef::check);
//!    an unregistered one is reported "kept, not checked", never as an error.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use omuzaki_core::id::{Slug, SlugError};
use omuzaki_core::text::edit_distance;
use omuzaki_doc_api::{DocPath, Value};

use super::ShowDef;
use super::check::Finding;
use crate::ids::ShowId;

/// The built-in keys of a show node: [`ShowDef::NODE_KEYS`] and [`ShowDef::SIBLING_KEYS`].
pub const BUILT_IN: [&str; 3] = ["show", "automations", "applied"];

/// How far (in edits) a key may be from a built-in key and still be refused as its misspelling.
pub const MISSPELLING_DISTANCE: usize = 2;

/// One registered show key, collected by [`register_show_key!`].
#[derive(Debug)]
pub struct ShowKeyRegistration {
    /// The key, as spelt in the project. Its path is `shows/<id>/<key>`.
    pub key: &'static str,
    /// The package that registered it, which owns it.
    pub package: &'static str,
    /// The owner's check of the key's sub-tree, if it has one.
    pub check: Option<fn(&Value) -> Vec<Finding>>,
}

inventory::collect!(ShowKeyRegistration);

/// Why a key can't be used under a show.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum KeyError {
    /// The key isn't a slug, so the project could not store it as a file or folder name.
    #[non_exhaustive]
    #[error("show key `{key}` can't name a file or folder: {why}")]
    NotASlug {
        /// The key.
        key: String,
        /// The slug rule it breaks.
        why: SlugError,
    },
    /// The key is within [`MISSPELLING_DISTANCE`] edits of a built-in key: taken as a typo of it.
    #[non_exhaustive]
    #[error("unknown field `{key}` in a show, did you mean `{near}`?")]
    Misspelt {
        /// The key.
        key: String,
        /// The built-in key it resembles.
        near: &'static str,
    },
    /// The key is registered more than once, or by a name the naming rule doesn't give its package.
    #[error("show key `{0}` is registered more than once or collides with a built-in key")]
    Collision(String),
}

impl KeyError {
    /// `key` isn't a slug, for `why`.
    pub fn not_a_slug(key: impl Into<String>, why: SlugError) -> Self {
        KeyError::NotASlug { key: key.into(), why }
    }

    /// `key` is taken as a typo of the built-in `near`.
    pub fn misspelt(key: impl Into<String>, near: &'static str) -> Self {
        KeyError::Misspelt { key: key.into(), near }
    }
}

/// What a key of a show node is.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum KeyKind {
    /// One of [`BUILT_IN`].
    BuiltIn,
    /// A feature's key whose owner registered it in this binary.
    Registered(&'static ShowKeyRegistration),
    /// A feature's key whose owner this binary doesn't link: kept as it is, not checked.
    Unchecked,
}

/// True if `key` may be registered by `package` (rule 1): the crate's name without `omuzaki-`, alone or followed by
/// `-<name>`; a slug; never `plugin` or `plugin-*`. `const`, so [`register_show_key!`] checks it at compile time.
pub const fn named_for(key: &str, package: &str) -> bool {
    const PREFIX: &[u8] = b"omuzaki-";
    let (key, package) = (key.as_bytes(), package.as_bytes());
    if package.len() <= PREFIX.len() || !starts_with(package, PREFIX, 0) || !slug_bytes(key) {
        return false;
    }
    let stem_len = package.len() - PREFIX.len();
    if key.len() < stem_len || !starts_with(key, package, PREFIX.len()) {
        return false;
    }
    if key.len() > stem_len && (key[stem_len] != b'-' || key.len() == stem_len + 1) {
        return false;
    }
    !(starts_with(key, b"plugin", 0) && (key.len() == 6 || key[6] == b'-'))
}

/// True if `haystack` starts with `needle[from..]`.
const fn starts_with(haystack: &[u8], needle: &[u8], from: usize) -> bool {
    if haystack.len() < needle.len() - from {
        return false;
    }
    let mut i = from;
    while i < needle.len() {
        if haystack[i - from] != needle[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// The slug characters, checked byte by byte (the full rules, Windows names included, are [`Slug::new`]'s).
const fn slug_bytes(key: &[u8]) -> bool {
    if key.is_empty() || key.len() > Slug::MAX_LEN {
        return false;
    }
    let mut i = 0;
    while i < key.len() {
        let b = key[i];
        if !(b.is_ascii_lowercase() || b.is_ascii_digit() || (i > 0 && (b == b'-' || b == b'_'))) {
            return false;
        }
        i += 1;
    }
    true
}

/// Registers a key under a show next to the feature that owns it (ADR 0054 §5; design D21). The key must be named
/// after the registering crate ([`named_for`]); anything else fails to compile.
///
/// ```ignore
/// omuzaki_live_api::register_show_key!("overlay");                        // kept and written back, no check
/// omuzaki_live_api::register_show_key!("overlay-layouts", check = check_layouts);   // with the owner's check
/// ```
#[macro_export]
macro_rules! register_show_key {
    (@submit $key:literal, $check:expr) => {
        const _: () = ::core::assert!(
            $crate::show::keys::named_for($key, ::core::env!("CARGO_PKG_NAME")),
            "a show key is named after its crate: `omuzaki-<stem>` registers `<stem>` or `<stem>-<name>`"
        );
        $crate::__inventory::submit! {
            $crate::show::keys::ShowKeyRegistration {
                key: $key,
                package: ::core::env!("CARGO_PKG_NAME"),
                check: $check,
            }
        }
        $crate::__record_registration!("show_key", $key);
    };
    ($key:literal) => {
        $crate::register_show_key!(@submit $key, ::core::option::Option::None);
    };
    ($key:literal, check = $check:path) => {
        $crate::register_show_key!(@submit $key, ::core::option::Option::Some($check));
    };
}

/// The registered keys, resolved once: the usable ones and the refused ones with the reason for each.
struct Resolved {
    usable: BTreeMap<&'static str, &'static ShowKeyRegistration>,
    refused: BTreeMap<&'static str, KeyError>,
}

fn resolved() -> &'static Resolved {
    static RESOLVED: OnceLock<Resolved> = OnceLock::new();
    RESOLVED.get_or_init(|| {
        // Per key: the first registration, how many there are, and whether every one is named for its package.
        let mut by_key: BTreeMap<&'static str, (&'static ShowKeyRegistration, usize, bool)> = BTreeMap::new();
        for registration in inventory::iter::<ShowKeyRegistration> {
            let named = named_for(registration.key, registration.package);
            let entry = by_key.entry(registration.key).or_insert((registration, 0, true));
            entry.1 += 1;
            entry.2 &= named;
        }
        let (mut usable, mut refused) = (BTreeMap::new(), BTreeMap::new());
        for (key, (registration, count, named)) in by_key {
            match unregistered(key) {
                Err(why) => {
                    refused.insert(key, why);
                }
                Ok(()) if count > 1 || !named => {
                    refused.insert(key, KeyError::Collision(key.to_owned()));
                }
                Ok(()) => {
                    usable.insert(key, registration);
                }
            }
        }
        Resolved { usable, refused }
    })
}

/// The rules every feature key keeps whether or not it is registered: a slug, not built in, not a misspelt built-in.
fn unregistered(key: &str) -> Result<(), KeyError> {
    Slug::new(key).map_err(|why| KeyError::NotASlug { key: key.to_owned(), why })?;
    if BUILT_IN.contains(&key) {
        return Err(KeyError::Collision(key.to_owned()));
    }
    match BUILT_IN.iter().find(|built_in| edit_distance(key, built_in) <= MISSPELLING_DISTANCE) {
        Some(near) => Err(KeyError::Misspelt { key: key.to_owned(), near }),
        None => Ok(()),
    }
}

/// What `key` is under a show: built in, a registered feature key, or a feature key kept unchecked.
///
/// # Errors
/// The [`KeyError`] if the key can't be used: not a slug, a misspelt built-in, or a refused registration.
pub fn classify(key: &str) -> Result<KeyKind, KeyError> {
    if BUILT_IN.contains(&key) {
        return Ok(KeyKind::BuiltIn);
    }
    if let Some(refusal) = resolved().refused.get(key) {
        return Err(refusal.clone());
    }
    unregistered(key)?;
    Ok(resolved().usable.get(key).map_or(KeyKind::Unchecked, |r| KeyKind::Registered(r)))
}

/// The usable registered keys, sorted.
pub fn registered() -> BTreeSet<&'static str> {
    resolved().usable.keys().copied().collect()
}

/// The registered keys that are refused, sorted by key, each with its reason. Empty in a healthy binary.
pub fn refused() -> &'static BTreeMap<&'static str, KeyError> {
    &resolved().refused
}

/// `shows/<id>/<key>` for a feature key.
///
/// # Errors
/// The [`KeyError`] if the key can't be a feature key (built in keys have their own [`paths`](super::paths)).
pub fn path(key: &str, show: &ShowId) -> Result<DocPath, KeyError> {
    match classify(key)? {
        KeyKind::BuiltIn => Err(KeyError::Collision(key.to_owned())),
        KeyKind::Registered(_) | KeyKind::Unchecked => Ok(super::paths::show(show).join(key)),
    }
}

// Every built-in key is one of the show's own, keeping the two tables in step.
const _: () = assert!(BUILT_IN.len() == ShowDef::NODE_KEYS.len() + ShowDef::SIBLING_KEYS.len());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_named_after_its_crate() {
        assert!(named_for("overlay", "omuzaki-overlay") && named_for("overlay-layouts", "omuzaki-overlay"));
        for (key, package) in [
            ("overlays", "omuzaki-overlay"),
            ("overlay-", "omuzaki-overlay"),
            ("other", "omuzaki-overlay"),
            ("overlay", "other-overlay"),
            ("Overlay", "omuzaki-overlay"),
            ("plugin", "omuzaki-plugin"),
            ("plugin-x", "omuzaki-plugin"),
            ("x", "omuzaki-"),
        ] {
            assert!(!named_for(key, package), "{key} for {package}");
        }
    }

    #[test]
    fn feature_keys_are_kept_and_misspellings_refused() {
        assert!(matches!(classify("automations"), Ok(KeyKind::BuiltIn)));
        assert!(matches!(classify("overlay"), Ok(KeyKind::Unchecked)));
        assert!(matches!(classify("plugin-x-layout"), Ok(KeyKind::Unchecked)));
        assert_eq!(
            classify("automatons").unwrap_err(),
            KeyError::Misspelt { key: "automatons".into(), near: "automations" }
        );
        assert!(matches!(classify("shows"), Err(KeyError::Misspelt { near: "show", .. })));
        assert!(matches!(classify("Overlay"), Err(KeyError::NotASlug { .. })));
        assert!(refused().is_empty() && registered().is_empty());
        assert_eq!(path("overlay", &ShowId::new("main").unwrap()).unwrap().to_string(), "shows/main/overlay");
        assert!(path("show", &ShowId::new("main").unwrap()).is_err());
    }
}
