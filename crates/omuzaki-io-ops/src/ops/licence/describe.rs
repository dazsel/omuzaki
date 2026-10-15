//! `licence.describe`: what a character's licence record says and what is still missing for an export purpose.
//!
//! The answer is a [`Report`] in the vocabulary of `io.check_export`, with a rule id, a location in the document and a
//! fix hint on every finding, so a person or an agent can fix a record the way they fix an export. The errors are what
//! blocks the purpose; `ok` is true when there are none. This op reads the record and the parts' sources only. It
//! builds no scene, so it does not repeat what only a scene can show (that is `io.check_export`).
//!
//! What it checks, by purpose:
//! - **Every purpose:** a record exists and reads; it does not contradict itself; private test material is flagged.
//! - **Any purpose but `private`** (today `publish`): the licence gate passes, a part that is private test material
//!   fails it even if the record forgot the flag, every part has a recorded source, and the record grants no more than
//!   the parts and application records allow. For `private` these are warnings, except the private-test flag, which is an error in both.
//! - **A VRM file** also needs authors and the licence URL; those are warnings here (`ok_for_vrm` counts them), because
//!   a plain glTF does not.

use omuzaki_char_api::CharId;
use omuzaki_io_api::{Finding, Purpose, Report, Severity, VRM_LICENSE_URLS};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_store_api::licence_gate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::derive::{DerivedLicence, DerivedSummary, load};
use super::excess::against_itself;
use super::findings::{against_stored_record, gate_hint, gate_rule, leaves_the_machine};
use crate::gather::avatar::{LicenceRecord, record_path};
use crate::ops::check_export::read_record;

/// Says what a character's licence record holds and what is missing for an export purpose, with a fix hint for each.
/// Writes nothing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicenceDescribe {
    /// The character to describe.
    pub character: CharId,
    /// What the export is for. `private` (the default) is a file for your own machine; `publish` is anything that
    /// leaves it, and the licence gate must pass.
    #[serde(default)]
    pub purpose: Purpose,
}

/// What `licence.describe` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LicenceDescription {
    /// The character.
    pub character: String,
    /// The purpose it was described for.
    pub purpose: Purpose,
    /// Where the record lives in the document.
    pub path: String,
    /// The record as stored, if the project holds one that reads.
    pub record: Option<LicenceRecord>,
    /// True when nothing blocks an export for this purpose: no finding is an error.
    pub ok: bool,
    /// True when `ok` and a VRM file would not be missing its authors or licence URL either.
    pub ok_for_vrm: bool,
    /// What the parts and application records allow (`licence.derive` has the record itself).
    pub derived: DerivedSummary,
    /// The findings, errors first.
    pub report: Report,
}

impl LicenceDescribe {
    /// The findings about the character's parts, which hold whether or not there is a record: a part without a source
    /// blocks any purpose that leaves the machine.
    fn parts_findings(&self, derived: &DerivedLicence, report: &mut Report) {
        for finding in &derived.report.findings {
            let mut finding = finding.clone();
            if leaves_the_machine(self.purpose)
                && matches!(finding.rule.as_str(), "licence.part_source_missing" | "licence.no_parts")
            {
                finding.severity = Severity::Error;
            }
            report.push(finding);
        }
    }

    /// The findings about a record that reads.
    fn record_findings(&self, record: &LicenceRecord, report: &mut Report) {
        let base = record_path(&self.character).to_string();
        if leaves_the_machine(self.purpose)
            && let Err(error) = licence_gate(&record.provenance, false)
        {
            let finding = Finding::error(gate_rule(&error), error.to_string());
            report.push(finding.at(format!("{base}/provenance")).hint(gate_hint(&error)));
        }
        for excess in against_itself(record) {
            let finding = Finding::error("rights.terms_exceed_provenance", excess.sentence());
            report.push(
                finding.at(format!("{base}/{}", excess.field)).hint("lower the terms to what the provenance allows"),
            );
        }
        if record.meta.authors.iter().all(|a| a.trim().is_empty())
            && record.provenance.author.as_deref().is_none_or(|a| a.trim().is_empty())
        {
            let msg = "a VRM file needs at least one author, and the record names none";
            let hint = "set provenance.author or the meta authors; `licence.derive` unions the parts' authors";
            report
                .push(Finding::warning("vrm.meta_authors_missing", msg).at(format!("{base}/meta/authors")).hint(hint));
        }
        if record.meta.license_url.trim().is_empty() {
            let msg = "a VRM 1.0 file needs the licence URL, and the record has none";
            let hint = format!("use {}", VRM_LICENSE_URLS[0]);
            report.push(
                Finding::warning("vrm.meta_license_url_missing", msg).at(format!("{base}/meta/licenseUrl")).hint(hint),
            );
        }
        if record.provenance.consent.is_none() {
            let msg = "if a real person's likeness or voice is involved, the record holds no consent for it";
            let hint =
                "record it in provenance.consent; the gate asks for it once an export is told a person is involved";
            report.push(
                Finding::info("rights.consent_not_checked", msg).at(format!("{base}/provenance/consent")).hint(hint),
            );
        }
    }
}

impl Op for LicenceDescribe {
    fn meta() -> OpMeta {
        OpMeta {
            name: "licence.describe",
            summary: "Say what a character's licence record holds and what is still missing for an export purpose \
                      (private, publish), with a fix hint for each. Writes nothing.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let derived = load(ctx, &self.character)?;
        let base = record_path(&self.character).to_string();
        let mut report = Report::new();
        let record = match read_record(ctx, &self.character) {
            Ok(Some(record)) => Some(record),
            Ok(None) => {
                let hint = "run licence.derive and store its record with licence.set; rights are never guessed from \
                            the parts";
                report.push(
                    Finding::error(
                        "rights.provenance_missing",
                        "the project holds no licence record for this character",
                    )
                    .at(base.clone())
                    .hint(hint),
                );
                None
            }
            Err(error) => {
                let hint = "licence.set replaces the record: store the one licence.derive returns";
                report.push(Finding::error("licence.record_invalid", error.message).at(base.clone()).hint(hint));
                None
            }
        };
        self.parts_findings(&derived, &mut report);
        report.findings.extend(against_stored_record(&self.character, self.purpose, record.as_ref(), &derived));
        if let Some(record) = &record {
            self.record_findings(record, &mut report);
        }
        // A stable sort: what blocks the purpose comes before the notes.
        report.findings.sort_by_key(|f| std::cmp::Reverse(f.severity));
        let ok = !report.has_errors();
        let description = LicenceDescription {
            character: self.character.to_string(),
            purpose: self.purpose,
            path: base,
            ok,
            ok_for_vrm: ok && !report.findings.iter().any(|f| f.rule.starts_with("vrm.")),
            derived: DerivedSummary::from(&derived),
            record,
            report,
        };
        let output = serde_json::to_value(description)
            .map_err(|e| OpError::internal(format!("serialising the description failed: {e}")))?;
        Ok(Plan::answer(output))
    }
}

register_op!(LicenceDescribe);
