//! Publishers and signatures (design D2): who an item claims to come from, and whether the claim holds.
//!
//! A name is never trusted without its key. An item's identity is its key and id together; an unsigned item, or
//! one whose signature fails, is "unverified publisher" whatever name it claims; `omuzaki/` ids are valid only
//! under the Omuzaki key. This module only verifies. Key generation, storage and signing arrive with saving (LB-03)
//! and publishing (LB-07, LB-08).

use ed25519_dalek::{Signature, VerifyingKey};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::ItemManifest;

/// A publisher: a display name and an ed25519 public key.
///
/// The key is the 32-byte public key as 64 lowercase hex characters. The design says "key fingerprint"; a
/// signature can only be checked against the key itself, so the key is stored in full and
/// [`PublisherKey::fingerprint`] is the short form people see.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    /// What people see. Never trusted without the key.
    pub name: String,
    /// The public key, 64 hex characters.
    pub key: String,
}

/// An ed25519 public key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct PublisherKey(pub [u8; 32]);

impl PublisherKey {
    /// Reads 64 hex characters (either case).
    ///
    /// # Errors
    /// A sentence saying what is wrong.
    pub fn from_hex(s: &str) -> Result<Self, String> {
        Ok(Self(hex_array::<32>(s).ok_or("a publisher key is 64 hex characters (an ed25519 public key)")?))
    }

    /// The key as 64 lowercase hex characters.
    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    /// A 16-character hex fingerprint (BLAKE3 of the key), for display and for pins.
    pub fn fingerprint(&self) -> String {
        ContentHash::of(&self.0).to_string()[..16].to_owned()
    }
}

/// The Omuzaki publisher key every `omuzaki/` item must be signed with once packs are signed (LB-02).
///
/// `None` until the key ceremony, so today no installed item can claim `omuzaki/`; compiled-in sources are Omuzaki's by
/// construction and need no key.
pub const OMUZAKI_PUBLISHER_KEY: Option<PublisherKey> = None;

/// Where a manifest came from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Compiled into this build by an `ItemSource`: Omuzaki's by construction, with no publisher in its files.
    CompiledIn,
    /// Read from a person's library, a pack, a team scope or a file.
    Installed,
}

/// The outcome of [`ItemManifest::verify`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublisherCheck {
    /// Compiled in, or signed by the Omuzaki key.
    FirstParty,
    /// The signature holds for this key. The name is what the publisher chose to show.
    Verified {
        /// The display name.
        name: String,
        /// The key's [`fingerprint`](PublisherKey::fingerprint).
        fingerprint: String,
    },
    /// Shown as "unverified publisher": unsigned, or the signature or key doesn't hold.
    Unverified {
        /// Why.
        reason: String,
    },
    /// Must not be installed: it claims a namespace its key doesn't own (`omuzaki/`, or the fixture-only `test/`).
    Refused {
        /// Why.
        reason: String,
    },
}

/// What a signature covers: a domain tag and the canonical hash, so a signature for an item can't be replayed as a
/// signature over anything else.
const SIGNING_TAG: &[u8] = b"omuzaki-library-item-v1\0";

impl ItemManifest {
    /// The bytes a publisher signs: a domain tag plus the canonical hash. The hash covers the id and every value,
    /// so a signature binds the key to this exact item. Signing (LB-07) uses the same function.
    pub fn signing_message(&self) -> Vec<u8> {
        let mut message = SIGNING_TAG.to_vec();
        message.extend_from_slice(&self.hash().0);
        message
    }

    /// [`verify_with`](Self::verify_with) against [`OMUZAKI_PUBLISHER_KEY`].
    pub fn verify(&self, origin: Origin) -> PublisherCheck {
        self.verify_with(origin, OMUZAKI_PUBLISHER_KEY.as_ref())
    }

    /// Decides how far to trust the publisher claim. `omuzaki` is the Omuzaki key, if one exists.
    pub fn verify_with(&self, origin: Origin, omuzaki: Option<&PublisherKey>) -> PublisherCheck {
        let Ok(id) = self.item_id() else {
            return PublisherCheck::Unverified { reason: format!("{:?} is not a valid item id", self.id) };
        };
        if origin == Origin::CompiledIn {
            let owned_namespace = id.is_first_party() || id.is_fixture();
            return if owned_namespace && self.publisher.is_none() && self.signature.is_none() {
                PublisherCheck::FirstParty
            } else {
                PublisherCheck::Unverified {
                    reason: "a compiled-in item must be under omuzaki/ or test/ and carry no publisher or signature"
                        .to_owned(),
                }
            };
        }
        if id.is_fixture() {
            return PublisherCheck::Refused { reason: "test/ ids are reserved for fixtures".to_owned() };
        }
        let signed = self.check_signature();
        if id.is_first_party() {
            return match (signed, omuzaki) {
                (Ok(key), Some(omuzaki)) if key == *omuzaki => PublisherCheck::FirstParty,
                (Err(why), _) => PublisherCheck::Refused {
                    reason: format!("omuzaki/ ids are valid only under the Omuzaki key: {why}"),
                },
                _ => PublisherCheck::Refused { reason: "omuzaki/ ids are valid only under the Omuzaki key".to_owned() },
            };
        }
        match (signed, &self.publisher) {
            (Ok(key), Some(publisher)) => {
                PublisherCheck::Verified { name: publisher.name.clone(), fingerprint: key.fingerprint() }
            }
            (Err(reason), _) => PublisherCheck::Unverified { reason },
            (Ok(_), None) => PublisherCheck::Unverified { reason: "no publisher".to_owned() },
        }
    }

    /// The publisher key if the signature over this manifest holds.
    fn check_signature(&self) -> Result<PublisherKey, String> {
        let publisher = self.publisher.as_ref().ok_or("it is unsigned (no publisher)")?;
        let signature = self.signature.as_deref().ok_or("it is unsigned (no signature)")?;
        let key = PublisherKey::from_hex(&publisher.key)?;
        let verifying =
            VerifyingKey::from_bytes(&key.0).map_err(|_| "the publisher key is not a valid ed25519 key".to_owned())?;
        let bytes = hex_array::<64>(signature).ok_or("the signature is not 128 hex characters")?;
        verifying
            .verify_strict(&self.signing_message(), &Signature::from_bytes(&bytes))
            .map_err(|_| "the signature does not match this content and key".to_owned())?;
        Ok(key)
    }
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(2 * bytes.len()), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn hex_array<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != 2 * N || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Signs `manifest` with a test key. Real signing arrives with LB-03 and LB-07, so this exists for tests only.
#[cfg(test)]
pub(crate) fn sign_for_test(manifest: &mut ItemManifest, seed: u8, name: &str) -> PublisherKey {
    use ed25519_dalek::{Signer, SigningKey};
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let key = PublisherKey(signing.verifying_key().to_bytes());
    manifest.publisher = Some(Publisher { name: name.to_owned(), key: key.to_hex() });
    manifest.signature = None;
    manifest.signature = Some(to_hex(&signing.sign(&manifest.signing_message()).to_bytes()));
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::fixtures::TWINTAILS;

    fn item(id: &str) -> ItemManifest {
        ItemManifest::parse(&TWINTAILS.replace("omuzaki/hair/twintails", id)).unwrap()
    }

    #[test]
    fn a_signed_manifest_verifies() {
        let mut m = item("mira/hair/twintails");
        let key = sign_for_test(&mut m, 7, "Mira");
        assert_eq!(
            m.verify(Origin::Installed),
            PublisherCheck::Verified { name: "Mira".into(), fingerprint: key.fingerprint() }
        );
        assert_eq!(key.fingerprint().len(), 16);
        assert_eq!(PublisherKey::from_hex(&key.to_hex()).unwrap(), key);
    }

    #[test]
    fn a_changed_value_a_wrong_key_or_a_bad_signature_is_unverified() {
        let mut m = item("mira/hair/twintails");
        sign_for_test(&mut m, 7, "Mira");

        let mut changed = m.clone();
        changed.summary.push('!');
        assert!(
            matches!(changed.verify(Origin::Installed), PublisherCheck::Unverified { reason } if reason.contains("does not match"))
        );

        let mut other_key = m.clone();
        other_key.publisher = Some(Publisher { name: "Mira".into(), key: sign_key_hex(8) });
        assert!(matches!(other_key.verify(Origin::Installed), PublisherCheck::Unverified { .. }));

        let mut garbage = m.clone();
        garbage.signature = Some("zz".repeat(64));
        assert!(matches!(garbage.verify(Origin::Installed), PublisherCheck::Unverified { .. }));

        let mut short_key = m.clone();
        short_key.publisher = Some(Publisher { name: "Mira".into(), key: "abcd".into() });
        assert!(matches!(short_key.verify(Origin::Installed), PublisherCheck::Unverified { .. }));
    }

    fn sign_key_hex(seed: u8) -> String {
        let mut scratch = item("mira/hair/twintails");
        sign_for_test(&mut scratch, seed, "x").to_hex()
    }

    #[test]
    fn the_name_alone_proves_nothing() {
        let mut m = item("mira/hair/twintails");
        m.publisher = Some(Publisher { name: "Omuzaki".into(), key: "00".repeat(32) });
        let check = m.verify(Origin::Installed);
        assert!(matches!(check, PublisherCheck::Unverified { .. }), "{check:?}");
        assert!(
            matches!(item("mira/x").verify(Origin::Installed), PublisherCheck::Unverified { reason } if reason.contains("unsigned"))
        );
    }

    #[test]
    fn omuzaki_ids_are_valid_only_under_the_omuzaki_key() {
        let mut m = item("omuzaki/hair/twintails");
        let other = sign_for_test(&mut m, 7, "Omuzaki");
        assert!(matches!(
            m.verify_with(Origin::Installed, Some(&PublisherKey([1; 32]))),
            PublisherCheck::Refused { .. }
        ));
        assert!(matches!(m.verify(Origin::Installed), PublisherCheck::Refused { .. }), "no Omuzaki key exists yet");
        assert_eq!(
            m.verify_with(Origin::Installed, Some(&other)),
            PublisherCheck::FirstParty,
            "signed with the Omuzaki key"
        );
        assert!(
            matches!(
                item("omuzaki/hair/twintails").verify_with(Origin::Installed, Some(&other)),
                PublisherCheck::Refused { .. }
            ),
            "unsigned"
        );
    }

    #[test]
    fn compiled_in_sources_are_first_party_without_a_publisher() {
        assert_eq!(item("omuzaki/hair/twintails").verify(Origin::CompiledIn), PublisherCheck::FirstParty);
        assert_eq!(item("test/hair/twintails").verify(Origin::CompiledIn), PublisherCheck::FirstParty);
        assert!(matches!(item("mira/hair/twintails").verify(Origin::CompiledIn), PublisherCheck::Unverified { .. }));
        let mut m = item("omuzaki/hair/twintails");
        sign_for_test(&mut m, 7, "Omuzaki");
        assert!(matches!(m.verify(Origin::CompiledIn), PublisherCheck::Unverified { .. }));
    }

    #[test]
    fn installed_items_may_not_use_the_fixture_namespace() {
        assert!(matches!(item("test/hair/twintails").verify(Origin::Installed), PublisherCheck::Refused { .. }));
    }
}
