//! The rights rows for performing as an imported avatar: what the terms allow, and the credit line to copy.
//!
//! Performing on your own machine is not publishing, so none of these rows blocks anything. Each one is a note with a
//! sentence and a fix, shown at import beside the other rights notes (`rights.unrecognised_license_url`,
//! `rights.import_restricted`) so the import op, the licence ops and the go-live notice share one copy.

use super::{AvatarPermission, CommercialUsage, CreditNotation, UseTerms, VrmMeta, spelled};
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
    /// avatar permission, non-profit, credit, content. The credit line names the model and every author, in file order.
    pub fn perform_findings(&self) -> Vec<Finding> {
        let credit = (self.terms.credit_notation == CreditNotation::Required).then(|| self.credit_finding());
        perform_rows(&self.terms, credit)
    }

    /// The credit line to put in stream titles: the model's name, then its authors in file order.
    fn credit_finding(&self) -> Finding {
        let authors = self.authors.join(", ");
        let line = match (self.name.is_empty(), authors.is_empty()) {
            (_, true) => self.name.clone(),
            (true, false) => authors,
            (false, false) => format!("{} - {authors}", self.name),
        };
        Finding::info("rights.credit_required", format!("credit is required; put this line in stream titles: {line}"))
            .at("meta/creditNotation")
            .hint("copy the credit line into the stream title")
    }
}

/// The rows in their fixed order. `credit` is only given by [`VrmMeta::perform_findings`], and sits after the terms
/// rows so the output is the same on every call.
fn perform_rows(terms: &UseTerms, credit: Option<Finding>) -> Vec<Finding> {
    let mut out = Vec::new();
    if terms.avatar_permission != AvatarPermission::Everyone {
        out.push(
            Finding::warning(
                "rights.avatar_permission",
                format!(
                    "only its author, or people licensed separately, may perform as this avatar \
                     (avatarPermission is {}); confirm that is you",
                    spelled(&terms.avatar_permission)
                ),
            )
            .at("meta/avatarPermission")
            .hint("confirm you are the author, or hold a separate licence, before performing as it"),
        );
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
    fn the_avatar_row_is_a_warning_for_either_restricted_permission_and_absent_for_everyone() {
        let only_author = UseTerms::default().perform_findings();
        let f = &only_author[0];
        assert_eq!((f.rule.as_str(), f.severity), ("rights.avatar_permission", crate::Severity::Warning));
        assert!(f.message.contains("\"onlyAuthor\"") && f.message.contains("confirm that is you"), "{}", f.message);
        assert!(f.hint.is_some());

        let licensed = UseTerms { avatar_permission: AvatarPermission::OnlySeparatelyLicensedPerson, ..open() };
        let rows = licensed.perform_findings();
        assert_eq!(rules(&rows), ["rights.avatar_permission"]);
        assert!(rows[0].message.contains("\"onlySeparatelyLicensedPerson\""));
        assert!(open().perform_findings().is_empty(), "everyone is not a row");
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
    fn the_credit_row_is_info_with_the_name_and_every_author_in_file_order() {
        let rows = meta(open()).perform_findings();
        assert!(rows.is_empty(), "credit unnecessary, so no credit row");

        let required = VrmMeta {
            terms: UseTerms { credit_notation: CreditNotation::Required, ..open() },
            copyright_information: Some("Someone Else".into()),
            ..meta(open())
        };
        let rows = required.perform_findings();
        assert_eq!(rules(&rows), ["rights.credit_required"]);
        assert_eq!(rows[0].severity, crate::Severity::Info);
        assert!(rows[0].message.ends_with("Model - Ann, Ben"), "{}", rows[0].message);
        assert!(!rows[0].message.contains("Someone Else"), "the copyright holder is not the credit line here");
        assert!(rows[0].hint.is_some());
    }

    #[test]
    fn the_credit_line_leaves_out_an_empty_name_or_no_authors() {
        let required = UseTerms { credit_notation: CreditNotation::Required, ..open() };
        let line = |m: VrmMeta| m.perform_findings().remove(0).message;
        let no_name = VrmMeta { name: String::new(), terms: required.clone(), ..meta(open()) };
        assert!(line(no_name).ends_with(": Ann, Ben"));
        let no_authors = VrmMeta { authors: vec![], terms: required.clone(), ..meta(open()) };
        assert!(line(no_authors).ends_with(": Model"));
        assert!(line(VrmMeta { terms: required, ..meta(open()) }).ends_with(": Model - Ann, Ben"));
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
