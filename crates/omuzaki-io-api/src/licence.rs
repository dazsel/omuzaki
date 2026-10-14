//! What an importer found about a file's licence, and the rights note that follows from it.
//!
//! Rights reporting is a legal surface, and "the file has no licence information" is a claim: made about a file that
//! does carry licence terms (say, a VRM read by the plain glTF importer, which drops the VRM extension) it points the
//! wrong way, away from a non-commercial licence. So the importer states what it found as a typed
//! [`LicenceEvidence`], required by [`ImportOutcome::new`](crate::ImportOutcome::new), and
//! [`run_import`](crate::run_import) derives the note from it. A string rule id would be easy to drop silently; this
//! way the compiler makes every importer decide.

use crate::{Finding, VrmMeta};

/// What an importer found about the licence of the file it read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LicenceEvidence {
    /// The importer read the file's licence data into `scene.meta` (and `scene.provenance` where it could derive one).
    /// [`run_import`](crate::run_import) then describes what those terms allow.
    Imported,
    /// The file has licence data this importer did not read, so `scene.meta` says nothing about it. The report warns
    /// with `rights.license_not_imported` instead of claiming the file has no licence.
    PresentNotImported {
        /// The licence data as the file spells it, for the warning (values cut short and free of control characters,
        /// because the text is from an untrusted file and may be printed to a terminal).
        summary: String,
        /// The format id whose importer would keep the data (`vrm0`, `vrm1`), for the hint.
        hint_format: &'static str,
    },
    /// The file holds no licence data at all. The report says `rights.no_license_info`.
    Absent,
}

impl LicenceEvidence {
    /// The rights notes for an import with this evidence. `meta` is the scene's, described only when it was imported.
    pub(crate) fn findings(&self, meta: &VrmMeta) -> Vec<Finding> {
        match self {
            Self::Imported => meta.import_findings(),
            Self::PresentNotImported { summary, hint_format } => vec![
                Finding::warning(
                    "rights.license_not_imported",
                    format!("licence data present but not imported: {summary}"),
                )
                .hint(format!("import as {hint_format} to keep it")),
            ],
            Self::Absent => vec![no_licence_finding()],
        }
    }
}

/// The note for a file that says nothing about its licence.
pub(crate) fn no_licence_finding() -> Finding {
    let msg = "the file carries no licence information, so its rights are recorded as unknown";
    Finding::info("rights.no_license_info", msg).hint("read where you got it, then set the provenance record by hand")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Severity;

    #[test]
    fn only_absent_evidence_says_there_is_no_licence() {
        let meta = VrmMeta::new("n", vec!["a".into()], "https://vrm.dev/licenses/1.0/");
        let absent = LicenceEvidence::Absent.findings(&meta);
        assert_eq!(absent.iter().map(|f| f.rule.as_str()).collect::<Vec<_>>(), ["rights.no_license_info"]);
        let present =
            LicenceEvidence::PresentNotImported { summary: "licenseName=CC_BY_NC".into(), hint_format: "vrm0" }
                .findings(&VrmMeta::default());
        assert_eq!(present.len(), 1, "{present:?}");
        let f = &present[0];
        assert_eq!((f.rule.as_str(), f.severity), ("rights.license_not_imported", Severity::Warning));
        assert!(
            f.message.contains("licence data present but not imported") && f.message.contains("licenseName=CC_BY_NC")
        );
        assert_eq!(f.hint.as_deref(), Some("import as vrm0 to keep it"));
        // Imported evidence describes the meta; a blank licence URL is still "no licence information".
        assert!(LicenceEvidence::Imported.findings(&meta).iter().all(|f| f.rule != "rights.no_license_info"));
        assert!(
            LicenceEvidence::Imported.findings(&VrmMeta::default()).iter().any(|f| f.rule == "rights.no_license_info")
        );
    }
}
