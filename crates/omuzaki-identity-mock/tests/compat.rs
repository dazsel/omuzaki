//! Byte compatibility with `omuzaki-library-api` (design D3): its tag, its signing message and its verifier.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use omuzaki_identity_api::domain::{Domain, signing_message};
use omuzaki_identity_api::key::Signature;
use omuzaki_identity_api::service::Signer;
use omuzaki_identity_api::verify::verify;
use omuzaki_identity_mock::MemorySigner;
use omuzaki_library_api::{ItemManifest, Origin, Publisher, PublisherCheck};

const ITEM: &str = r#"
schema = 1
id = "mira/hair/twintails"
version = "1.0.0"
section = "hair"
kind = "part"
target = "character"
slot = "hairstyle"
title = "Twintails"
summary = "Two high tails tied above the ears."
tags = ["hair"]
origin = "test"
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }

[rights]
author = "Mira"
licence = "CC0-1.0"
commercial = "ok"
owned = true

[[step]]
op = "part.add"
[step.args]
character = "$character"
kind = "hair"
"#;

fn manifest() -> ItemManifest {
    ItemManifest::parse(ITEM).unwrap()
}

#[test]
fn the_library_item_message_matches_the_library_byte_for_byte() {
    let m = manifest();
    assert_eq!(signing_message(Domain::LibraryItem, &m.hash()), m.signing_message());
    assert_eq!(Domain::LibraryItem.tag(), b"omuzaki-library-item-v1\0");
}

#[test]
fn signed_with_the_mock_it_verifies_with_the_library() {
    let signer = MemorySigner::from_test_seed([7; 32], "Mira");
    let mut m = manifest();
    let sig = signer.sign(Domain::LibraryItem, &m.hash()).unwrap();
    m.publisher = Some(Publisher { name: "Mira".into(), key: signer.key_id().to_hex() });
    m.signature = Some(sig.to_hex());
    assert!(
        matches!(m.verify(Origin::Installed), PublisherCheck::Verified { ref name, .. } if name == "Mira"),
        "{:?}",
        m.verify(Origin::Installed)
    );
}

#[test]
fn signed_by_the_library_it_verifies_here() {
    use ed25519_dalek::{Signer as _, SigningKey};
    let signing = SigningKey::from_bytes(&[9; 32]);
    let m = manifest();
    let sig = Signature(signing.sign(&m.signing_message()).to_bytes());
    let key = omuzaki_identity_api::key::KeyId(signing.verifying_key().to_bytes());
    assert_eq!(verify(Domain::LibraryItem, &m.hash(), &key, &sig), Ok(()));
    assert!(verify(Domain::Export, &m.hash(), &key, &sig).is_err());
}
