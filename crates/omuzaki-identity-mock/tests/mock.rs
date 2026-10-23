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
    let nan = Signed { body: Some(f64::NAN), signer: ok.signer.clone(), signature: ok.signature };
    assert!(nan.hash().is_err());
    assert!(matches!(nan.check(Domain::Export, &trust), Check::Malformed(m) if m.contains("not a JSON number")));
    assert!(hash_of(&Some(f64::INFINITY), &ok.signer).is_err(), "a signer is refused before anything is signed");
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

/// RFC 8032 section 7.1, TEST 1024: its 1023-byte message, copied from the RFC text
/// (<https://www.rfc-editor.org/rfc/rfc8032.txt>).
const TEST_1024_MESSAGE: &str = concat!(
    "08b8b2b733424243760fe426a4b54908632110a66c2f6591eabd3345e3e4eb98fa6e264bf09efe12ee50f8f54e9f77b1e355f6c50544e23f",
    "b1433ddf73be84d879de7c0046dc4996d9e773f4bc9efe5738829adb26c81b37c93a1b270b20329d658675fc6ea534e0810a4432826bf58c",
    "941efb65d57a338bbd2e26640f89ffbc1a858efcb8550ee3a5e1998bd177e93a7363c344fe6b199ee5d02e82d522c4feba15452f80288a82",
    "1a579116ec6dad2b3b310da903401aa62100ab5d1a36553e06203b33890cc9b832f79ef80560ccb9a39ce767967ed628c6ad573cb116dbef",
    "efd75499da96bd68a8a97b928a8bbc103b6621fcde2beca1231d206be6cd9ec7aff6f6c94fcd7204ed3455c68c83f4a41da4af2b74ef5c53",
    "f1d8ac70bdcb7ed185ce81bd84359d44254d95629e9855a94a7c1958d1f8ada5d0532ed8a5aa3fb2d17ba70eb6248e594e1a2297acbbb39d",
    "502f1a8c6eb6f1ce22b3de1a1f40cc24554119a831a9aad6079cad88425de6bde1a9187ebb6092cf67bf2b13fd65f27088d78b7e883c8759",
    "d2c4f5c65adb7553878ad575f9fad878e80a0c9ba63bcbcc2732e69485bbc9c90bfbd62481d9089beccf80cfe2df16a2cf65bd92dd597b07",
    "07e0917af48bbb75fed413d238f5555a7a569d80c3414a8d0859dc65a46128bab27af87a71314f318c782b23ebfe808b82b0ce26401d2e22",
    "f04d83d1255dc51addd3b75a2b1ae0784504df543af8969be3ea7082ff7fc9888c144da2af58429ec96031dbcad3dad9af0dcbaaaf268cb8",
    "fcffead94f3c7ca495e056a9b47acdb751fb73e666c6c655ade8297297d07ad1ba5e43f1bca32301651339e22904cc8c42f58c30c04aafdb",
    "038dda0847dd988dcda6f3bfd15c4b4c4525004aa06eeff8ca61783aacec57fb3d1f92b0fe2fd1a85f6724517b65e614ad6808d6f6ee34df",
    "f7310fdc82aebfd904b01e1dc54b2927094b2db68d6f903b68401adebf5a7e08d78ff4ef5d63653a65040cf9bfd4aca7984a74d371459867",
    "80fc0b16ac451649de6188a7dbdf191f64b5fc5e2ab47b57f7f7276cd419c17a3ca8e1b939ae49e488acba6b965610b5480109c8b17b80e1",
    "b7b750dfc7598d5d5011fd2dcc5600a32ef5b52a1ecc820e308aa342721aac0943bf6686b64b2579376504ccc493d97e6aed3fb0f9cd71a4",
    "3dd497f01f17c0e2cb3797aa2a2f256656168e6c496afc5fb93246f6b1116398a346f1a641f3b041e989f7914f90cc2c7fff357876e506b5",
    "0d334ba77c225bc307ba537152f3f1610e4eafe595f6d9d90d11faa933a15ef1369546868a7f3a45a96768d40fd9d03412c091c6315cf4fd",
    "e7cb68606937380db2eaaa707b4c4185c32eddcdd306705e4dc1ffc872eeee475a64dfac86aba41c0618983f8741c5ef68d3a101e8a3b8ca",
    "c60c905c15fc910840b94c00a0b9d0",
);

#[test]
fn rfc_8032_signatures_match_for_the_vectors() {
    // RFC 8032 section 7.1, all five Ed25519 vectors (secret key, message, signature, public key), copied from the RFC
    // text (https://www.rfc-editor.org/rfc/rfc8032.txt): signing is deterministic, so a match proves the dalek build
    // agrees with the RFC. Each public key is derived from its secret key, and each vector is then tampered with.
    use ed25519_dalek::{Signer as _, SigningKey, Verifier as _};
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
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
        ),
        (
            "f5e5767cf153319517630f226876b86c8160cc583bc013744c6bf255f5cc0ee5",
            TEST_1024_MESSAGE,
            "0aab4c900501b3e24d7cdf4663326a3a87df5e4843b2cbdb67cbf6e460fec350aa5371b1508f9f4528ecea23c436d94b5e8fcd4f681e30a6ac00a9704a188a03",
            "278117fc144c72340f67d0f2316e8386ceffbf2b2428c9c51fef7c597f1d426e",
        ),
        (
            "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b58909351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
            "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
        ),
    ];
    let bytes = |text: &str| -> Vec<u8> {
        (0..text.len() / 2).map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap()).collect()
    };
    assert_eq!(bytes(cases[3].1).len(), 1023, "TEST 1024's message is 1023 bytes");
    {
        use sha2::Digest as _;
        assert_eq!(hex(&sha2::Sha512::digest(b"abc")), cases[4].1, "TEST SHA(abc)'s message is SHA-512 of abc");
    }
    for (seed, msg, sig, pk) in cases {
        let signing = SigningKey::from_bytes(&KeyId::from_hex(seed).unwrap().0);
        let msg = bytes(msg);
        let signature = signing.sign(&msg);
        assert_eq!(hex(&signature.to_bytes()), sig);
        let key = signing.verifying_key();
        assert_eq!(hex(&key.to_bytes()), pk);
        assert!(key.verify(&msg, &signature).is_ok());

        // A flipped message bit (or one byte more, for the empty message), a flipped signature bit, the wrong key.
        let mut other = msg.clone();
        match other.first_mut() {
            Some(byte) => *byte ^= 1,
            None => other.push(0),
        }
        assert!(key.verify(&other, &signature).is_err(), "{pk}: a changed message verified");
        let mut flipped = signature.to_bytes();
        flipped[17] ^= 0x10;
        assert!(
            key.verify(&msg, &ed25519_dalek::Signature::from_bytes(&flipped)).is_err(),
            "{pk}: a flipped signature"
        );
        let stranger = SigningKey::from_bytes(&[7; 32]).verifying_key();
        assert!(stranger.verify(&msg, &signature).is_err(), "{pk}: another key verified it");
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
