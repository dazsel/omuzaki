//! The pure join of rights (marketplace design `docs/design/marketplace.md`, D5 rights table and section 5.1): many rows of rights become the one record that allows no more than any of them.
//!
//! `licence.derive` joins a character's parts and application records with it; the marketplace client joins an item's
//! rights with its assets' rights by the same rule, so the rule lives here, in a contract crate both can call. It reads
//! nothing and never guesses:
//! - every permission is the **strictest** of the rows: the commercial status (`no`, then `unknown`, then `ok`), the
//!   private-test flag (any row makes the result private test material) and each of the nine flags of the terms
//!   ([`AXES`]);
//! - the licence is every distinct licence joined with SPDX `AND` (all of them apply); authors and attributions are the
//!   **union** of the rows', in row order, and a blank one counts as none;
//! - different consents cannot be one: the join reports them as [`JointConsent::Ambiguous`] and keeps none.

use omuzaki_io_api::{AvatarPermission, CommercialUsage, CreditNotation, Modification, UseTerms};
use omuzaki_store_api::{Commercial, Consent};
use serde::Serialize;

/// The licence of a row nobody has recorded a source for, and of a join of no rows at all.
pub const UNKNOWN_LICENCE: &str = "LicenseRef-Unknown";

/// One permission flag of [`UseTerms`].
pub struct Axis {
    /// The key in the `meta` table of a record (`commercialUsage`), which is how the file and the schema spell it.
    pub key: &'static str,
    /// How much the flag grants: 0 is the most restrictive value, higher grants more.
    rank: fn(&UseTerms) -> u8,
    /// Sets this flag of `to` to the one of `from`.
    pub copy: fn(&mut UseTerms, &UseTerms),
    /// The flag's value as the record writes it (`personalProfit`, `true`), for messages.
    show: fn(&UseTerms) -> String,
}

impl Axis {
    /// How much this flag of `terms` grants; 0 is the most restrictive value.
    pub fn rank_of(&self, terms: &UseTerms) -> u8 {
        (self.rank)(terms)
    }

    /// This flag of `terms`, as the record writes it.
    pub fn show_of(&self, terms: &UseTerms) -> String {
        (self.show)(terms)
    }
}

fn avatar_rank(value: AvatarPermission) -> u8 {
    match value {
        AvatarPermission::OnlyAuthor => 0,
        AvatarPermission::OnlySeparatelyLicensedPerson => 1,
        AvatarPermission::Everyone => 2,
    }
}

fn commercial_usage_rank(value: CommercialUsage) -> u8 {
    match value {
        CommercialUsage::PersonalNonProfit => 0,
        CommercialUsage::PersonalProfit => 1,
        CommercialUsage::Corporation => 2,
    }
}

fn modification_rank(value: Modification) -> u8 {
    match value {
        Modification::Prohibited => 0,
        Modification::AllowModification => 1,
        Modification::AllowModificationRedistribution => 2,
    }
}

/// Omitting the credit grants more than requiring it.
fn credit_rank(value: CreditNotation) -> u8 {
    match value {
        CreditNotation::Required => 0,
        CreditNotation::Unnecessary => 1,
    }
}

/// Allowing a use grants more than forbidding it.
fn flag_rank(value: bool) -> u8 {
    u8::from(value)
}

/// A value as the record writes it: a string without its quotes, anything else as JSON.
fn written<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => text,
        Ok(other) => other.to_string(),
        Err(_) => String::new(),
    }
}

macro_rules! axis {
    ($key:literal, $field:ident, $rank:path) => {
        Axis {
            key: $key,
            rank: |terms| $rank(terms.$field),
            copy: |to, from| to.$field = from.$field,
            show: |terms| written(&terms.$field),
        }
    };
}

/// Every flag of [`UseTerms`], in the order the file lists them.
pub const AXES: [Axis; 9] = [
    axis!("avatarPermission", avatar_permission, avatar_rank),
    axis!("allowExcessivelyViolentUsage", allow_excessively_violent_usage, flag_rank),
    axis!("allowExcessivelySexualUsage", allow_excessively_sexual_usage, flag_rank),
    axis!("commercialUsage", commercial_usage, commercial_usage_rank),
    axis!("allowPoliticalOrReligiousUsage", allow_political_or_religious_usage, flag_rank),
    axis!("allowAntisocialOrHateUsage", allow_antisocial_or_hate_usage, flag_rank),
    axis!("creditNotation", credit_notation, credit_rank),
    axis!("allowRedistribution", allow_redistribution, flag_rank),
    axis!("modification", modification, modification_rank),
];

/// The terms that grant least on every flag across `terms`: each flag is the most restrictive value any of them has.
/// Nothing at all joins to the spec defaults (the most restrictive value of every flag), never to something looser.
pub fn join_terms<'a>(terms: impl IntoIterator<Item = &'a UseTerms>) -> UseTerms {
    let terms: Vec<&UseTerms> = terms.into_iter().collect();
    let mut joined = UseTerms::default();
    for axis in &AXES {
        if let Some(strictest) = terms.iter().min_by_key(|t| axis.rank_of(t)) {
            (axis.copy)(&mut joined, strictest);
        }
    }
    joined
}

/// The flags on which `terms` grants more than `limit` does.
pub fn exceeding(terms: &UseTerms, limit: &UseTerms) -> Vec<&'static Axis> {
    AXES.iter().filter(|axis| axis.rank_of(terms) > axis.rank_of(limit)).collect()
}

/// How much a commercial status grants: `No` is a known restriction, `Unknown` is one nobody has established yet (the
/// licence gate treats it as `No`, but a person can clear it by reading the licence), `Ok` grants the most.
pub fn commercial_rank(status: Commercial) -> u8 {
    match status {
        Commercial::No => 0,
        Commercial::Unknown => 1,
        Commercial::Ok => 2,
    }
}

/// One contribution to the join: what a part, an application record, an item or an asset says about its rights. The
/// caller has already settled what the row grants (the terms of private test material, credit required by an
/// attribution); the join only combines.
#[derive(Clone, Debug, PartialEq)]
pub struct RightsRow {
    /// What the row is called in a message about it (a part id, `applied/<app>`, an asset key).
    pub name: String,
    /// Its SPDX expression or `LicenseRef-<name>`. The join trims it; a blank one reads as [`UNKNOWN_LICENCE`].
    pub licence: String,
    /// Its commercial status.
    pub commercial: Commercial,
    /// The consent it records, if a real person's likeness or voice is involved.
    pub consent: Option<Consent>,
    /// The terms it grants.
    pub terms: UseTerms,
    /// Whether it is private test material.
    pub private_test: bool,
    /// Who made it, if recorded. The join trims it and drops a blank one.
    pub author: Option<String>,
    /// The credit that must travel with it, if any. The join trims it and drops a blank one.
    pub attribution: Option<String>,
    /// Who marked it as the project's own, if anyone did.
    pub owned_by: Option<String>,
}

/// What the consents of the rows join to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JointConsent {
    /// No row records a consent.
    None,
    /// Every row that records a consent records this one.
    Agreed(Consent),
    /// The rows record different consents, which cannot be one; none is kept. Each row that records one, by name.
    Ambiguous(Vec<(String, Consent)>),
}

/// The join of a set of [`RightsRow`]s.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct JoinedRights {
    /// Every distinct licence joined with `AND`; [`UNKNOWN_LICENCE`] for no rows.
    pub licence: String,
    /// The strictest commercial status (`Unknown` for no rows).
    pub commercial: Commercial,
    /// Whether any row is private test material.
    pub private_test: bool,
    /// The strictest of every flag; the defaults when the status is not `ok` or the result is private test material.
    pub terms: UseTerms,
    /// The authors, each once, in row order.
    pub authors: Vec<String>,
    /// The credit lines, each once, in row order.
    pub attributions: Vec<String>,
    /// Who marked rows as the project's own (sorted, without repeats).
    pub owners: Vec<String>,
    /// Whether every row is marked as the project's own and none is private test material. False for no rows.
    pub project_owned: bool,
    /// What the consents join to.
    pub consent: JointConsent,
}

/// The text without surrounding whitespace, or `None` when nothing is left.
fn trimmed(text: &str) -> Option<&str> {
    Some(text.trim()).filter(|t| !t.is_empty())
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
fn join_licences(rows: &[RightsRow]) -> String {
    let mut licences: Vec<&str> = rows.iter().map(|r| trimmed(&r.licence).unwrap_or(UNKNOWN_LICENCE)).collect();
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

/// The consent the rows can hold: the one every row that has a consent agrees on. Different consents cannot be one.
fn joint_consent(rows: &[RightsRow]) -> JointConsent {
    let consents: Vec<(String, &Consent)> =
        rows.iter().filter_map(|r| r.consent.as_ref().map(|c| (r.name.clone(), c))).collect();
    let Some(first) = consents.first().map(|(_, c)| *c) else { return JointConsent::None };
    if consents.iter().all(|(_, c)| *c == first) {
        return JointConsent::Agreed(first.clone());
    }
    JointConsent::Ambiguous(consents.into_iter().map(|(name, c)| (name, c.clone())).collect())
}

/// Joins `rows` into the one record that allows no more than any of them. Pure: it reads nothing, so every rule is
/// testable alone. The rows are joined in the order given, which decides only the order of authors and credit lines.
pub fn join_rights(rows: &[RightsRow]) -> JoinedRights {
    let private_test = rows.iter().any(|r| r.private_test);
    let commercial =
        rows.iter().map(|r| r.commercial).min_by_key(|c| commercial_rank(*c)).unwrap_or(Commercial::Unknown);
    let mut terms = join_terms(rows.iter().map(|r| &r.terms));
    // The exporters' rule (`rights.terms_exceed_provenance`): a status that is not `ok`, and private test material, carry
    // only the most restrictive value of every flag. The join of rows that are individually fine can still land here,
    // for instance when one of them is unknown.
    if commercial != Commercial::Ok || private_test {
        terms = UseTerms::default();
    }
    let mut owners = unique(rows.iter().filter_map(|r| r.owned_by.clone()));
    owners.sort_unstable();
    JoinedRights {
        licence: join_licences(rows),
        commercial,
        private_test,
        terms,
        authors: unique(rows.iter().filter_map(|r| r.author.as_deref().and_then(trimmed).map(str::to_owned))),
        attributions: unique(rows.iter().filter_map(|r| r.attribution.as_deref().and_then(trimmed).map(str::to_owned))),
        owners,
        project_owned: !rows.is_empty() && !private_test && rows.iter().all(|r| r.owned_by.is_some()),
        consent: joint_consent(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str, licence: &str) -> RightsRow {
        RightsRow {
            name: name.to_owned(),
            licence: licence.to_owned(),
            commercial: Commercial::Ok,
            consent: None,
            terms: UseTerms::default(),
            private_test: false,
            author: None,
            attribution: None,
            owned_by: None,
        }
    }

    fn consent(person: &str) -> Consent {
        Consent { person: person.to_owned(), scope: "likeness".to_owned(), document: None }
    }

    #[test]
    fn cc_by_with_mit_is_both_joined_with_and_in_sorted_order() {
        let joined = join_rights(&[row("item", "MIT"), row("asset", "CC-BY-4.0")]);
        assert_eq!(joined.licence, "CC-BY-4.0 AND MIT");
        assert_eq!(join_rights(&[row("a", "MIT"), row("b", "MIT")]).licence, "MIT", "the same licence twice is once");
    }

    #[test]
    fn surrounding_whitespace_does_not_make_a_second_term_and_blank_credits_are_dropped() {
        assert_eq!(join_rights(&[row("a", " MIT "), row("b", "MIT")]).licence, "MIT");
        assert_eq!(join_rights(&[row("a", "  ")]).licence, UNKNOWN_LICENCE, "a blank licence is unknown");
        let rows = [
            RightsRow { author: Some("  ".into()), attribution: Some(String::new()), ..row("a", "MIT") },
            RightsRow { author: Some(" Amy ".into()), attribution: Some(" by Amy ".into()), ..row("b", "MIT") },
            RightsRow { author: Some("Amy".into()), attribution: Some("by Amy".into()), ..row("c", "MIT") },
        ];
        let joined = join_rights(&rows);
        assert_eq!((joined.authors, joined.attributions), (vec!["Amy".to_owned()], vec!["by Amy".to_owned()]));
    }

    #[test]
    fn a_licence_ref_with_an_spdx_id_is_kept_whole_in_both_orientations() {
        let forward = join_rights(&[row("item", "LicenseRef-Studio-Terms"), row("asset", "CC-BY-4.0")]);
        let reverse = join_rights(&[row("asset", "CC-BY-4.0"), row("item", "LicenseRef-Studio-Terms")]);
        assert_eq!(forward.licence, "CC-BY-4.0 AND LicenseRef-Studio-Terms");
        assert_eq!(forward.licence, reverse.licence, "the order of the rows does not change the licence");
    }

    #[test]
    fn an_expression_of_its_own_is_parenthesised_because_and_binds_tighter_than_or() {
        let rows = [row("a", "MIT OR Apache-2.0"), row("b", "CC-BY-4.0")];
        assert_eq!(join_rights(&rows).licence, "CC-BY-4.0 AND (MIT OR Apache-2.0)");
        assert_eq!(join_rights(&rows[..1]).licence, "MIT OR Apache-2.0", "one row needs no brackets");
    }

    #[test]
    fn an_asset_stricter_than_the_item_pulls_every_flag_down() {
        let loose = UseTerms {
            allow_redistribution: true,
            commercial_usage: CommercialUsage::Corporation,
            ..UseTerms::default()
        };
        let item = RightsRow { terms: loose.clone(), ..row("item", "CC0-1.0") };
        let asset = RightsRow {
            commercial: Commercial::No,
            private_test: false,
            terms: UseTerms { allow_redistribution: true, ..UseTerms::default() },
            ..row("asset", "CC-BY-NC-4.0")
        };
        assert_eq!(join_rights(std::slice::from_ref(&item)).terms, loose);
        let joined = join_rights(&[item, asset]);
        assert_eq!(joined.commercial, Commercial::No);
        assert_eq!(joined.terms, UseTerms::default(), "a status that is not ok grants nothing");
        assert_eq!(joined.licence, "CC-BY-NC-4.0 AND CC0-1.0");
    }

    #[test]
    fn a_known_restriction_beats_an_unknown_status_which_beats_a_clearance() {
        let mut rows = [row("a", "CC0-1.0"), row("b", "CC0-1.0"), row("c", "CC0-1.0")];
        rows[1].commercial = Commercial::Unknown;
        assert_eq!(join_rights(&rows).commercial, Commercial::Unknown);
        rows[2].commercial = Commercial::No;
        assert_eq!(join_rights(&rows).commercial, Commercial::No);
        assert!(commercial_rank(Commercial::No) < commercial_rank(Commercial::Unknown));
        assert!(commercial_rank(Commercial::Unknown) < commercial_rank(Commercial::Ok));
    }

    #[test]
    fn private_test_material_grants_nothing_and_taints_the_join() {
        let loose = UseTerms { allow_redistribution: true, ..UseTerms::default() };
        let mut rows = [
            RightsRow { terms: loose.clone(), ..row("a", "CC0-1.0") },
            RightsRow { terms: loose.clone(), ..row("b", "CC0-1.0") },
        ];
        assert_eq!(join_rights(&rows).terms, loose);
        rows[1].private_test = true;
        let joined = join_rights(&rows);
        assert!(joined.private_test && !joined.project_owned);
        assert_eq!(joined.terms, UseTerms::default());
    }

    #[test]
    fn differing_consents_are_ambiguous_with_none_kept_and_equal_ones_agree() {
        let mut rows = [row("a", "CC0-1.0"), row("b", "CC0-1.0"), row("c", "CC0-1.0")];
        assert_eq!(join_rights(&rows).consent, JointConsent::None);
        rows[0].consent = Some(consent("Amy"));
        assert_eq!(join_rights(&rows).consent, JointConsent::Agreed(consent("Amy")));
        rows[2].consent = Some(consent("Amy"));
        assert_eq!(join_rights(&rows).consent, JointConsent::Agreed(consent("Amy")));
        rows[2].consent = Some(consent("Bo"));
        assert_eq!(
            join_rights(&rows).consent,
            JointConsent::Ambiguous(vec![("a".to_owned(), consent("Amy")), ("c".to_owned(), consent("Bo"))])
        );
    }

    #[test]
    fn authors_and_credit_are_a_union_in_row_order_and_owners_are_sorted() {
        let mut rows = [row("a", "CC0-1.0"), row("b", "CC0-1.0"), row("c", "CC0-1.0")];
        for (r, (who, owner)) in rows.iter_mut().zip([("Zed", "Zoe"), ("Amy", "Abe"), ("Zed", "Zoe")]) {
            r.author = Some(who.to_owned());
            r.attribution = Some(format!("By {who}"));
            r.owned_by = Some(owner.to_owned());
        }
        let joined = join_rights(&rows);
        assert_eq!(joined.authors, ["Zed", "Amy"]);
        assert_eq!(joined.attributions, ["By Zed", "By Amy"]);
        assert_eq!(joined.owners, ["Abe", "Zoe"]);
        assert!(joined.project_owned);
        rows[1].owned_by = None;
        assert!(!join_rights(&rows).project_owned, "one row that is not the project's own spoils it");
    }

    #[test]
    fn no_rows_join_to_the_most_restrictive_record_and_are_nobodys() {
        let joined = join_rights(&[]);
        assert_eq!((joined.licence.as_str(), joined.commercial), (UNKNOWN_LICENCE, Commercial::Unknown));
        assert_eq!(joined.terms, UseTerms::default());
        assert!(!joined.project_owned && joined.authors.is_empty() && joined.consent == JointConsent::None);
    }

    #[test]
    fn each_axis_takes_the_strictest_value_and_nothing_joins_to_the_defaults() {
        let up = UseTerms {
            avatar_permission: AvatarPermission::Everyone,
            commercial_usage: CommercialUsage::Corporation,
            modification: Modification::AllowModificationRedistribution,
            credit_notation: CreditNotation::Unnecessary,
            allow_redistribution: true,
            ..UseTerms::default()
        };
        assert_eq!(join_terms([&up, &up]), up);
        assert_eq!(join_terms([&up, &UseTerms::default()]), UseTerms::default());
        assert_eq!(join_terms(std::iter::empty()), UseTerms::default());
        assert_eq!(exceeding(&up, &UseTerms::default()).len(), 5);
        assert!(exceeding(&UseTerms::default(), &up).is_empty());
        let keys: Vec<&str> = AXES.iter().map(|a| a.key).collect();
        assert_eq!(keys.len(), 9);
        assert_eq!(AXES[3].show_of(&up), "corporation");
    }
}
