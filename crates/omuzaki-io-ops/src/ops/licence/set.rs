//! `licence.set`: store a character's licence record, refusing one that grants more than its parts and application records allow.
//!
//! Rights are a legal surface, so the op is the one place a record can enter the document and it checks the whole
//! record first:
//! 1. the record is well formed (`schema`, a licence, a consent that names a person and a scope);
//! 2. it does not contradict itself: terms grant nothing while the commercial status is not `ok`, and private test
//!    material carries only the most restrictive terms (the rule the exporters enforce);
//! 3. it grants **no more than `licence.derive` says the parts and application records allow**, on any permission. The refusal names the field
//!    (`record/meta/commercialUsage`) and the contributions that set the limit.
//!
//! The one exception to 3 is a character whose parts and application records are **all the project's own** (each marked with who asserted it).
//! The record may then grant more, and the op writes who asserted it into the record (`owned`), replacing whatever the
//! caller passed there. The private-test flag is never waived, and neither is rule 2.

use omuzaki_char_api::CharId;
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::derive::{DerivedLicence, DerivedSummary, load};
use super::excess::{Excess, against_itself, against_parts};
use super::store;
use crate::gather::avatar::{LicenceRecord, OwnedBasis, SCHEMA, record_path};

/// Stores `record` as the licence record of `character` (`characters/<id>/licence`), replacing any record there.
///
/// The record may not grant more than `licence.derive` returns for the character, on any permission: the commercial
/// status, the private-test flag, the credit that must travel, or any flag of the file's terms. The exception is a
/// character whose parts are all marked as the project's own, in which case the record states who asserted it. Run
/// `licence.derive` first and store the record it returns, adding what only a person knows (authors, contact).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicenceSet {
    /// The character the record is for.
    pub character: CharId,
    /// The whole record: `schema` 1, a `provenance` table and, for a VRM, the `meta` keys.
    pub record: LicenceRecord,
}

/// The first problem that makes `record` unfit to store whatever the parts say, if any.
fn check_form(record: &LicenceRecord) -> Result<(), OpError> {
    if record.schema != SCHEMA {
        return Err(OpError::invalid(
            "record/schema",
            format!("`schema` is {}, but this build writes only schema {SCHEMA}", record.schema),
        )
        .hint("set `schema` to 1"));
    }
    let provenance = &record.provenance;
    if provenance.licence.trim().is_empty() {
        return Err(OpError::invalid("record/provenance/licence", "the licence is blank").hint(
            "give an SPDX expression (`CC-BY-4.0`) or `LicenseRef-<name>`; `licence.derive` returns the join of the parts'",
        ));
    }
    if let Some(consent) = &provenance.consent {
        for (field, text) in [("person", &consent.person), ("scope", &consent.scope)] {
            if text.trim().is_empty() {
                return Err(OpError::invalid(
                    format!("record/provenance/consent/{field}"),
                    format!("the consent has no {field}"),
                )
                .hint("a consent names who agreed and to what; leave `consent` out if nobody's likeness or voice is involved"));
            }
        }
    }
    Ok(())
}

/// The refusal for a record that contradicts its own provenance. `excess` is not empty.
fn contradiction(excess: &[Excess]) -> OpError {
    let sentences: Vec<String> = excess.iter().map(Excess::sentence).collect();
    OpError::invalid(
        format!("record/{}", excess[0].field),
        format!("the record contradicts itself: {}", sentences.join("; ")),
    )
    .hint("lower the terms to what the provenance allows; exporters refuse a record like this")
}

/// The refusal for a record that grants more than the parts and application records allow. `excess` is not empty.
fn refusal(character: &CharId, excess: &[Excess]) -> OpError {
    let sentences: Vec<String> = excess.iter().map(Excess::sentence).collect();
    let message = format!("the record grants more than the parts of `{character}` allow: {}", sentences.join("; "));
    let hint = if excess.iter().all(|e| e.waivable) {
        "store the record licence.derive returns, or lower these fields. To grant more, record each part's source \
         (licence.set_source) with the terms it was really licensed under, or, when every part and application record \
         is the project's own, mark each part with `owned.asserted_by` in its source row (an application record is \
         owned when its item's rights say so); the message names what limits the record"
    } else {
        "private test material can never be exported for any purpose but private: set `provenance.private_test` to \
         true and store the record licence.derive returns"
    };
    OpError::invalid(format!("record/{}", excess[0].field), message).hint(hint)
}

impl LicenceSet {
    /// The record to store, or the refusal. `derived` is what the parts and application records allow.
    fn checked(&self, derived: &DerivedLicence) -> Result<(LicenceRecord, Vec<Excess>), OpError> {
        check_form(&self.record)?;
        let contradictions = against_itself(&self.record);
        if !contradictions.is_empty() {
            return Err(contradiction(&contradictions));
        }
        let excess = against_parts(&self.record, derived);
        let mut stored = self.record.clone();
        stored.owned = None;
        if excess.is_empty() {
            return Ok((stored, excess));
        }
        // `project_owned` is never true for private test material, and that flag is the one excess nothing waives.
        if derived.project_owned && excess.iter().all(|e| e.waivable) {
            stored.owned = Some(OwnedBasis { asserted_by: derived.owners.clone() });
            return Ok((stored, excess));
        }
        Err(refusal(&self.character, &excess))
    }
}

impl Op for LicenceSet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "licence.set",
            summary: "Store a character's licence record. Refused if it grants more than licence.derive says the \
                      parts and application records allow, unless every part and application record is the project's own.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let derived = load(ctx, &self.character)?;
        let (stored, waived) = self.checked(&derived)?;
        let path = record_path(&self.character);
        let after = store::node_of(&stored, "the record")?;
        let before = ctx.doc.get(&path).cloned();
        let changed = before.as_ref() != Some(&after);
        let edit = if changed { Patch::new().with(path.clone(), before, Some(after)) } else { Patch::new() };
        let summary = json!({
            "character": self.character,
            "path": path.to_string(),
            "changed": changed,
            "record": stored,
            "granted_beyond_the_parts": waived.iter().map(|e| e.field.as_str()).collect::<Vec<_>>(),
            "derived": DerivedSummary::from(&derived),
        });
        Ok(Plan { output: Some(summary), ..Plan::edit(edit) })
    }
}

register_op!(LicenceSet);
