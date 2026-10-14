//! `licence.set_source`: record where one part came from and what its rights are, by hand.
//!
//! A part's source row (`characters/<id>/sources/<part>`) is what `licence.derive` joins, and a part without one counts
//! as the most restrictive part possible. The library and upload ops write the row for the parts they bring in; this is
//! how a person or an agent records one for a part nothing wrote a row for (a part added by hand, an old project).
//!
//! The row passes the checks of the file format ([`PartSource::check`]) and a refusal names the exact field
//! (`source/provenance/licence`). Nothing here widens a right on its own: the row is stored as given, the terms it
//! leaves out stay the most restrictive value, and `licence.set` still refuses a character record that grants more than
//! the rows allow. The edit is undoable.

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_io_api::UseTerms;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::Commercial;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::derive::no_such_character;
use super::source::{PartSource, row_patch, source_path};
use super::store;
use crate::gather::avatar::record_path;

/// Records the source row of one part of a character: `characters/<id>/sources/<part>`, replacing any row there.
///
/// The row says where the part came from (`provenance`: its source, author, SPDX `licence`, the `attribution` that must
/// travel with it, `commercial` status, `consent` and `private_test` flag) and, optionally, the `terms` it was
/// licensed under (the keys of the VRM `meta`; what you leave out stays the most restrictive value) and who marked it
/// as the project's own (`owned.asserted_by`). Record only what is true: `commercial` is `unknown` until someone has
/// read the licence, and a part with no row counts as the most restrictive part possible. After recording, run
/// `licence.derive` to see what the character's parts now allow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicenceSetSource {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part the row is about. Must exist; a row for a part the character does not have is refused.
    pub part: PartId,
    /// The row: `schema` 1, a `provenance` table and, optionally, `terms` and `owned`.
    pub source: PartSource,
}

/// A commercial status as the files spell it.
fn status(commercial: Commercial) -> &'static str {
    match commercial {
        Commercial::Ok => "ok",
        Commercial::No => "no",
        Commercial::Unknown => "unknown",
    }
}

/// What the person should know about the row they stored, beyond what it says.
fn notes(source: &PartSource, record_stored: bool) -> Vec<String> {
    let provenance = &source.provenance;
    let mut notes = Vec::new();
    let grants = source.terms.as_ref().is_some_and(|terms| *terms != UseTerms::default());
    if grants && provenance.private_test {
        notes.push(
            "the row is private test material, so its `terms` are ignored: it carries only the most restrictive terms"
                .to_owned(),
        );
    } else if grants && provenance.commercial != Commercial::Ok {
        notes.push(format!(
            "the `commercial` status is `{}`, not `ok`, so the `terms` are ignored: the part carries only the most \
             restrictive terms until the licence is established",
            status(provenance.commercial)
        ));
    }
    if provenance.private_test && source.owned.is_some() {
        notes.push(
            "`owned` does not waive private test material: the part can never be exported for any purpose but private"
                .to_owned(),
        );
    }
    if record_stored {
        notes.push(
            "the character has a stored licence record, and this row can change what its parts allow: run \
             licence.describe with purpose publish to see whether the record still holds"
                .to_owned(),
        );
    }
    notes
}

impl Op for LicenceSetSource {
    fn meta() -> OpMeta {
        OpMeta {
            name: "licence.set_source",
            summary: "Record where one part of a character came from and what its rights are (its source row). \
                      Nothing is assumed beyond what the row says.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let character =
            ctx.doc.get(&paths::character(&self.character)).ok_or_else(|| no_such_character(&self.character))?;
        let known: Vec<&str> = match character.get("parts") {
            Some(serde_json::Value::Object(parts)) => parts.keys().map(String::as_str).collect(),
            _ => Vec::new(),
        };
        if !known.contains(&self.part.as_str()) {
            let mut error = OpError::not_found(format!("character `{}` has no part `{}`", self.character, self.part));
            error.field = Some("part".to_owned());
            let listed = if known.is_empty() { "none".to_owned() } else { known.join(", ") };
            return Err(error.hint(format!("the parts of `{}` are: {listed}", self.character)));
        }
        if let Err(fault) = self.source.check() {
            return Err(OpError::invalid(format!("source/{}", fault.field), fault.message)
                .hint("a source row is `schema = 1`, a `provenance` table and, optionally, `terms` and `owned`"));
        }
        let edit = row_patch(ctx, &self.character, &self.part, &self.source)?;
        let path = source_path(&self.character, &self.part);
        let summary = json!({
            "character": self.character,
            "part": self.part,
            "path": path.to_string(),
            "changed": !edit.is_empty(),
            "replaced": ctx.doc.get(&path).is_some(),
            "source": store::node_of(&self.source, "the source row")?,
            "notes": notes(&self.source, ctx.doc.get(&record_path(&self.character)).is_some()),
        });
        Ok(Plan { output: Some(summary), ..Plan::edit(edit) })
    }
}

register_op!(LicenceSetSource);
