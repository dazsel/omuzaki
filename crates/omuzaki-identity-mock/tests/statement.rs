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

    assert!(
        matches!(check_statement(std::slice::from_ref(&by_old), &trust), Check::Malformed(m) if m.contains("missing"))
    );
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

fn permutations<T: Clone>(items: &[T]) -> Vec<Vec<T>> {
    if items.len() <= 1 {
        return vec![items.to_vec()];
    }
    let mut out = Vec::new();
    for i in 0..items.len() {
        let mut rest = items.to_vec();
        let head = rest.remove(i);
        for mut tail in permutations(&rest) {
            tail.insert(0, head.clone());
            out.push(tail);
        }
    }
    out
}

#[test]
fn equal_keys_in_a_two_key_statement_are_refused_before_coverage() {
    let a = MemorySigner::from_test_seed([8; 32], "Mira");
    let trust = MemoryTrust::new().trusted(a.key_id(), "Mira");
    for body in [KeyStatement::rotate(a.key_id(), a.key_id()), KeyStatement::also(a.key_id(), a.key_id())] {
        assert_eq!(body.validate(), Err(omuzaki_identity_api::statement::StatementError::SameKey));
        let one = sign(&a, Domain::KeyStatement, &body);
        let check = check_statement(&[one.clone(), one], &trust);
        assert!(matches!(check, Check::Malformed(m) if m.contains("two different keys")), "{body:?}");
    }
    assert_eq!(KeyStatement::revoke(a.key_id(), RevokeReason::Lost).validate(), Ok(()));
}

#[test]
fn the_outcome_does_not_depend_on_the_order_or_repeats_of_the_copies() {
    let old = MemorySigner::from_test_seed([9; 32], "Mira");
    let new = MemorySigner::from_test_seed([10; 32], "Mira");
    let stray = MemorySigner::from_test_seed([11; 32], "Eve");
    let body = KeyStatement::rotate(old.key_id(), new.key_id());
    let copies = [
        sign(&old, Domain::KeyStatement, &body),
        sign(&new, Domain::KeyStatement, &body),
        sign(&stray, Domain::KeyStatement, &body),
    ];
    let trusts = [
        (MemoryTrust::new().trusted(old.key_id(), "Mira"), Trust::Trusted { name: "Mira".into() }),
        (MemoryTrust::new().trusted(new.key_id(), "Mira"), Trust::Unknown),
        (MemoryTrust::new().trusted(old.key_id(), "Mira").revoked(old.key_id()), Trust::Revoked),
    ];
    for (store, expected) in &trusts {
        for order in permutations(&copies) {
            let check = check_statement(&order, store);
            assert!(
                matches!(&check, Check::Valid { identity, trust } if identity.key == old.key_id() && trust == expected),
                "{check:?}"
            );
            let mut repeated = order.clone();
            repeated.extend(order.iter().cloned());
            assert_eq!(check_statement(&repeated, store), check);
        }
    }
}

#[test]
fn a_linked_pair_is_revoked_or_trusted_whichever_key_the_list_knows() {
    let a = MemorySigner::from_test_seed([12; 32], "Mira");
    let b = MemorySigner::from_test_seed([13; 32], "Mira");
    let body = KeyStatement::also(a.key_id(), b.key_id());
    let copies = [sign(&a, Domain::KeyStatement, &body), sign(&b, Domain::KeyStatement, &body)];
    let cases = [
        (MemoryTrust::new().trusted(a.key_id(), "A"), Trust::Trusted { name: "A".into() }),
        (MemoryTrust::new().trusted(b.key_id(), "B"), Trust::Trusted { name: "B".into() }),
        (MemoryTrust::new().trusted(a.key_id(), "A").revoked(b.key_id()), Trust::Revoked),
        (MemoryTrust::new(), Trust::Unknown),
    ];
    for (store, expected) in &cases {
        let results: Vec<_> = permutations(&copies).iter().map(|order| check_statement(order, store)).collect();
        assert!(results.windows(2).all(|pair| pair[0] == pair[1]), "{results:?}");
        assert!(matches!(&results[0], Check::Valid { trust, .. } if trust == expected), "{:?}", results[0]);
    }
}

#[test]
fn several_failures_report_the_same_one_in_every_order() {
    let a = MemorySigner::from_test_seed([14; 32], "Mira");
    let b = MemorySigner::from_test_seed([15; 32], "Mira");
    let body = KeyStatement::also(a.key_id(), b.key_id());
    let good = sign(&a, Domain::KeyStatement, &body);
    let wrong_domain = sign(&b, Domain::Export, &body);
    let mut renamed = sign(&b, Domain::KeyStatement, &body);
    renamed.signer.name = "Other".into();
    let copies = [good, wrong_domain, renamed];
    let results: Vec<_> =
        permutations(&copies).iter().map(|order| check_statement(order, &MemoryTrust::new())).collect();
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]), "{results:?}");
    assert!(matches!(results[0], Check::Tampered(_)));
}

mod shuffled {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn any_shuffle_of_any_copies_gives_one_result(order in Just((0..6usize).collect::<Vec<_>>()).prop_shuffle(), keep in 1usize..=6) {
            let signers: Vec<_> = (0..3u8).map(|n| MemorySigner::from_test_seed([40 + n; 32], "Mira")).collect();
            let body = KeyStatement::rotate(signers[0].key_id(), signers[1].key_id());
            let pool: Vec<_> = (0..6).map(|i| sign(&signers[i % 3], Domain::KeyStatement, &body)).collect();
            let trust = MemoryTrust::new().trusted(signers[0].key_id(), "Mira");
            let chosen: Vec<_> = order.iter().take(keep).map(|i| pool[*i].clone()).collect();
            let mut sorted = chosen.clone();
            sorted.sort_by_key(|c| c.signer.key);
            prop_assert_eq!(check_statement(&chosen, &trust), check_statement(&sorted, &trust));
        }
    }
}
