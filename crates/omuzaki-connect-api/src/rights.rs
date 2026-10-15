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
/// if any is, distinct licences joined with SPDX ` AND ` (sorted, so the order of the records does not matter), and
/// `consent` kept only when every record agrees. Source, author and attribution are `base`'s.
pub fn join_provenance<'a>(base: Provenance, others: impl IntoIterator<Item = &'a Provenance>) -> Provenance {
    let mut joined = base;
    let mut licences: BTreeSet<String> = BTreeSet::from([joined.licence.clone()]);
    for other in others {
        joined.commercial = join_commercial(joined.commercial, other.commercial);
        joined.private_test |= other.private_test;
        if joined.consent != other.consent {
            joined.consent = None;
        }
        licences.insert(other.licence.clone());
    }
    joined.licence = licences.into_iter().collect::<Vec<_>>().join(" AND ");
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
}
