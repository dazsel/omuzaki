//! The findings that measure a stored licence record against the parts as they are now, in the vocabulary of
//! `io.check_export` (a rule id, a location in the document and a fix hint on each).
//!
//! The record is a snapshot: `licence.set` refuses a record that grants more than the parts and application records allow, but a part can change
//! afterwards (a library item with tighter terms swapped in, a part with a non-commercial source switched on, a new part
//! with no source), and the stored record then grants more than the parts do. `licence.describe` and `io.check_export`
//! (and so `io.export`) both report that through this module, so a stale record reads the same in each and the two can
//! never disagree about it.
//!
//! The same module raises what the library's application records say (`characters/<id>/applied/<app>`): an applied item
//! that is not cleared for commercial use blocks `publish` (`rights.item_not_cleared`). It does not stop a file for the
//! user's own machine.

use omuzaki_char_api::CharId;
use omuzaki_io_api::{Finding, Purpose};
use omuzaki_store_api::{Commercial, GateError};

use super::derive::{ApplicationRow, DerivedLicence};
use super::excess::{Excess, against_parts};
use super::source::sources_path;
use crate::gather::avatar::{LicenceRecord, record_path};

/// The rule id of the licence gate's refusal, as `preflight` spells it.
pub(crate) fn gate_rule(error: &GateError) -> &'static str {
    match error {
        GateError::NonCommercial { .. } => "rights.gate_non_commercial",
        GateError::Unknown { .. } => "rights.gate_unknown",
        GateError::PrivateTest => "rights.gate_private_test",
        GateError::NoConsent => "rights.gate_no_consent",
    }
}

/// How to fix what the licence gate refused.
pub(crate) fn gate_hint(error: &GateError) -> &'static str {
    match error {
        GateError::NonCommercial { .. } => {
            "replace the non-commercial parts, or correct the record if it is wrong; for your own machine use purpose \
             \"private\""
        }
        GateError::Unknown { .. } => {
            "record where each part comes from with licence.set_source, and mark it commercial \"ok\" only once someone has \
             read the licence; then store licence.derive's record with licence.set"
        }
        GateError::PrivateTest => "private test material never leaves this machine: use purpose \"private\"",
        GateError::NoConsent => "record the consent in provenance.consent",
    }
}

/// Whether the file is for anything but the user's own machine. These are the purposes the licence gate applies to.
pub(crate) const fn leaves_the_machine(purpose: Purpose) -> bool {
    !matches!(purpose, Purpose::Private)
}

/// Everything the parts say about the stored `record` for an export for `purpose`, errors and notes alike:
/// - **private test material** in a part fails every purpose but `private` (`rights.gate_private_test`, located at the
///   part's row), with or without a record and even if the record forgot the flag. A record that carries the flag gets
///   the gate's own finding from `preflight`, so it is not raised twice;
/// - **a record that grants more than the parts and application records allow** (`licence.exceeds_parts`), one finding per field with the part
///   that limits it, located at that field of the record. Nothing is said without a record: its absence is
///   `rights.provenance_missing`, which `preflight` reports.
pub(crate) fn against_stored_record(
    id: &CharId,
    purpose: Purpose,
    record: Option<&LicenceRecord>,
    derived: &DerivedLicence,
) -> Vec<Finding> {
    let mut found = Vec::new();
    let record_flags_private = record.is_some_and(|r| r.provenance.private_test);
    found.extend(private_parts_finding(id, purpose, record_flags_private, derived));
    found.extend(item_findings(purpose, derived));
    if let Some(record) = record {
        let base = record_path(id).to_string();
        found.extend(
            against_parts(record, derived)
                .iter()
                .map(|excess| exceeds_parts_finding(excess, derived, record, &base, purpose)),
        );
    }
    found
}

/// What the application records say about a file that leaves the machine; nothing for one that stays on it.
/// - **`rights.item_not_cleared`** (an error), located at the application record: an applied item whose rights are
///   `commercial: no` or `unknown`, or private test material.
fn item_findings(purpose: Purpose, derived: &DerivedLicence) -> Vec<Finding> {
    if !leaves_the_machine(purpose) {
        return Vec::new();
    }
    derived.applications.iter().filter_map(item_not_cleared).collect()
}

/// The error for an applied item that is not cleared for commercial use, or `None` for one that is.
fn item_not_cleared(app: &ApplicationRow) -> Option<Finding> {
    let why = if app.private_test {
        "is private test material, which never leaves this machine"
    } else {
        match app.commercial {
            Commercial::Ok => return None,
            Commercial::No => "is not licensed for commercial use",
            Commercial::Unknown => "has no recorded commercial status",
        }
    };
    let message = format!("the applied item `{}@{}` (licence {}) {why}", app.item, app.version, app.licence);
    let hint = "apply a cleared item in the same slot, or remove what the item created; for your own machine use purpose \"private\"";
    Some(Finding::error("rights.item_not_cleared", message).at(app.path.clone()).hint(hint))
}

/// The gate's refusal for a character with a private test part, raised from the parts. `None` for a file on the user's
/// own machine, when no part is private test material, or when the record already carries the flag.
fn private_parts_finding(
    id: &CharId,
    purpose: Purpose,
    record_flags_private: bool,
    derived: &DerivedLicence,
) -> Option<Finding> {
    let private_parts: Vec<&str> = derived.parts.iter().filter(|r| r.private_test).map(|r| r.part.as_str()).collect();
    let first = private_parts.first()?;
    if !leaves_the_machine(purpose) || record_flags_private {
        return None;
    }
    let named: Vec<String> = private_parts.iter().map(|p| format!("`{p}`")).collect();
    Some(
        Finding::error(
            gate_rule(&GateError::PrivateTest),
            format!("{} ({})", GateError::PrivateTest, named.join(", ")),
        )
        .at(sources_path(id).join(*first).to_string())
        .hint(gate_hint(&GateError::PrivateTest)),
    )
}

/// One field of a record that grants more than the parts and application records allow.
///
/// - **An error** (`licence.exceeds_parts`) for a file that leaves the machine, and for the private-test flag whatever
///   the purpose: that is never waived.
/// - **A warning** for a file that stays on the user's machine.
/// - **A note** (`licence.exceeds_parts_owned`) when every part and application record is the project's own and the record says so; a warning if
///   it does not yet say so. The project-owned exception waives every field but the private-test flag.
pub(crate) fn exceeds_parts_finding(
    excess: &Excess,
    derived: &DerivedLicence,
    record: &LicenceRecord,
    base: &str,
    purpose: Purpose,
) -> Finding {
    let at = format!("{base}/{}", excess.field);
    if derived.project_owned && excess.waivable {
        let basis = match &record.owned {
            Some(owned) => format!("on the word of {}", owned.asserted_by.join(", ")),
            None => format!(
                "the project owns every part (asserted by {}), but the record does not say so",
                derived.owners.join(", ")
            ),
        };
        let hint = "store the record again with licence.set to write who asserted it";
        let message = format!("{}: {basis}", excess.sentence());
        let finding = if record.owned.is_some() {
            Finding::info("licence.exceeds_parts_owned", message)
        } else {
            Finding::warning("licence.exceeds_parts_owned", message)
        };
        return finding.at(at).hint(hint);
    }
    let blocking = leaves_the_machine(purpose) || !excess.waivable;
    let message = format!("the record grants more than its parts allow: {}", excess.sentence());
    let hint = "a part may have changed since the record was set: run licence.derive and licence.set to store what the \
                parts allow, or record the parts' real terms with licence.set_source";
    let finding = if blocking {
        Finding::error("licence.exceeds_parts", message)
    } else {
        Finding::warning("licence.exceeds_parts", message)
    };
    finding.at(at).hint(hint)
}
