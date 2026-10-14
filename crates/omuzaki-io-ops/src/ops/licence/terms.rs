//! The nine permission flags of `VRMC_vrm.meta` as axes with an order: which of two values grants more.
//!
//! The join of the parts' terms is "each flag the most restrictive of the parts" and `licence.set`'s check is "no flag
//! of the record grants more than the join". Both are the same question asked of one flag at a time, so each flag is
//! one [`Axis`]: its name in the file (the key a person sees in the record), its rank (0 is the most restrictive
//! value), and how to copy it. `UseTerms::is_within` answers the same question as a yes or no; the axes also say
//! *which* flag, which is what an error has to name, and a test checks that the two agree.

use omuzaki_io_api::{AvatarPermission, CommercialUsage, CreditNotation, Modification, UseTerms};
use omuzaki_store_api::Commercial;
use serde::Serialize;

/// One permission flag of [`UseTerms`].
pub(crate) struct Axis {
    /// The key in the `meta` table of a record (`commercialUsage`), which is how the file and the schema spell it.
    pub key: &'static str,
    /// How much the flag grants: 0 is the most restrictive value, higher grants more.
    rank: fn(&UseTerms) -> u8,
    /// Sets this flag of `to` to the one of `from`.
    copy: fn(&mut UseTerms, &UseTerms),
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
pub(crate) const AXES: [Axis; 9] = [
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
pub(crate) fn join<'a>(terms: impl IntoIterator<Item = &'a UseTerms>) -> UseTerms {
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
pub(crate) fn exceeding(terms: &UseTerms, limit: &UseTerms) -> Vec<&'static Axis> {
    AXES.iter().filter(|axis| axis.rank_of(terms) > axis.rank_of(limit)).collect()
}

/// How much a commercial status grants: `No` is a known restriction, `Unknown` is one nobody has established yet (the
/// licence gate treats it as `No`, but a person can clear it by reading the licence), `Ok` grants the most.
pub(crate) fn commercial_rank(status: Commercial) -> u8 {
    match status {
        Commercial::No => 0,
        Commercial::Unknown => 1,
        Commercial::Ok => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(change: impl FnOnce(&mut UseTerms)) -> UseTerms {
        let mut terms = UseTerms::default();
        change(&mut terms);
        terms
    }

    /// One terms value per flag with that flag at its most permissive value, and the rest at the defaults.
    fn one_flag_raised() -> Vec<UseTerms> {
        vec![
            with(|t| t.avatar_permission = AvatarPermission::Everyone),
            with(|t| t.allow_excessively_violent_usage = true),
            with(|t| t.allow_excessively_sexual_usage = true),
            with(|t| t.commercial_usage = CommercialUsage::Corporation),
            with(|t| t.allow_political_or_religious_usage = true),
            with(|t| t.allow_antisocial_or_hate_usage = true),
            with(|t| t.credit_notation = CreditNotation::Unnecessary),
            with(|t| t.allow_redistribution = true),
            with(|t| t.modification = Modification::AllowModificationRedistribution),
        ]
    }

    #[test]
    fn the_keys_are_exactly_the_keys_the_record_writes_for_the_terms() {
        let written = serde_json::to_value(UseTerms::default()).unwrap();
        let mut keys: Vec<&str> = written.as_object().unwrap().keys().map(String::as_str).collect();
        let mut axes: Vec<&str> = AXES.iter().map(|a| a.key).collect();
        keys.sort_unstable();
        axes.sort_unstable();
        assert_eq!(axes, keys, "a flag added to UseTerms needs an axis here");
    }

    #[test]
    fn each_axis_agrees_with_is_within_one_flag_at_a_time() {
        let raised = one_flag_raised();
        let defaults = UseTerms::default();
        for (axis, terms) in AXES.iter().zip(&raised) {
            let named: Vec<&str> = exceeding(terms, &defaults).iter().map(|a| a.key).collect();
            assert_eq!(named, [axis.key], "raising {} exceeds the defaults on that flag alone", axis.key);
            assert!(!terms.is_within(&defaults), "{}", axis.key);
            assert!(exceeding(&defaults, terms).is_empty() && defaults.is_within(terms), "{}", axis.key);
        }
        // The middle values of the three-valued flags rank between the ends.
        let middle = [
            with(|t| t.avatar_permission = AvatarPermission::OnlySeparatelyLicensedPerson),
            with(|t| t.commercial_usage = CommercialUsage::PersonalProfit),
            with(|t| t.modification = Modification::AllowModification),
        ];
        for (terms, ends) in middle.iter().zip([&raised[0], &raised[3], &raised[8]]) {
            assert!(terms.is_within(ends) && !ends.is_within(terms));
            assert_eq!(exceeding(ends, terms).len(), 1);
            assert!(exceeding(terms, ends).is_empty());
        }
    }

    #[test]
    fn several_flags_at_once_agree_with_is_within() {
        let raised = one_flag_raised();
        for (a, first) in raised.iter().enumerate() {
            for (b, second) in raised.iter().enumerate() {
                let both = join_raised(first, second);
                assert!(first.is_within(&both) && second.is_within(&both));
                let named = exceeding(&both, first).len();
                assert_eq!(named, usize::from(a != b), "{a} and {b}");
                assert_eq!(both.is_within(first), named == 0);
            }
        }
    }

    /// The loosest terms that are no tighter than either: each flag the more permissive of the two.
    fn join_raised(a: &UseTerms, b: &UseTerms) -> UseTerms {
        let mut loosest = UseTerms::default();
        for axis in &AXES {
            let from = if axis.rank_of(a) >= axis.rank_of(b) { a } else { b };
            (axis.copy)(&mut loosest, from);
        }
        loosest
    }

    #[test]
    fn the_join_takes_each_flag_from_the_strictest_part() {
        let raised = one_flag_raised();
        // Every part grants everything on its own flag only, so the join grants nothing.
        assert_eq!(join(&raised), UseTerms::default());
        // Parts that all grant the same keep it; one part holding it back removes it.
        let loose = join_raised(&raised[3], &raised[7]);
        assert_eq!(join([&loose, &loose]), loose);
        assert!(!join([&loose, &raised[3]]).allow_redistribution);
        assert_eq!(join([&loose, &raised[3]]).commercial_usage, CommercialUsage::Corporation);
        assert_eq!(join([&loose, &UseTerms::default()]), UseTerms::default());
    }

    #[test]
    fn nothing_to_join_is_the_most_restrictive_terms() {
        assert_eq!(join(std::iter::empty()), UseTerms::default());
    }

    #[test]
    fn values_are_written_the_way_the_record_spells_them() {
        let terms = with(|t| {
            t.commercial_usage = CommercialUsage::PersonalProfit;
            t.allow_redistribution = true;
        });
        let show = |key: &str| AXES.iter().find(|a| a.key == key).unwrap().show_of(&terms);
        assert_eq!(show("commercialUsage"), "personalProfit");
        assert_eq!(show("allowRedistribution"), "true");
        assert_eq!(show("avatarPermission"), "onlyAuthor");
    }

    #[test]
    fn a_known_restriction_outranks_an_unknown_status_which_outranks_a_clearance() {
        assert!(commercial_rank(Commercial::No) < commercial_rank(Commercial::Unknown));
        assert!(commercial_rank(Commercial::Unknown) < commercial_rank(Commercial::Ok));
    }
}
