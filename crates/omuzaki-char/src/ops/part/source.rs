//! A part's source row goes where the part goes.
//!
//! `characters/<id>/sources/<part>` records where a part came from and what its rights are (`licence.set_source`,
//! `docs/design/io-api.md` D10). This crate never reads a row, it only moves it, so the rows stay right when parts
//! change: `part.duplicate` copies the row (the copy is the same asset under the same licence), `part.remove` takes it
//! away, and `part.add` clears any row left under the new id, so a new part never inherits the rights of a part that was
//! once called the same. A part with no row counts as the most restrictive part possible, so each of these can only
//! narrow what a character may do, never widen it, unless the row really does belong to the part.
//!
//! The row is an opaque node here (the type is `omuzaki-io-ops`' and implementations may not depend on each other); only
//! its place is shared, and the test below pins the key against `CharacterDef::SIBLING_KEYS`.

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{Change, DocPath};
use omuzaki_ops_api::OpCtx;

use crate::ops::character::access;

/// The key of the character node that holds every part's source row.
const SOURCES: &str = "sources";

/// `characters/<id>/sources/<part>`.
fn row_path(character: &CharId, part: &PartId) -> DocPath {
    paths::character(character).join(SOURCES).join(part.as_str())
}

/// The change that makes the row of `to` the same as the row of `from`, if that changes anything: copies the row, or
/// clears a leftover row under `to` when `from` has none, so the copy never keeps rights that were not its original's.
pub(super) fn carry(ctx: &OpCtx<'_>, character: &CharId, from: &PartId, to: &PartId) -> Option<Change> {
    let row = ctx.doc.get(&row_path(character, from)).cloned();
    (ctx.doc.get(&row_path(character, to)) != row.as_ref()).then(|| access::change(ctx, row_path(character, to), row))
}

/// The change that removes the row of `part`, if it has one.
pub(super) fn discard(ctx: &OpCtx<'_>, character: &CharId, part: &PartId) -> Option<Change> {
    let path = row_path(character, part);
    ctx.doc.get(&path).is_some().then(|| access::change(ctx, path, None))
}

#[cfg(test)]
mod tests {
    use omuzaki_char_api::CharacterDef;

    use super::*;

    #[test]
    fn the_rows_live_under_a_sibling_key_of_the_character_node() {
        assert!(CharacterDef::SIBLING_KEYS.contains(&SOURCES), "`from_node` must skip the rows");
        let (character, part) = (CharId::new("mira").unwrap(), PartId::new("hair").unwrap());
        assert_eq!(row_path(&character, &part).to_string(), "characters/mira/sources/hair");
    }
}
