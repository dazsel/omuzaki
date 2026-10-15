//! Domain separation: one tag per signing use, so a signature made for one use fails under every other (design D3).

use omuzaki_store_api::ContentHash;

/// What a signature is for. The set is closed on purpose: a new signing use is a security decision that passes
/// review as an API change. `#[non_exhaustive]` keeps adding one non-breaking.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Domain {
    /// A library item manifest (legacy canonical form; byte-compatible with `omuzaki-library-api`).
    LibraryItem,
    /// A shared source's index snapshot.
    LibraryIndex,
    /// A signed export statement.
    Export,
    /// An audit-log export.
    AuditExport,
    /// A membership log entry.
    Membership,
    /// An approval or review record.
    Approval,
    /// A review bundle's manifest.
    ReviewBundle,
    /// A key statement: rotate, link, revoke.
    KeyStatement,
    /// A signed entitlement receipt (reserved).
    Entitlement,
}

impl Domain {
    /// Every domain, for tests that range over all pairs.
    pub const ALL: [Domain; 9] = [
        Domain::LibraryItem,
        Domain::LibraryIndex,
        Domain::Export,
        Domain::AuditExport,
        Domain::Membership,
        Domain::Approval,
        Domain::ReviewBundle,
        Domain::KeyStatement,
        Domain::Entitlement,
    ];

    /// The tag bytes, `omuzaki-<use>-v<N>` and a terminating NUL, so no tag reads as a prefix of a longer one.
    pub const fn tag(self) -> &'static [u8] {
        match self {
            Domain::LibraryItem => b"omuzaki-library-item-v1\0",
            Domain::LibraryIndex => b"omuzaki-library-index-v1\0",
            Domain::Export => b"omuzaki-export-v1\0",
            Domain::AuditExport => b"omuzaki-audit-export-v1\0",
            Domain::Membership => b"omuzaki-membership-v1\0",
            Domain::Approval => b"omuzaki-approval-v1\0",
            Domain::ReviewBundle => b"omuzaki-review-bundle-v1\0",
            Domain::KeyStatement => b"omuzaki-key-statement-v1\0",
            Domain::Entitlement => b"omuzaki-entitlement-v1\0",
        }
    }
}

/// The bytes that are signed: the domain's tag followed by the 32-byte hash.
pub fn signing_message(domain: Domain, hash: &ContentHash) -> Vec<u8> {
    let tag = domain.tag();
    let mut message = Vec::with_capacity(tag.len() + 32);
    message.extend_from_slice(tag);
    message.extend_from_slice(&hash.0);
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tags_are_unique_nul_terminated_and_none_is_a_prefix_of_another() {
        for a in Domain::ALL {
            assert_eq!(a.tag().last(), Some(&0));
            assert!(a.tag().starts_with(b"omuzaki-"));
            for b in Domain::ALL {
                if a != b {
                    assert!(!b.tag().starts_with(a.tag()), "{a:?} is a prefix of {b:?}");
                }
            }
        }
    }

    #[test]
    fn the_message_is_tag_then_hash() {
        let hash = ContentHash([7; 32]);
        let message = signing_message(Domain::Export, &hash);
        assert_eq!(&message[..18], b"omuzaki-export-v1\0");
        assert_eq!(&message[18..], &[7; 32]);
    }
}
