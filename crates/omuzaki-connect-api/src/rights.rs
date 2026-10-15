//! Rights only narrow (D6): the joins the tray's effective provenance and the host's output provenance share, with the
//! `derive` rules of `io-api.md` D10.

use std::collections::BTreeSet;

use omuzaki_store_api::{Commercial, Provenance};

/// The stricter of two commercial statuses: `no`, then `unknown`, then `ok`.
pub fn join_commercial(a: Commercial, b: Commercial) -> Commercial {
    match (a, b) {
        (Commercial::No, _) | (_, Commercial::No) => Commercial::No,
        (Commercial::Unknown, _) | (_, Commercial::Unknown) => Commercial::Unknown,
        (Commercial::Ok, Commercial::Ok) => Commercial::Ok,
    }
}

/// `base` joined with every record of `others`, strictest wins: `commercial` by [`join_commercial`], `private_test`
/// if any is, distinct licences joined with SPDX ` AND ` (an expression of its own, `MIT OR Apache-2.0`, in
/// parentheses, because `AND` binds tighter than `OR`), every distinct attribution joined with `; `, and `consent`
/// kept only when every record agrees. Licences and attributions are sorted, so the order of the records does not
/// matter. Source and author are `base`'s.
pub fn join_provenance<'a>(base: Provenance, others: impl IntoIterator<Item = &'a Provenance>) -> Provenance {
    let mut joined = base;
    let mut licences: BTreeSet<String> = BTreeSet::from([joined.licence.clone()]);
    let mut attributions: BTreeSet<String> = joined.attribution.iter().cloned().collect();
    for other in others {
        joined.commercial = join_commercial(joined.commercial, other.commercial);
        joined.private_test |= other.private_test;
        if joined.consent != other.consent {
            joined.consent = None;
        }
        licences.insert(other.licence.clone());
        attributions.extend(other.attribution.iter().cloned());
    }
    joined.licence = match licences.len() {
        1 => licences.into_iter().collect(),
        _ => licences
            .into_iter()
            .map(|l| if l.contains(char::is_whitespace) { format!("({l})") } else { l })
            .collect::<Vec<_>>()
            .join(" AND "),
    };
    attributions.retain(|a| !a.trim().is_empty());
    joined.attribution = (!attributions.is_empty()).then(|| attributions.into_iter().collect::<Vec<_>>().join("; "));
    joined
}

#[cfg(test)]
mod tests {
    use omuzaki_store_api::Source;

    use super::*;

    fn record(licence: &str, commercial: Commercial, private_test: bool) -> Provenance {
        Provenance {
            source: Source::Authored,
            author: None,
            licence: licence.into(),
            attribution: None,
            commercial,
            consent: None,
            private_test,
        }
    }

    #[test]
    fn commercial_only_narrows() {
        use Commercial::{No, Ok, Unknown};
        assert_eq!(join_commercial(Ok, Ok), Ok);
        assert_eq!(join_commercial(Ok, Unknown), Unknown);
        assert_eq!(join_commercial(Unknown, No), No);
        assert_eq!(join_commercial(No, Ok), No);
    }

    #[test]
    fn the_join_is_strictest_and_order_free() {
        let a = record("CC0-1.0", Commercial::Ok, false);
        let b = record("LicenseRef-Vendor", Commercial::No, true);
        let ab = join_provenance(a.clone(), [&b]);
        let ba = join_provenance(b.clone(), [&a]);
        assert_eq!(
            (ab.commercial, ab.private_test, ab.licence.as_str()),
            (Commercial::No, true, "CC0-1.0 AND LicenseRef-Vendor")
        );
        assert_eq!((ba.commercial, ba.private_test, &ba.licence), (ab.commercial, ab.private_test, &ab.licence));
        assert_eq!(join_provenance(a.clone(), [&a]).licence, "CC0-1.0");
    }

    #[test]
    fn a_compound_licence_keeps_its_meaning_in_the_join() {
        let a = record("CC-BY-4.0", Commercial::Ok, false);
        let b = record("MIT OR Apache-2.0", Commercial::Ok, false);
        assert_eq!(join_provenance(a.clone(), [&b]).licence, "CC-BY-4.0 AND (MIT OR Apache-2.0)");
        assert_eq!(join_provenance(b.clone(), [&b]).licence, "MIT OR Apache-2.0", "one licence stays as written");
    }

    #[test]
    fn every_attribution_survives_the_join_in_either_order() {
        let none = record("CC0-1.0", Commercial::Ok, false);
        let credit =
            |who: &str| Provenance { attribution: Some(who.into()), ..record("CC-BY-4.0", Commercial::Ok, false) };
        let (amy, bo) = (credit("By Amy"), credit("By Bo"));
        assert_eq!(join_provenance(none.clone(), [&amy]).attribution.as_deref(), Some("By Amy"));
        let forward = join_provenance(amy.clone(), [&none, &bo]).attribution;
        let backward = join_provenance(bo.clone(), [&none, &amy]).attribution;
        assert_eq!((forward.as_deref(), backward.as_deref()), (Some("By Amy; By Bo"), Some("By Amy; By Bo")));
        assert_eq!(join_provenance(none.clone(), [&none]).attribution, None);
    }
}
