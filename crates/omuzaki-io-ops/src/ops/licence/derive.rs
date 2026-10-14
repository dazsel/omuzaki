//! `licence.derive`: the licence record a character's parts allow, as a query.
//!
//! The record is the **join of the parts' source rows** (`source.rs`), and it never guesses:
//! - every permission is the **strictest** of the parts: the commercial status (`no`, then `unknown`, then `ok`), the
//!   private-test flag (any part makes the character private test material) and each of the nine flags of the file's
//!   terms (`terms::AXES`);
//! - the licence is every distinct licence joined with SPDX `AND` (all of them apply), and the authors and attributions
//!   are the **union** of the parts', in part-id order;
//! - a part with **no source row counts as the most restrictive part possible**: commercial `unknown`, the default
//!   terms, licence `LicenseRef-Unknown`. A character with no parts joins to the same floor, so nothing is ever
//!   derived from nothing. `complete` says whether every part had a row;
//! - the **name** is the character's display name and the **licence URL** is the VRM Public License 1.0 page, the
//!   document that defines the flags, which a VRM 1.0 file must name. Neither widens a right.
//!
//! Every part is joined, enabled or not: a variant can switch any part on, and one record serves every variant.

use omuzaki_char_api::{CharId, KindProvenance, PartId, find_kind, paths};
use omuzaki_io_api::{CreditNotation, Finding, Report, UseTerms, VRM_LICENSE_URLS, VrmMeta};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::{Commercial, Consent, Provenance, Source};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::source::{PartSource, source_path, sources_path};
use super::terms::{self, commercial_rank};
use crate::gather::avatar::LicenceRecord;

/// The licence of a part made by one of the project's own generators. Terms stay the most restrictive: a kind says whose
/// rights they are, not what is granted.
const PROJECT_LICENCE: &str = "LicenseRef-Omuzaki-User";

/// The licence of a part nobody has recorded a source for.
const UNKNOWN_LICENCE: &str = "LicenseRef-Unknown";

/// Works out the licence record a character's parts allow: the strictest join of their recorded sources. Writes
/// nothing; `licence.set` stores a record.
///
/// Parts with no recorded source count as the most restrictive part possible, so the answer only widens as sources are
/// recorded. `parts` shows each part's contribution and `report` lists the parts that have no source.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicenceDerive {
    /// The character to derive the record of.
    pub character: CharId,
}

/// One part's contribution to the join: what its source row says, or the most restrictive values when it has none.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[allow(clippy::struct_excessive_bools)] // the row is a flat report of independent facts about one part
pub struct PartRow {
    /// The part's id.
    pub part: String,
    /// Its kind (`hair`, `mock-garment`).
    pub kind: String,
    /// Whether it is enabled. Disabled parts are joined too: a variant can enable them.
    pub enabled: bool,
    /// False when the part has no source row, so every value below is the most restrictive possible.
    pub recorded: bool,
    /// True when the values come from the part's kind (a project generator) because the part has no row.
    pub kind_owned: bool,
    /// Where the part came from (`None` without a row or a project kind).
    pub source: Option<Source>,
    /// Its SPDX licence (`LicenseRef-Unknown` without a row).
    pub licence: String,
    /// Its commercial status (`unknown` without a row).
    pub commercial: Commercial,
    /// Whether it is private test material.
    pub private_test: bool,
    /// Who made it, if recorded.
    pub author: Option<String>,
    /// The credit that must travel with it, if any.
    pub attribution: Option<String>,
    /// Who marked it as the project's own, if anyone did.
    pub owned_by: Option<String>,
    /// The terms it grants: its row's, or the defaults. Credit is always required when it has an attribution, and
    /// private test material grants nothing.
    pub terms: UseTerms,
}

/// What `licence.derive` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DerivedLicence {
    /// The character.
    pub character: String,
    /// True when every part has a source row. When false, `record` is still the strictest join, with each part that
    /// has no row counted as the most restrictive possible.
    pub complete: bool,
    /// The record the parts allow. Storing it with `licence.set` never needs the project-owned basis.
    pub record: LicenceRecord,
    /// Each part's contribution, in part-id order.
    pub parts: Vec<PartRow>,
    /// True when any part is private test material: the character can never be exported for any purpose but private.
    pub private_test: bool,
    /// True when every part is marked as the project's own (and none is private test material): a record may then grant
    /// more than `record` does, on the word of `owners`.
    pub project_owned: bool,
    /// Who marked the parts as the project's own (sorted, without repeats).
    pub owners: Vec<String>,
    /// The credit lines the parts require, in part-id order. A record must keep every one.
    pub attributions: Vec<String>,
    /// Parts without a source row, a character without parts, and consents the record cannot hold together.
    pub report: Report,
}

/// The facts about a derived record that decide what may be done with a character, without the record itself.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DerivedSummary {
    /// Whether every part has a source row.
    pub complete: bool,
    /// Whether any part is private test material.
    pub private_test: bool,
    /// Whether every part is the project's own.
    pub project_owned: bool,
    /// Who said so.
    pub owners: Vec<String>,
    /// The parts without a source row.
    pub unresolved: Vec<String>,
}

impl From<&DerivedLicence> for DerivedSummary {
    fn from(derived: &DerivedLicence) -> Self {
        Self {
            complete: derived.complete,
            private_test: derived.private_test,
            project_owned: derived.project_owned,
            owners: derived.owners.clone(),
            unresolved: derived.parts.iter().filter(|p| !p.recorded).map(|p| p.part.clone()).collect(),
        }
    }
}

/// A part as the document holds it, before the join.
#[derive(Clone, Debug)]
pub(crate) struct PartInput {
    pub id: PartId,
    pub kind: String,
    pub enabled: bool,
    pub source: Option<PartSource>,
    /// The part's kind is a registered generator of this project (`KindProvenance::ProjectOwned`).
    pub kind_owned: bool,
}

/// Who `owned_by` names for a part whose rights come from its kind: the project's own generators.
pub const KIND_OWNER: &str = "the project (built-in part kind)";

/// How strictly a source binds the file's own terms: `preflight` applies the creator's terms to `Imported` material.
pub(crate) fn source_rank(source: &Source) -> u8 {
    match source {
        Source::Authored => 0,
        Source::Library { .. } => 1,
        Source::Generated { .. } => 2,
        Source::Imported { .. } => 3,
    }
}

impl PartRow {
    fn of(input: &PartInput) -> Self {
        let (part, kind, enabled) = (input.id.to_string(), input.kind.clone(), input.enabled);
        // A row about this part wins over what its kind says; the kind only speaks when there is no row.
        let Some(source) = &input.source else {
            let owned = input.kind_owned;
            return Self {
                part,
                kind,
                enabled,
                recorded: owned,
                kind_owned: owned,
                source: owned.then_some(Source::Authored),
                licence: if owned { PROJECT_LICENCE } else { UNKNOWN_LICENCE }.to_owned(),
                commercial: if owned { Commercial::Ok } else { Commercial::Unknown },
                private_test: false,
                author: None,
                attribution: None,
                owned_by: owned.then(|| KIND_OWNER.to_owned()),
                terms: UseTerms::default(),
            };
        };
        let provenance = &source.provenance;
        let attribution = non_blank(provenance.attribution.as_deref());
        let mut terms = source.terms.clone().unwrap_or_default();
        if provenance.private_test {
            terms = UseTerms::default();
        } else if attribution.is_some() {
            terms.credit_notation = CreditNotation::Required;
        }
        Self {
            part,
            kind,
            enabled,
            recorded: true,
            kind_owned: false,
            source: Some(provenance.source.clone()),
            licence: provenance.licence.trim().to_owned(),
            commercial: provenance.commercial,
            private_test: provenance.private_test,
            author: non_blank(provenance.author.as_deref()),
            attribution,
            owned_by: source.owned.as_ref().map(|o| o.asserted_by.trim().to_owned()),
            terms,
        }
    }
}

fn non_blank(text: Option<&str>) -> Option<String> {
    text.map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned)
}

/// Each value once, in the order first met.
fn unique(values: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for value in values {
        if !seen.contains(&value) {
            seen.push(value);
        }
    }
    seen
}

/// Every distinct licence joined with SPDX `AND`, which says that all of them apply. An expression of its own
/// (`MIT OR Apache-2.0`) is parenthesised, because `AND` binds tighter than `OR`.
fn join_licences(rows: &[PartRow]) -> String {
    let mut licences: Vec<&str> = rows.iter().map(|r| r.licence.as_str()).collect();
    licences.sort_unstable();
    licences.dedup();
    match licences.as_slice() {
        [] => UNKNOWN_LICENCE.to_owned(),
        [one] => (*one).to_owned(),
        many => many
            .iter()
            .map(|l| if l.contains(char::is_whitespace) { format!("({l})") } else { (*l).to_owned() })
            .collect::<Vec<_>>()
            .join(" AND "),
    }
}

/// The consent a record can hold: the one every part that has a consent agrees on. Different consents cannot be one.
fn joint_consent(inputs: &[PartInput], report: &mut Report) -> Option<Consent> {
    let mut consents: Vec<(&PartId, &Consent)> = Vec::new();
    for input in inputs {
        if let Some(consent) = input.source.as_ref().and_then(|s| s.provenance.consent.as_ref()) {
            consents.push((&input.id, consent));
        }
    }
    let first = consents.first()?.1;
    if consents.iter().all(|(_, c)| *c == first) {
        return Some(first.clone());
    }
    let named: Vec<String> = consents.iter().map(|(part, c)| format!("`{part}`: {} ({})", c.person, c.scope)).collect();
    report.push(
        Finding::warning(
            "licence.consent_ambiguous",
            format!("the parts record different consents ({}), and a record holds one", named.join("; ")),
        )
        .hint("record the consent that covers the whole character in the record's provenance.consent"),
    );
    None
}

/// The join of `inputs` for the character `id` called `name`. Pure: it reads nothing, so every rule is testable alone.
pub(crate) fn join(id: &CharId, name: &str, inputs: &[PartInput]) -> DerivedLicence {
    let rows: Vec<PartRow> = inputs.iter().map(PartRow::of).collect();
    let mut report = Report::new();
    if rows.is_empty() {
        report.push(
            Finding::warning(
                "licence.no_parts",
                "the character has no parts, so there is nothing to derive a record from",
            )
            .at(paths::character(id).join("parts").to_string())
            .hint("add parts first; until then the record is the most restrictive one"),
        );
    }
    for row in rows.iter().filter(|r| !r.recorded) {
        let path = PartId::new(&row.part).map_or_else(|_| sources_path(id), |p| source_path(id, &p));
        let message = format!(
            "part `{}` (kind `{}`) has no recorded source, so it counts as the most restrictive part possible",
            row.part, row.kind
        );
        let hint = format!(
            "record where it came from at {path}: schema 1, a provenance table and, if its licence grants more than \
             the defaults, a terms table; licence.set_source records it by hand, and the ops that bring a part in write it"
        );
        report.push(Finding::warning("licence.part_source_missing", message).at(path.to_string()).hint(hint));
    }

    let private_test = rows.iter().any(|r| r.private_test);
    let commercial =
        rows.iter().map(|r| r.commercial).min_by_key(|c| commercial_rank(*c)).unwrap_or(Commercial::Unknown);
    let mut joined_terms = terms::join(rows.iter().map(|r| &r.terms));
    // The exporters' rule (`rights.terms_exceed_provenance`): a status that is not `ok`, and private test material, carry
    // only the most restrictive value of every flag. The join of parts that are individually fine can still land here,
    // for instance when one of them is unknown.
    if commercial != Commercial::Ok || private_test {
        joined_terms = UseTerms::default();
    }
    let authors = unique(rows.iter().filter_map(|r| r.author.clone()));
    let attributions = unique(rows.iter().filter_map(|r| r.attribution.clone()));
    let owners = {
        let mut owners = unique(rows.iter().filter_map(|r| r.owned_by.clone()));
        owners.sort_unstable();
        owners
    };
    let provenance = Provenance {
        source: rows
            .iter()
            .filter_map(|r| r.source.as_ref())
            .fold(None::<&Source>, |best, next| match best {
                Some(b) if source_rank(b) >= source_rank(next) => Some(b),
                _ => Some(next),
            })
            .cloned()
            .unwrap_or(Source::Authored),
        author: (!authors.is_empty()).then(|| authors.join(", ")),
        licence: join_licences(&rows),
        attribution: (!attributions.is_empty()).then(|| attributions.join("; ")),
        commercial,
        consent: joint_consent(inputs, &mut report),
        private_test,
    };
    let mut meta = VrmMeta::new(name, authors, VRM_LICENSE_URLS[0]);
    meta.terms = joined_terms;
    DerivedLicence {
        character: id.to_string(),
        complete: !rows.is_empty() && rows.iter().all(|r| r.recorded),
        record: LicenceRecord::new(provenance, meta),
        private_test,
        project_owned: !rows.is_empty() && !private_test && rows.iter().all(|r| r.owned_by.is_some()),
        owners,
        attributions,
        parts: rows,
        report,
    }
}

/// The error for a character the document does not hold.
pub(crate) fn no_such_character(id: &CharId) -> OpError {
    let mut error = OpError::not_found(format!("there is no character `{id}`"))
        .hint("check the id; characters live under characters/<id>");
    error.field = Some("character".to_owned());
    error
}

/// Reads the character's parts and their source rows from the document and joins them.
///
/// # Errors
/// `not_found` for a character the document lacks; `invalid` (naming `parts/<part>` or `sources/<part>`) for a part id
/// that is not a slug or a source row that does not read.
pub(crate) fn load(ctx: &OpCtx<'_>, id: &CharId) -> Result<DerivedLicence, OpError> {
    let node = ctx.doc.get(&paths::character(id)).ok_or_else(|| no_such_character(id))?;
    let name = node.get("name").and_then(Value::as_str).unwrap_or_default();
    let mut table: Vec<(&String, &Value)> = match node.get("parts") {
        Some(Value::Object(parts)) => parts.iter().collect(),
        _ => Vec::new(),
    };
    table.sort_unstable_by_key(|(key, _)| key.as_str());
    let mut inputs = Vec::with_capacity(table.len());
    for (key, part) in table {
        let part_id = PartId::new(key.as_str()).map_err(|e| {
            OpError::invalid(format!("parts/{key}"), format!("`{key}` is not a part id: {e}"))
                .hint("part ids are lowercase letters, digits, `-` and `_`; rename the part")
        })?;
        let path = source_path(id, &part_id);
        let source = ctx.doc.get(&path).map(PartSource::from_node).transpose().map_err(|e| {
            OpError::invalid(format!("sources/{part_id}"), format!("the source of part `{part_id}` at {path}: {e}"))
                .hint("a source row is `schema = 1`, a `provenance` table and, optionally, `terms` and `owned`")
        })?;
        let kind = part.get("kind").and_then(Value::as_str).unwrap_or_default();
        inputs.push(PartInput {
            kind_owned: find_kind(kind).is_some_and(|k| (k.provenance)() == KindProvenance::ProjectOwned),
            kind: part.get("kind").and_then(Value::as_str).unwrap_or_default().to_owned(),
            enabled: part.get("enabled").and_then(Value::as_bool).unwrap_or(true),
            id: part_id,
            source,
        });
    }
    let mut derived = join(id, name, &inputs);
    note_orphans(ctx, id, &inputs, &mut derived.report);
    Ok(derived)
}

/// Source rows of parts the character no longer has: harmless, and worth a line. `part.remove` takes a part's row with it,
/// so these come from a project edited by hand.
fn note_orphans(ctx: &OpCtx<'_>, id: &CharId, inputs: &[PartInput], report: &mut Report) {
    let Some(Value::Object(rows)) = ctx.doc.get(&sources_path(id)) else { return };
    let mut orphans: Vec<&str> =
        rows.keys().map(String::as_str).filter(|key| !inputs.iter().any(|p| p.id.as_str() == *key)).collect();
    orphans.sort_unstable();
    if !orphans.is_empty() {
        let named: Vec<String> = orphans.iter().map(|o| format!("`{o}`")).collect();
        report.push(
            Finding::info(
                "licence.source_orphaned",
                format!("sources for parts the character no longer has are ignored: {}", named.join(", ")),
            )
            .at(sources_path(id).to_string())
            .hint("remove them when you are sure the parts are gone for good"),
        );
    }
}

impl Op for LicenceDerive {
    fn meta() -> OpMeta {
        OpMeta {
            name: "licence.derive",
            summary: "Work out the licence record a character's parts allow: the strictest join of their recorded \
                      sources. Writes nothing; licence.set stores a record.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let derived = load(ctx, &self.character)?;
        let output = serde_json::to_value(derived)
            .map_err(|e| OpError::internal(format!("serialising the derived record failed: {e}")))?;
        Ok(Plan::answer(output))
    }
}

register_op!(LicenceDerive);

#[cfg(test)]
mod tests {
    use omuzaki_io_api::UseTerms;

    use super::*;
    use crate::ops::licence::source::Ownership;

    fn input(part: &str, licence: &str, commercial: Commercial, author: Option<&str>) -> PartInput {
        let provenance = Provenance {
            source: Source::Authored,
            author: author.map(str::to_owned),
            licence: licence.to_owned(),
            attribution: None,
            commercial,
            consent: None,
            private_test: false,
        };
        PartInput {
            id: PartId::new(part).unwrap(),
            kind: "mock".to_owned(),
            enabled: true,
            source: Some(PartSource::new(provenance)),
            kind_owned: false,
        }
    }

    fn joined(inputs: &[PartInput]) -> DerivedLicence {
        join(&CharId::new("mira").unwrap(), "Mira", inputs)
    }

    #[test]
    fn a_licence_expression_of_its_own_is_parenthesised_because_and_binds_tighter_than_or() {
        let parts = [
            input("a", "MIT OR Apache-2.0", Commercial::Ok, None),
            input("b", "CC-BY-4.0", Commercial::Ok, None),
            input("c", "CC-BY-4.0", Commercial::Ok, None),
        ];
        assert_eq!(joined(&parts).record.provenance.licence, "CC-BY-4.0 AND (MIT OR Apache-2.0)");
        assert_eq!(joined(&parts[1..]).record.provenance.licence, "CC-BY-4.0", "the same licence twice is once");
        assert_eq!(joined(&parts[..1]).record.provenance.licence, "MIT OR Apache-2.0", "one part needs no brackets");
    }

    #[test]
    fn a_known_restriction_beats_an_unknown_status_and_authors_are_a_union_in_part_order() {
        let parts = [
            input("a", "CC0-1.0", Commercial::Unknown, Some("Zed")),
            input("b", "CC0-1.0", Commercial::No, Some("Amy")),
            input("c", "CC0-1.0", Commercial::Ok, Some("Zed")),
            input("d", "CC0-1.0", Commercial::Ok, Some("  ")),
        ];
        let derived = joined(&parts);
        assert_eq!(derived.record.provenance.commercial, Commercial::No);
        assert_eq!(derived.record.provenance.author.as_deref(), Some("Zed, Amy"));
        assert_eq!(derived.record.meta.authors, ["Zed", "Amy"]);
    }

    #[test]
    fn owners_are_sorted_and_a_character_is_owned_only_if_every_part_is() {
        let mut parts = [input("a", "CC0-1.0", Commercial::Ok, None), input("b", "CC0-1.0", Commercial::Ok, None)];
        for (part, name) in parts.iter_mut().zip(["Zoe", "Abe"]) {
            part.source.as_mut().unwrap().owned = Some(Ownership { asserted_by: name.to_owned() });
        }
        let derived = joined(&parts);
        assert!(derived.project_owned);
        assert_eq!(derived.owners, ["Abe", "Zoe"]);
        parts[1].source.as_mut().unwrap().owned = None;
        assert!(!joined(&parts).project_owned);
        parts[1].source = None;
        assert!(!joined(&parts).project_owned, "a part with no source is not the project's own");
    }

    #[test]
    fn private_test_material_and_an_uncleared_status_both_grant_nothing() {
        let mut parts = [input("a", "CC0-1.0", Commercial::Ok, None), input("b", "CC0-1.0", Commercial::Ok, None)];
        for part in &mut parts {
            part.source.as_mut().unwrap().terms = Some(UseTerms { allow_redistribution: true, ..UseTerms::default() });
        }
        assert!(joined(&parts).record.meta.terms.allow_redistribution);
        parts[1].source.as_mut().unwrap().provenance.private_test = true;
        let derived = joined(&parts);
        assert!(derived.private_test && derived.record.provenance.private_test);
        assert_eq!(derived.record.meta.terms, UseTerms::default());
        parts[1].source.as_mut().unwrap().provenance.private_test = false;
        parts[1].source.as_mut().unwrap().provenance.commercial = Commercial::Unknown;
        assert_eq!(joined(&parts).record.meta.terms, UseTerms::default());
    }

    #[test]
    fn credit_is_required_whenever_a_part_asks_for_an_attribution_whatever_its_terms_say() {
        let mut part = input("a", "CC-BY-4.0", Commercial::Ok, None);
        let source = part.source.as_mut().unwrap();
        source.terms = Some(UseTerms { credit_notation: CreditNotation::Unnecessary, ..UseTerms::default() });
        assert_eq!(joined(std::slice::from_ref(&part)).record.meta.terms.credit_notation, CreditNotation::Unnecessary);
        part.source.as_mut().unwrap().provenance.attribution = Some("  By Amy  ".to_owned());
        let derived = joined(&[part]);
        assert_eq!(derived.record.meta.terms.credit_notation, CreditNotation::Required);
        assert_eq!(derived.attributions, ["By Amy"], "trimmed");
    }
}
