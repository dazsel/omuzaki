//! What an item records about rights (design D8; the attestation, terms hash and asset rights follow
//! `docs/design/marketplace.md`: D5 rights table, D7 attestation, section 5.1).

use omuzaki_identity_api::key::KeyId;
use omuzaki_io_api::UseTerms;
use omuzaki_store_api::{Commercial, Consent, ContentHash, Provenance, Source};
use serde::{Deserialize, Serialize};

use crate::{ItemId, Version};

/// The licence of every first-party item (a project decision: free to use for any purpose).
pub const FIRST_PARTY_LICENCE: &str = "CC0-1.0";

/// Who asserts an item's rights, and the claim they stand on (marketplace design `docs/design/marketplace.md`, D5 rights table and D7). It is signed with the rest of the
/// manifest, so a client can show who vouched for the rights without trusting anything outside the signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Attestation {
    /// The key that asserts the rights (design D7): it is the same key as the manifest's `publisher.key`, so the claim
    /// is made in the name of the key that signs the manifest and a display name is never what vouches for it. Written
    /// as 64 hex characters; anything else is refused when the manifest is read.
    pub by: KeyId,
    /// The id of the shipped claim (the wording the asserter agreed to) the rights stand on.
    pub claim: String,
}

/// The rights of an item: the [`Provenance`] fields a part source row holds, so apply writes rows without
/// translating them. The `source` is implied: `Source::Library { item: "id@version" }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ItemRights {
    /// Who made it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// SPDX expression or `LicenseRef-<name>`.
    pub licence: String,
    /// Attribution text that must travel with the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
    /// Commercial status. A missing licence reads as `unknown`.
    pub commercial: Commercial,
    /// Consent, when a real person's likeness or voice is involved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub consent: Option<Consent>,
    /// Private test material: it applies normally and makes the character private-test.
    #[serde(default)]
    pub private_test: bool,
    /// Permissions the item grants for what it makes; anything left out is the most restrictive value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms: Option<UseTerms>,
    /// The result belongs to whoever applies it (the part rows carry `owned { asserted_by }`).
    #[serde(default)]
    pub owned: bool,
    /// Who asserts these rights and on which shipped claim. At item level `by` must equal the manifest's
    /// `publisher.key` ([`check`](crate::ItemManifest::check) and `verify` reject a mismatch). Absent for rights nobody has attested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attestation: Option<Attestation>,
    /// The hash of the custom terms text a `LicenseRef-` licence points at, so the text a person read is the text that
    /// applies. Absent when the licence needs no custom text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terms_hash: Option<ContentHash>,
}

/// What the manifest records about one binary asset before it is requested: its size and its own rights, so a client can
/// join the rights and bound the sizes first. The key is the asset's key in `assets`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetInfo {
    /// The asset's size in bytes.
    pub size: u64,
    /// The asset's own rights: the same type an item carries, joined with the item's by the rights join. An
    /// `attestation` here is ignored (never joined, never shown, not compared with the publisher key); only the
    /// item-level one is, and its `by` must be the manifest's `publisher.key`.
    pub rights: ItemRights,
}

impl ItemRights {
    /// The one constant every first-party item carries: author `Omuzaki`, `CC0-1.0`, commercial `ok`, not private
    /// test, owned. The seed lint checks each first-party manifest equals it.
    pub fn first_party() -> Self {
        Self {
            author: Some("Omuzaki".to_owned()),
            licence: FIRST_PARTY_LICENCE.to_owned(),
            attribution: None,
            commercial: Commercial::Ok,
            consent: None,
            private_test: false,
            terms: None,
            owned: true,
            attestation: None,
            terms_hash: None,
        }
    }

    /// The provenance record for something the item `id@version` made.
    pub fn to_provenance(&self, id: &ItemId, version: &Version) -> Provenance {
        Provenance {
            source: Source::Library { item: format!("{id}@{version}") },
            author: self.author.clone(),
            licence: self.licence.clone(),
            attribution: self.attribution.clone(),
            commercial: self.commercial,
            consent: self.consent.clone(),
            private_test: self.private_test,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_store_api::licence_gate;

    const KEY: KeyId = KeyId([7; 32]);

    #[test]
    fn first_party_rights_are_cc0_owned_and_pass_the_gate() {
        let rights = ItemRights::first_party();
        assert_eq!((rights.licence.as_str(), rights.commercial, rights.owned), ("CC0-1.0", Commercial::Ok, true));
        let id = ItemId::new("omuzaki/hair/twintails").unwrap();
        let p = rights.to_provenance(&id, &Version::new(1, 0, 0));
        assert_eq!(p.source, Source::Library { item: "omuzaki/hair/twintails@1.0.0".into() });
        assert_eq!(licence_gate(&p, false), Ok(()));
    }

    #[test]
    fn attestation_and_terms_hash_round_trip_and_are_skipped_when_absent() {
        let plain = toml::to_string(&ItemRights::first_party()).unwrap();
        assert!(!plain.contains("attestation") && !plain.contains("terms_hash"), "{plain}");
        let rights = ItemRights {
            attestation: Some(Attestation { by: KEY, claim: "own-work-v1".into() }),
            terms_hash: Some(ContentHash::of(b"terms")),
            ..ItemRights::first_party()
        };
        let text = toml::to_string(&rights).unwrap();
        assert!(text.contains("[attestation]") && text.contains("terms_hash"), "{text}");
        assert_eq!(toml::from_str::<ItemRights>(&text).unwrap(), rights);
        assert_eq!(toml::from_str::<ItemRights>(&plain).unwrap(), ItemRights::first_party());
    }

    #[test]
    fn asset_info_carries_the_items_own_rights_type_and_refuses_unknown_keys() {
        let info = AssetInfo { size: 42, rights: ItemRights::first_party() };
        let text = toml::to_string(&info).unwrap();
        assert_eq!(toml::from_str::<AssetInfo>(&text).unwrap(), info);
        assert!(toml::from_str::<AssetInfo>(&format!("{text}\nextra = 1")).is_err());
        let by = KEY.to_hex();
        assert!(toml::from_str::<Attestation>(&format!("by = '{by}'\nclaim = 'y'\nextra = 1")).is_err());
    }

    #[test]
    fn the_attesting_key_round_trips_as_hex_and_a_malformed_key_is_refused() {
        let attestation = Attestation { by: KEY, claim: "own-work-v1".into() };
        let text = toml::to_string(&attestation).unwrap();
        assert!(text.contains(&format!("by = \"{}\"", "07".repeat(32))), "{text}");
        assert_eq!(toml::from_str::<Attestation>(&text).unwrap(), attestation);
        for bad in ["them", "", &"07".repeat(31), &"zz".repeat(32), &"07".repeat(33)] {
            let text = format!("by = '{bad}'\nclaim = 'y'");
            assert!(toml::from_str::<Attestation>(&text).is_err(), "{bad:?} must not read as a key");
        }
    }

    #[test]
    fn unknown_keys_are_refused() {
        let toml = "licence = 'CC0-1.0'\ncommercial = 'ok'\nlicense = 'typo'";
        assert!(toml::from_str::<ItemRights>(toml).is_err());
    }
}
