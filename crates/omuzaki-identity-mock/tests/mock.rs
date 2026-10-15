//! The mock itself, and the contract it lets us test: envelope, domains, trust.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use omuzaki_identity_api::domain::Domain;
use omuzaki_identity_api::key::{Identity, KeyId, Signature};
use omuzaki_identity_api::service::{Check, Mismatch, Signer, Trust, TrustStore};
use omuzaki_identity_api::signed::{Signed, hash_of};
use omuzaki_identity_api::verify::verify;
use omuzaki_identity_mock::{MemorySigner, MemoryTrust};
use omuzaki_store_api::ContentHash;
use proptest::prelude::*;
use serde_json::json;

fn signed(signer: &MemorySigner, domain: Domain, body: serde_json::Value) -> Signed<serde_json::Value> {
    let identity = signer.identity().unwrap();
    let hash = hash_of(&body, &identity).unwrap();
    let signature = signer.sign(domain, &hash).unwrap();
    Signed { body, signer: identity, signature }
}

#[test]
fn debug_prints_no_key_bytes() {
    let signer = MemorySigner::from_test_seed([0xAB; 32], "Mira");
    let text = format!("{signer:?}");
    assert!(text.contains(&signer.key_id().fingerprint()));
    assert!(!text.contains("abab") && !text.contains(&signer.key_id().to_hex()), "{text}");
}

#[test]
fn signing_is_deterministic() {
    let signer = MemorySigner::from_test_seed([1; 32], "Mira");
    let hash = ContentHash::of(b"x");
    assert_eq!(signer.sign(Domain::Export, &hash), signer.sign(Domain::Export, &hash));
}

#[test]
fn a_signature_under_one_domain_fails_under_every_other() {
    let signer = MemorySigner::from_test_seed([2; 32], "Mira");
    let hash = ContentHash::of(b"x");
    let key = signer.key_id();
    for a in Domain::ALL {
        let sig = signer.sign(a, &hash).unwrap();
        for b in Domain::ALL {
            assert_eq!(verify(b, &hash, &key, &sig).is_ok(), a == b, "{a:?} as {b:?}");
        }
    }
}

#[test]
fn the_check_covers_valid_trust_tampered_and_malformed() {
    let mira = MemorySigner::from_test_seed([3; 32], "Mira");
    let stranger = MemorySigner::from_test_seed([4; 32], "Eve");
    let trust = MemoryTrust::new().trusted(mira.key_id(), "Mira (mine)").revoked(stranger.key_id());

    let ok = signed(&mira, Domain::Export, json!({"a": 1}));
    assert_eq!(
        ok.check(Domain::Export, &trust),
        Check::Valid { identity: ok.signer.clone(), trust: Trust::Trusted { name: "Mira (mine)".into() } }
    );
    let revoked = signed(&stranger, Domain::Export, json!({"a": 1}));
    assert!(matches!(revoked.check(Domain::Export, &trust), Check::Valid { trust: Trust::Revoked, .. }));
    assert!(matches!(ok.check(Domain::Export, &MemoryTrust::new()), Check::Valid { trust: Trust::Unknown, .. }));

    assert_eq!(ok.check(Domain::Approval, &trust), Check::Tampered(Mismatch::Signature));
    let mut edited = ok.clone();
    edited.body = json!({"a": 2});
    assert_eq!(edited.check(Domain::Export, &trust), Check::Tampered(Mismatch::Signature));
    let mut renamed = ok.clone();
    renamed.signer = Identity { name: "Omuzaki".into(), key: ok.signer.key };
    assert_eq!(renamed.check(Domain::Export, &trust), Check::Tampered(Mismatch::Signature), "the name is bound");

    let mut bad_key = ok.clone();
    bad_key.signer.key = KeyId([2; 32]);
    assert!(matches!(bad_key.check(Domain::Export, &trust), Check::Malformed(_) | Check::Tampered(_)));

    let unserialisable = Signed {
        body: std::collections::BTreeMap::from([((1, 2), 3)]),
        signer: ok.signer.clone(),
        signature: ok.signature,
    };
    assert!(unserialisable.hash().is_err());
    assert!(matches!(unserialisable.check(Domain::Export, &trust), Check::Malformed(_)));
}

#[test]
fn a_trust_list_revoke_wins_and_unknown_is_the_default() {
    let k = KeyId([5; 32]);
    assert_eq!(MemoryTrust::new().trust(&k), Trust::Unknown);
    assert_eq!(MemoryTrust::new().revoked(k).trusted(k, "x").trust(&k), Trust::Revoked);
}

#[test]
fn hex_parsing_rejects_bad_lengths_and_characters() {
    let good = "ab".repeat(32);
    assert!(KeyId::from_hex(&good).is_ok());
    assert!(KeyId::from_hex(&good[..63]).is_err());
    assert!(KeyId::from_hex(&format!("{good}a")).is_err());
    assert!(KeyId::from_hex(&"zz".repeat(32)).is_err());
    assert!(KeyId::from_hex(&"é".repeat(32)).is_err(), "64 bytes of non-ASCII");
    assert!(Signature::from_hex(&"ab".repeat(63)).is_err());
    assert_eq!(KeyId::from_hex(&good.to_uppercase()).unwrap().to_hex(), good);
}

#[test]
fn serde_and_json_schema_round_trip() {
    let signer = MemorySigner::from_test_seed([6; 32], "Mira");
    let s = signed(&signer, Domain::Export, json!({"a": 1}));
    let text = serde_json::to_string(&s).unwrap();
    assert!(text.contains(&s.signer.key.to_hex()) && text.contains(&s.signature.to_hex()));
    assert_eq!(serde_json::from_str::<Signed<serde_json::Value>>(&text).unwrap(), s);
    assert!(serde_json::from_str::<KeyId>("\"abc\"").is_err());
    assert!(serde_json::from_str::<Identity>(&format!(r#"{{"name":"a","key":"{}","x":1}}"#, "00".repeat(32))).is_err());
    for schema in [schemars::schema_for!(KeyId), schemars::schema_for!(Signature)] {
        assert_eq!(serde_json::to_value(schema).unwrap()["type"], "string");
    }
    assert!(serde_json::to_string(&schemars::schema_for!(Identity)).unwrap().contains("\"key\""));
}

#[test]
fn rfc_8032_signatures_match_for_the_vectors() {
    // RFC 8032 section 7.1, tests 1 to 3 (seed, message, signature): signing is deterministic, so a match proves
    // the dalek build agrees with the RFC.
    use ed25519_dalek::{Signer as _, SigningKey};
    let cases = [
        (
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
        ),
        (
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
        ),
        (
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
            "",
        ),
    ];
    for (seed, msg, sig, pk) in cases {
        let signing = SigningKey::from_bytes(&KeyId::from_hex(seed).unwrap().0);
        let msg: Vec<u8> =
            (0..msg.len() / 2).map(|i| u8::from_str_radix(&msg[2 * i..2 * i + 2], 16).unwrap()).collect();
        assert_eq!(hex(&signing.sign(&msg).to_bytes()), sig);
        if !pk.is_empty() {
            assert_eq!(hex(&signing.verifying_key().to_bytes()), pk);
        }
    }
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

proptest! {
    #[test]
    fn sign_then_verify_holds_and_any_one_flipped_bit_fails(
        seed in any::<[u8; 32]>(), data in any::<Vec<u8>>(), bit in 0usize..(32 * 8), sbit in 0usize..(64 * 8),
        kbit in 0usize..(32 * 8), domain in 0usize..Domain::ALL.len(),
    ) {
        let signer = MemorySigner::from_test_seed(seed, "t");
        let domain = Domain::ALL[domain];
        let hash = ContentHash::of(&data);
        let key = signer.key_id();
        let sig = signer.sign(domain, &hash).unwrap();
        prop_assert!(verify(domain, &hash, &key, &sig).is_ok());

        let mut h = hash;
        h.0[bit / 8] ^= 1 << (bit % 8);
        prop_assert!(verify(domain, &h, &key, &sig).is_err());
        let mut s = sig;
        s.0[sbit / 8] ^= 1 << (sbit % 8);
        prop_assert!(verify(domain, &hash, &key, &s).is_err());
        let mut k = key;
        k.0[kbit / 8] ^= 1 << (kbit % 8);
        prop_assert!(verify(domain, &hash, &k, &sig).is_err());
    }
}
