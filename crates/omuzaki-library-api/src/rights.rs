//! What an item records about rights (design D8).

use omuzaki_io_api::UseTerms;
use omuzaki_store_api::{Commercial, Consent, Provenance, Source};
use serde::{Deserialize, Serialize};

use crate::{ItemId, Version};

/// The licence of every first-party item (a project decision: free to use for any purpose).
pub const FIRST_PARTY_LICENCE: &str = "CC0-1.0";

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
    fn unknown_keys_are_refused() {
        let toml = "licence = 'CC0-1.0'\ncommercial = 'ok'\nlicense = 'typo'";
        assert!(toml::from_str::<ItemRights>(toml).is_err());
    }
}
