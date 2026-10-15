//! The rights rows for performing as an imported avatar: what the terms allow, and the credit line to copy.
//!
//! Performing on your own machine is not publishing, so none of these rows blocks anything. Each one is a note with a
//! sentence and a fix, shown at import beside the other rights notes (`rights.unrecognised_license_url`,
//! `rights.import_restricted`) so the import op, the licence ops and the go-live notice share one copy.

use super::{AvatarPermission, CommercialUsage, CreditNotation, UseTerms, VrmMeta, credit_line};
use crate::Finding;

impl UseTerms {
    /// The rights rows that need only the terms: `rights.avatar_permission`, `rights.perform_non_profit` and
    /// `rights.content_restricted`, in that order. Empty when the terms allow everything they can.
    pub fn perform_findings(&self) -> Vec<Finding> {
        perform_rows(self, None)
    }
}

impl VrmMeta {
    /// [`UseTerms::perform_findings`] plus `rights.credit_required` when the model asks for credit, in the order
    /// avatar permission, non-profit, credit, content. The credit line is the one the provenance record derives
    /// (`imported_provenance`), so a stream title and the record never disagree.
    pub fn perform_findings(&self) -> Vec<Finding> {
        let credit = (self.terms.credit_notation == CreditNotation::Required).then(|| self.credit_finding());
        perform_rows(&self.terms, credit)
    }

    /// The credit line to put in stream titles, from the same helper as the provenance attribution.
    fn credit_finding(&self) -> Finding {
        let line = credit_line(&self.name, self.copyright_information.as_deref(), &self.authors);
        Finding::info("rights.credit_required", format!("credit is required; put this line in stream titles: {line}"))
            .at("meta/creditNotation")
            .hint("copy the credit line into the stream title")
    }
}

/// The rows in their fixed order. `credit` is only given by [`VrmMeta::perform_findings`], and sits after the terms
/// rows so the output is the same on every call.
fn perform_rows(terms: &UseTerms, credit: Option<Finding>) -> Vec<Finding> {
    let mut out = Vec::new();
    if let Some(finding) = avatar_permission_row(terms) {
        out.push(finding);
    }
    if terms.commercial_usage == CommercialUsage::PersonalNonProfit {
        out.push(
            Finding::warning(
                "rights.perform_non_profit",
                "the terms allow personal, non-profit use only (commercialUsage is personalNonProfit)",
            )
            .at("meta/commercialUsage")
            .hint("do not use it on monetised streams"),
        );
    }
    out.extend(credit);
    // The content flags of `VRMC_vrm.meta` that restrict performing, named as the spec spells them, in its order.
    let flags = [
        ("allowExcessivelyViolentUsage", terms.allow_excessively_violent_usage),
        ("allowExcessivelySexualUsage", terms.allow_excessively_sexual_usage),
        ("allowPoliticalOrReligiousUsage", terms.allow_political_or_religious_usage),
        ("allowAntisocialOrHateUsage", terms.allow_antisocial_or_hate_usage),
    ];
    let restricted: Vec<&str> = flags.into_iter().filter(|(_, allowed)| !allowed).map(|(name, _)| name).collect();
    if !restricted.is_empty() {
        out.push(
            Finding::info(
                "rights.content_restricted",
                format!("the terms do not allow these kinds of content: {}", restricted.join(", ")),
            )
            .at("meta")
            .hint("do not perform content of these kinds with it"),
        );
    }
    out
}

/// The avatar-permission row. Each restricted value says its own thing, because they grant different permissions:
/// `onlyAuthor` is the author alone, `onlySeparatelyLicensedPerson` is people with their own licence. `everyone` has
/// no row.
fn avatar_permission_row(terms: &UseTerms) -> Option<Finding> {
    let (message, hint) = match terms.avatar_permission {
        AvatarPermission::Everyone => return None,
        AvatarPermission::OnlyAuthor => (
            "only the author may perform as this avatar (avatarPermission is \"onlyAuthor\"); confirm that is you",
            "confirm you are the author before performing as it",
        ),
        AvatarPermission::OnlySeparatelyLicensedPerson => (
            "only people licensed separately may perform as this avatar \
             (avatarPermission is \"onlySeparatelyLicensedPerson\"); confirm you hold a separate licence",
            "confirm you hold a separate licence from the author before performing as it",
        ),
    };
    Some(Finding::warning("rights.avatar_permission", message).at("meta/avatarPermission").hint(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Terms that grant every row's permission: everyone may perform, corporate use, no credit, every content flag true.
    fn open() -> UseTerms {
        UseTerms {
            avatar_permission: AvatarPermission::Everyone,
            allow_excessively_violent_usage: true,
            allow_excessively_sexual_usage: true,
            commercial_usage: CommercialUsage::Corporation,
            allow_political_or_religious_usage: true,
            allow_antisocial_or_hate_usage: true,
            credit_notation: CreditNotation::Unnecessary,
            ..UseTerms::default()
        }
    }

    fn rules(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.rule.as_str()).collect()
    }

    fn meta(terms: UseTerms) -> VrmMeta {
        VrmMeta {
            name: "Model".into(),
            authors: vec!["Ann".into(), "Ben".into()],
            license_url: crate::meta::VRM_LICENSE_URLS[0].into(),
            terms,
            ..VrmMeta::default()
        }
    }

    #[test]
    fn the_default_terms_give_every_row_in_order() {
        assert_eq!(
            rules(&meta(UseTerms::default()).perform_findings()),
            [
                "rights.avatar_permission",
                "rights.perform_non_profit",
                "rights.credit_required",
                "rights.content_restricted"
            ]
        );
        assert_eq!(
            rules(&UseTerms::default().perform_findings()),
            ["rights.avatar_permission", "rights.perform_non_profit", "rights.content_restricted"],
            "credit is a meta row, not a terms row"
        );
    }

    #[test]
    fn only_author_has_its_own_avatar_row() {
        let rows = UseTerms::default().perform_findings();
        let f = &rows[0];
        assert_eq!((f.rule.as_str(), f.severity), ("rights.avatar_permission", crate::Severity::Warning));
        assert!(f.message.contains("only the author may perform"), "{}", f.message);
        assert!(f.message.contains("\"onlyAuthor\"") && f.message.contains("confirm that is you"), "{}", f.message);
        assert!(!f.message.contains("licensed separately"), "{}", f.message);
        assert_eq!(f.hint.as_deref(), Some("confirm you are the author before performing as it"));
    }

    #[test]
    fn only_separately_licensed_has_its_own_avatar_row() {
        let licensed = UseTerms { avatar_permission: AvatarPermission::OnlySeparatelyLicensedPerson, ..open() };
        let rows = licensed.perform_findings();
        assert_eq!(rules(&rows), ["rights.avatar_permission"]);
        let f = &rows[0];
        assert_eq!(f.severity, crate::Severity::Warning);
        assert!(f.message.contains("only people licensed separately may perform"), "{}", f.message);
        assert!(f.message.contains("\"onlySeparatelyLicensedPerson\""), "{}", f.message);
        assert!(!f.message.contains("only the author"), "{}", f.message);
        assert_eq!(
            f.hint.as_deref(),
            Some("confirm you hold a separate licence from the author before performing as it")
        );
    }

    #[test]
    fn everyone_has_no_avatar_row() {
        assert!(rules(&open().perform_findings()).iter().all(|r| *r != "rights.avatar_permission"));
    }

    #[test]
    fn the_non_profit_row_is_a_warning_only_for_personal_non_profit() {
        let rows = UseTerms { commercial_usage: CommercialUsage::PersonalNonProfit, ..open() }.perform_findings();
        assert_eq!(rules(&rows), ["rights.perform_non_profit"]);
        assert_eq!(rows[0].severity, crate::Severity::Warning);
        assert!(rows[0].message.contains("personalNonProfit"));

        for commercial in [CommercialUsage::PersonalProfit, CommercialUsage::Corporation] {
            let rows = UseTerms { commercial_usage: commercial, ..open() }.perform_findings();
            assert!(!rules(&rows).contains(&"rights.perform_non_profit"), "{commercial:?}");
        }
    }

    #[test]
    fn the_credit_row_is_info_and_absent_when_credit_is_unnecessary() {
        assert!(meta(open()).perform_findings().is_empty(), "credit unnecessary, so no credit row");

        let required =
            VrmMeta { terms: UseTerms { credit_notation: CreditNotation::Required, ..open() }, ..meta(open()) };
        let rows = required.perform_findings();
        assert_eq!(rules(&rows), ["rights.credit_required"]);
        assert_eq!(rows[0].severity, crate::Severity::Info);
        assert!(rows[0].message.ends_with("Model - Ann, Ben"), "{}", rows[0].message);
        assert!(rows[0].hint.is_some());
    }

    #[test]
    fn the_credit_line_is_the_provenance_attribution_with_or_without_a_copyright_holder() {
        let required = UseTerms { credit_notation: CreditNotation::Required, ..open() };
        for copyright in [None, Some("Someone Else")] {
            let m = VrmMeta {
                copyright_information: copyright.map(str::to_owned),
                terms: required.clone(),
                ..meta(open())
            };
            let notice = m.perform_findings().remove(0).message;
            let attribution = m.imported_provenance(None).attribution.expect("credit required gives an attribution");
            assert!(notice.ends_with(&attribution), "{notice:?} vs {attribution:?}");
        }
        let with_holder =
            VrmMeta { copyright_information: Some("Someone Else".into()), terms: required, ..meta(open()) };
        let notice = with_holder.perform_findings().remove(0).message;
        assert!(notice.ends_with("Model - Someone Else"), "the holder is the credit when one is set: {notice}");
    }

    #[test]
    fn the_content_row_names_each_restricted_flag_and_only_those() {
        let one = UseTerms { allow_antisocial_or_hate_usage: false, ..open() };
        let rows = one.perform_findings();
        assert_eq!(rules(&rows), ["rights.content_restricted"]);
        assert_eq!(rows[0].severity, crate::Severity::Info);
        assert!(rows[0].message.ends_with("allowAntisocialOrHateUsage"), "{}", rows[0].message);

        let two =
            UseTerms { allow_excessively_violent_usage: false, allow_political_or_religious_usage: false, ..open() };
        let msg = two.perform_findings().remove(0).message;
        assert!(msg.contains("allowExcessivelyViolentUsage, allowPoliticalOrReligiousUsage"), "{msg}");
        assert!(!msg.contains("allowExcessivelySexualUsage"), "{msg}");
    }

    #[test]
    fn open_terms_give_no_rows() {
        let open_terms = UseTerms { commercial_usage: CommercialUsage::PersonalProfit, ..open() };
        assert_eq!(rules(&open_terms.perform_findings()), Vec::<&str>::new());
        assert_eq!(rules(&meta(open_terms).perform_findings()), Vec::<&str>::new());
    }

    #[test]
    fn findings_are_deterministic() {
        let m = meta(UseTerms::default());
        assert_eq!(m.perform_findings(), m.perform_findings());
        assert_eq!(m.terms.perform_findings(), m.terms.perform_findings());
    }
}
