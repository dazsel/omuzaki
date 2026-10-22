//! `Domain::Plugin` (ADR 0042 amendment, ADR 0058): its tag, a checked-in signature vector, and that a signature made
//! under it never verifies under another domain, or the reverse.

use ed25519_dalek::{Signer as _, SigningKey};
use omuzaki_identity_api::domain::{Domain, signing_message};
use omuzaki_identity_api::key::{KeyId, Signature};
use omuzaki_identity_api::verify::{VerifyError, verify};
use omuzaki_store_api::ContentHash;

/// A fixed test key seed; never a real key.
const SEED: [u8; 32] = [0x11; 32];
/// A fixed content hash, standing in for a plugin package's.
const HASH: ContentHash = ContentHash([0x22; 32]);
/// The ed25519 signature of `omuzaki-plugin-v1\0` followed by `HASH`, under the key from `SEED`.
const PLUGIN_VECTOR: &str = "365edc19c62383b8dc91b5bcd78f4ef1bef368742fde0d6c47342c0024eb4443d91658ffbd6e57632084e482a618f1c492a409ef4f20120d15194913c52f2f0a";

fn sign(domain: Domain) -> (KeyId, Signature) {
    let key = SigningKey::from_bytes(&SEED);
    let sig = key.sign(&signing_message(domain, &HASH));
    (KeyId(key.verifying_key().to_bytes()), Signature(sig.to_bytes()))
}

#[test]
fn the_plugin_tag_starts_the_signed_bytes() {
    assert_eq!(Domain::Plugin.tag(), b"omuzaki-plugin-v1\0");
    let message = signing_message(Domain::Plugin, &HASH);
    assert!(message.starts_with(b"omuzaki-plugin-v1\0"));
    assert_eq!(&message[Domain::Plugin.tag().len()..], &HASH.0);
    assert!(Domain::ALL.contains(&Domain::Plugin));
    assert_eq!(serde_json::to_string(&Domain::Plugin).unwrap(), "\"plugin\"");
}

#[test]
fn the_plugin_signature_matches_the_checked_in_vector() {
    let (key, sig) = sign(Domain::Plugin);
    assert_eq!(sig.to_hex(), PLUGIN_VECTOR);
    assert_eq!(verify(Domain::Plugin, &HASH, &key, &Signature::from_hex(PLUGIN_VECTOR).unwrap()), Ok(()));
}

#[test]
fn a_signature_never_crosses_into_or_out_of_the_plugin_domain() {
    let (key, plugin_sig) = sign(Domain::Plugin);
    for other in Domain::ALL.into_iter().filter(|d| *d != Domain::Plugin) {
        assert_eq!(verify(other, &HASH, &key, &plugin_sig), Err(VerifyError::Mismatch), "plugin as {other:?}");
        let (_, other_sig) = sign(other);
        assert_eq!(verify(Domain::Plugin, &HASH, &key, &other_sig), Err(VerifyError::Mismatch), "{other:?} as plugin");
    }
}

/// Walks every variant, so a domain added later is checked too.
#[test]
fn no_tag_is_a_prefix_of_another_and_each_has_exactly_one_nul_at_the_end() {
    for a in Domain::ALL {
        let tag = a.tag();
        assert_eq!(tag.last(), Some(&0), "{a:?}");
        assert!(!tag[..tag.len() - 1].contains(&0), "{a:?} has a NUL before its end");
        for b in Domain::ALL.into_iter().filter(|b| *b != a) {
            assert!(!b.tag().starts_with(tag), "{a:?} is a prefix of {b:?}");
        }
    }
}
