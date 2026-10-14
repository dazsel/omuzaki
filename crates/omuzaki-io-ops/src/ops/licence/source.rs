//! Where a part came from and what its rights are: the per-part source rows `licence.derive` joins.
//!
//! Each part of a character can have a **source row** at `characters/<id>/sources/<part>`, a sibling sub-tree of the
//! character node like `licence`, `variants` and `look` (a part cannot hold it: `PartDef` rejects unknown keys). The ops
//! that bring a part in write the row: `licence.set_source` records one by hand ([`row_patch`] is the one way a row is
//! written, so the ops that bring a part in use it too), `part.duplicate` copies it, `part.remove` removes it. Nothing
//! here invents a row. A part without one is treated as the most restrictive part possible (see [`super::derive`]), so a
//! missing row can only narrow what a character may do, never widen it.

use std::fmt;

use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_doc_api::{DocPath, Patch};
use omuzaki_io_api::UseTerms;
use omuzaki_ops_api::{OpCtx, OpError};
use omuzaki_store_api::Provenance;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::store;
use crate::gather::avatar::{RecordError, SCHEMA, check_schema};

/// `characters/<id>/sources`: every part's [`PartSource`], by part id.
pub fn sources_path(id: &CharId) -> DocPath {
    paths::character(id).join("sources")
}

/// `characters/<id>/sources/<part>`: the [`PartSource`] of one part.
pub fn source_path(id: &CharId, part: &PartId) -> DocPath {
    sources_path(id).join(part.as_str())
}

/// What the project records about the rights to one part (spec §7, §28): the library item's or the upload's provenance
/// and the terms it was licensed under.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PartSource {
    /// The row's version, [`SCHEMA`]. Required, like the record's.
    pub schema: u32,
    /// The part's provenance: what produced it, who made it, its SPDX licence, attribution, commercial status, consent
    /// and the private-test flag, exactly as the store or the library holds them.
    pub provenance: Provenance,
    /// The VRM-vocabulary terms the part was licensed under. What the row leaves out is the most restrictive value of
    /// every flag, never a looser default: a `commercial = "ok"` licence that says nothing about profit still derives
    /// `personalNonProfit`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<UseTerms>,
    /// Set when the part is the project's own (one of its own library items, or the person's own upload marked as
    /// self-authored): the record that holds it may then grant more than the terms above, on this person's word.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owned: Option<Ownership>,
}

/// Who said a part is the project's own.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Ownership {
    /// The person (or account) that marked the part as the project's own. Never blank: an assertion nobody made is not
    /// one.
    pub asserted_by: String,
}

/// What is wrong with a [`PartSource`] that has the right shape: the field at fault, as a path into the row, and a
/// message that says how to fix it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFault {
    /// The field at fault, as a path into the row (`provenance/licence`, `owned/asserted_by`, `schema`).
    pub field: &'static str,
    /// What is wrong and how to fix it.
    pub message: String,
}

impl fmt::Display for SourceFault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl PartSource {
    /// A row of the current [`SCHEMA`] with no terms and no ownership.
    pub fn new(provenance: Provenance) -> Self {
        Self { schema: SCHEMA, provenance, terms: None, owned: None }
    }

    /// Reads a row from its document node. The `schema` is checked first, as for the record.
    ///
    /// # Errors
    /// [`RecordError::Schema`] for a missing or unknown `schema`, [`RecordError::Invalid`] for anything else,
    /// including a blank licence or an ownership assertion with a blank name.
    pub fn from_node(node: &Value) -> Result<Self, RecordError> {
        check_schema(node)?;
        let source = Self::deserialize(node).map_err(|e| RecordError::Invalid(e.to_string()))?;
        source.check().map_err(|fault| RecordError::Invalid(fault.message))?;
        Ok(source)
    }

    /// The checks a row must pass beyond its shape, the same whether it is read from a file ([`PartSource::from_node`])
    /// or handed to `licence.set_source`: the `schema` is the one this build writes, the licence is not blank (give
    /// `LicenseRef-Unknown` when nobody knows) and an ownership assertion names who made it.
    ///
    /// # Errors
    /// The first [`SourceFault`], naming the field.
    pub fn check(&self) -> Result<(), SourceFault> {
        if self.schema != SCHEMA {
            return Err(SourceFault {
                field: "schema",
                message: format!("`schema` is {}, but this build reads only schema {SCHEMA}", self.schema),
            });
        }
        if self.provenance.licence.trim().is_empty() {
            return Err(SourceFault {
                field: "provenance/licence",
                message: "`provenance.licence` is blank: give an SPDX id, or `LicenseRef-Unknown` if nobody knows"
                    .to_owned(),
            });
        }
        match &self.owned {
            Some(owner) if owner.asserted_by.trim().is_empty() => Err(SourceFault {
                field: "owned/asserted_by",
                message: "`owned.asserted_by` is blank: name the person who marked the part as the project's own"
                    .to_owned(),
            }),
            _ => Ok(()),
        }
    }
}

/// The patch that records `source` as the row of `part` in `character`, replacing any row there; empty if the row is
/// already exactly that.
///
/// This is the one way a row is written. It stores the row without nulls (as the project files hold it) and creates the
/// character's `sources` table when the document lacks it, in the same patch, so undo removes everything the edit
/// created. It does not check that the character or the part exist (the op that calls it does, with the wording that
/// fits it) and it does not widen anything: the row is stored as given, and a part without one stays the most
/// restrictive.
///
/// # Errors
/// `internal` if the row cannot be serialised.
pub fn row_patch(ctx: &OpCtx<'_>, character: &CharId, part: &PartId, source: &PartSource) -> Result<Patch, OpError> {
    let path = source_path(character, part);
    let table = sources_path(character);
    let mut edit = Patch::new();
    if ctx.doc.get(&table).is_none() {
        edit = edit.with(table, None, Some(json!({})));
    }
    let after = store::node_of(source, "the source row")?;
    let before = ctx.doc.get(&path).cloned();
    if before.as_ref() != Some(&after) {
        edit = edit.with(path, before, Some(after));
    }
    Ok(edit)
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::{Commercial, Source};
    use serde_json::json;

    use super::*;

    fn provenance() -> Provenance {
        Provenance {
            source: Source::Authored,
            author: None,
            licence: "CC0-1.0".into(),
            attribution: None,
            commercial: Commercial::Ok,
            consent: None,
            private_test: false,
        }
    }

    #[test]
    fn rows_live_beside_the_record_in_the_characters_own_node() {
        let id = CharId::new("mira").unwrap();
        assert_eq!(sources_path(&id).to_string(), "characters/mira/sources");
        assert_eq!(source_path(&id, &PartId::new("hair").unwrap()).to_string(), "characters/mira/sources/hair");
    }

    #[test]
    fn a_row_round_trips_and_leaves_out_what_it_does_not_hold() {
        let row = PartSource::new(provenance());
        let node = serde_json::to_value(&row).unwrap();
        assert!(node.get("terms").is_none() && node.get("owned").is_none(), "{node}");
        assert_eq!(PartSource::from_node(&node).unwrap(), row);
    }

    #[test]
    fn the_schema_is_checked_before_anything_else() {
        let mut node = serde_json::to_value(PartSource::new(provenance())).unwrap();
        node["schema"] = json!(2);
        node["surprise"] = json!(true);
        assert!(matches!(PartSource::from_node(&node), Err(RecordError::Schema { .. })));
    }

    #[test]
    fn a_blank_licence_is_refused() {
        let mut row = PartSource::new(provenance());
        row.provenance.licence = " ".into();
        let RecordError::Invalid(why) = PartSource::from_node(&serde_json::to_value(row).unwrap()).unwrap_err() else {
            panic!("invalid")
        };
        assert!(why.contains("provenance.licence"), "{why}");
    }

    #[test]
    fn an_unknown_key_or_a_blank_assertion_is_refused() {
        let mut node = serde_json::to_value(PartSource::new(provenance())).unwrap();
        node["surprise"] = json!(true);
        assert!(matches!(PartSource::from_node(&node), Err(RecordError::Invalid(_))));
        node.as_object_mut().unwrap().remove("surprise");
        node["owned"] = json!({ "asserted_by": "  " });
        let RecordError::Invalid(why) = PartSource::from_node(&node).unwrap_err() else { panic!("invalid") };
        assert!(why.contains("owned.asserted_by"), "{why}");
    }
}
