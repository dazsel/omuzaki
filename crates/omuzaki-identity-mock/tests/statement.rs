//! Key statements and the serialisable outcomes, through the mock signer.

#![allow(clippy::unwrap_used)] // test helpers outside #[test] fns

use omuzaki_identity_api::domain::Domain;
use omuzaki_identity_api::key::KeyError;
use omuzaki_identity_api::service::Signer;
use omuzaki_identity_api::service::{Check, Mismatch, SignError, Trust};
use omuzaki_identity_api::signed::{Signed, hash_of};
use omuzaki_identity_api::statement::{KeyStatement, RevokeReason, check_statement};
use omuzaki_identity_api::verify::VerifyError;
use omuzaki_identity_mock::{MemorySigner, MemoryTrust};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::json;

fn sign(signer: &MemorySigner, domain: Domain, body: &KeyStatement) -> Signed<KeyStatement> {
    let identity = signer.identity().unwrap();
    let signature = signer.sign(domain, &hash_of(body, &identity).unwrap()).unwrap();
    Signed { body: body.clone(), signer: identity, signature }
}

fn round_trip<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug>(value: &T) -> serde_json::Value {
    let json = serde_json::to_value(value).unwrap();
    assert_eq!(&serde_json::from_value::<T>(json.clone()).unwrap(), value);
    json
}

#[test]
fn a_two_key_statement_needs_both_keys_to_sign_the_same_body() {
    let (old, new) = (MemorySigner::from_test_seed([1; 32], "Mira"), MemorySigner::from_test_seed([2; 32], "Mira"));
    let trust = MemoryTrust::new().trusted(old.key_id(), "Mira");
    let body = KeyStatement::rotate(old.key_id(), new.key_id());
    let (by_old, by_new) = (sign(&old, Domain::KeyStatement, &body), sign(&new, Domain::KeyStatement, &body));

    let held = check_statement(&[by_old.clone(), by_new.clone()], &trust);
    assert!(matches!(held, Check::Valid { trust: Trust::Trusted { .. }, .. }), "{held:?}");

    assert!(matches!(check_statement(&[by_old.clone()], &trust), Check::Malformed(m) if m.contains("missing")));
    assert!(matches!(check_statement(&[by_old.clone(), by_old.clone()], &trust), Check::Malformed(_)));
    let other = sign(&new, Domain::KeyStatement, &KeyStatement::rotate(new.key_id(), old.key_id()));
    assert!(matches!(check_statement(&[by_old, other], &trust), Check::Malformed(m) if m.contains("different")));
    assert_eq!(check_statement(&[], &trust), Check::Unsigned);
}

#[test]
fn a_statement_signed_under_another_domain_does_not_hold() {
    let a = MemorySigner::from_test_seed([3; 32], "Mira");
    let b = MemorySigner::from_test_seed([4; 32], "Mira");
    let body = KeyStatement::also(a.key_id(), b.key_id());
    for domain in Domain::ALL.into_iter().filter(|d| *d != Domain::KeyStatement) {
        let copies = [sign(&a, domain, &body), sign(&b, Domain::KeyStatement, &body)];
        assert_eq!(check_statement(&copies, &MemoryTrust::new()), Check::Tampered(Mismatch::Signature), "{domain:?}");
    }
}

#[test]
fn a_revoke_statement_holds_with_one_signature_and_reports_the_signer() {
    let lost = MemorySigner::from_test_seed([5; 32], "Mira");
    let linked = MemorySigner::from_test_seed([6; 32], "Mira");
    let body = KeyStatement::revoke(lost.key_id(), RevokeReason::Lost);
    let check = check_statement(&[sign(&linked, Domain::KeyStatement, &body)], &MemoryTrust::new());
    assert!(matches!(check, Check::Valid { identity, .. } if identity.key == linked.key_id()));
}

#[test]
fn outcomes_and_statements_have_stable_serde_names() {
    let signer = MemorySigner::from_test_seed([7; 32], "Mira");
    let identity = signer.identity().unwrap();
    let valid = Check::Valid { identity: identity.clone(), trust: Trust::Trusted { name: "Mira".into() } };
    assert_eq!(
        round_trip(&valid),
        json!({"valid": {"identity": {"name": "Mira", "key": identity.key}, "trust": {"trusted": {"name": "Mira"}}}})
    );
    assert_eq!(round_trip(&Check::Unsigned), json!("unsigned"));
    assert_eq!(round_trip(&Check::Tampered(Mismatch::Content)), json!({"tampered": "content"}));
    assert_eq!(round_trip(&Check::Malformed("why".into())), json!({"malformed": "why"}));
    assert_eq!(round_trip(&Trust::Revoked), json!("revoked"));
    assert_eq!(round_trip(&Trust::Unknown), json!("unknown"));
    assert_eq!(round_trip(&Domain::KeyStatement), json!("key_statement"));
    assert_eq!(round_trip(&SignError::Unavailable("no keychain".into())), json!({"unavailable": "no keychain"}));
    assert_eq!(round_trip(&VerifyError::BadKey), json!("bad_key"));
    assert_eq!(round_trip(&KeyError::NotHex), json!("not_hex"));
    let statement = KeyStatement::revoke(identity.key, RevokeReason::Compromised);
    assert_eq!(round_trip(&statement), json!({"statement": "revoke", "key": identity.key, "reason": "compromised"}));
}

#[test]
fn every_domain_round_trips_under_a_distinct_name() {
    let names: Vec<_> = Domain::ALL.iter().map(round_trip).collect();
    for (i, a) in names.iter().enumerate() {
        assert!(names[i + 1..].iter().all(|b| a != b), "{a}");
    }
}

#[test]
fn the_schemas_exist_for_every_outcome() {
    for schema in [
        serde_json::to_value(schemars::schema_for!(Check)),
        serde_json::to_value(schemars::schema_for!(Trust)),
        serde_json::to_value(schemars::schema_for!(Mismatch)),
        serde_json::to_value(schemars::schema_for!(KeyStatement)),
        serde_json::to_value(schemars::schema_for!(Domain)),
    ] {
        assert!(schema.unwrap().is_object());
    }
}
