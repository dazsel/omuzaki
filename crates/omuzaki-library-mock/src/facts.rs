//! What the mock knows about a character, so `Query::compatible_with` can be answered. The `Library` trait is given
//! only a character id, so the facts are handed to the mock (see [`MockLibrary::set_characters`]) as the real library
//! is handed them by the op that reads the document. The type is the API's.
//!
//! [`MockLibrary::set_characters`]: crate::MockLibrary::set_characters

use omuzaki_library_api::{CharacterFacts, Compat, CompatStatus};

/// Whether an item with `compat` fits a character ([`CharacterFacts::status`]).
pub(crate) fn status(compat: &Compat, facts: &CharacterFacts) -> CompatStatus {
    facts.status(compat)
}
