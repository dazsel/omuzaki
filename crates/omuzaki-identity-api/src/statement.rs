//! Key statements: rotate, link, revoke and bind, signed under [`Domain::KeyStatement`] (design D5).
//!
//! One body type for every consumer (`identity.rotate`, `identity.revoke`, `trust.import`, team rosters), so no crate
//! defines a second serialisation under the same security domain.

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::Domain;
use crate::key::{Identity, KeyId};
use crate::service::{Check, Trust, TrustStore};
use crate::signed::Signed;

/// Why a key is revoked.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum RevokeReason {
    /// The private key may have been seen by someone else.
    Compromised,
    /// The private key is gone (a dead disk, a wiped account) and a linked key revokes it.
    Lost,
}

/// A claim about keys. Wire form: an object with a `statement` tag and the variant's fields in snake case.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "statement", rename_all = "snake_case", deny_unknown_fields)]
#[non_exhaustive]
pub enum KeyStatement {
    /// `new` replaces `old`. The old key is retired, not revoked: what it signed stays valid.
    Rotate {
        /// The key being replaced.
        old: KeyId,
        /// Its successor.
        new: KeyId,
    },
    /// `a` and `b` belong to one person (a second machine); either can revoke the other.
    Also {
        /// One key.
        a: KeyId,
        /// The other key.
        b: KeyId,
    },
    /// `key` is no longer to be trusted, for every signature it ever made.
    Revoke {
        /// The revoked key.
        key: KeyId,
        /// Why.
        reason: RevokeReason,
    },
    /// A hosted identity provider vouches that `subject` at `issuer` holds `key` (reserved for VC-12).
    Bind {
        /// The identity provider.
        issuer: String,
        /// The account at that provider.
        subject: String,
        /// The key bound to it.
        key: KeyId,
    },
}

impl KeyStatement {
    /// The domain every key statement is signed under.
    pub const DOMAIN: Domain = Domain::KeyStatement;

    /// `Rotate { old, new }`.
    pub fn rotate(old: KeyId, new: KeyId) -> Self {
        Self::Rotate { old, new }
    }

    /// `Also { a, b }`.
    pub fn also(a: KeyId, b: KeyId) -> Self {
        Self::Also { a, b }
    }

    /// `Revoke { key, reason }`.
    pub fn revoke(key: KeyId, reason: RevokeReason) -> Self {
        Self::Revoke { key, reason }
    }

    /// `Bind { issuer, subject, key }`.
    pub fn bind(issuer: impl Into<String>, subject: impl Into<String>, key: KeyId) -> Self {
        Self::Bind { issuer: issuer.into(), subject: subject.into(), key }
    }

    /// The keys that must each sign this body for it to hold: both keys of `Rotate` and `Also` (so the claim is made
    /// by whoever holds both) and the bound key of `Bind`. `Revoke` names none: who may revoke a key (itself, a key
    /// linked by `Also`, a team list) is the consumer's rule, applied to the signers it finds valid.
    pub fn required_signers(&self) -> Vec<KeyId> {
        match self {
            Self::Rotate { old, new } => vec![*old, *new],
            Self::Also { a, b } => vec![*a, *b],
            Self::Revoke { .. } => Vec::new(),
            Self::Bind { key, .. } => vec![*key],
        }
    }
}

/// Why a statement body is refused before any signature is looked at.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StatementError {
    /// A two-key statement (`Rotate`, `Also`) names one key twice, so one signature would stand for both roles.
    #[error("a two-key statement needs two different keys")]
    SameKey,
}

impl KeyStatement {
    /// Refuses a body that cannot mean what it says: `Rotate` with `old == new` and `Also` with `a == b`.
    ///
    /// # Errors
    /// [`StatementError::SameKey`] for equal keys in a two-key statement.
    pub fn validate(&self) -> Result<(), StatementError> {
        match self {
            Self::Rotate { old: x, new: y } | Self::Also { a: x, b: y } if x == y => Err(StatementError::SameKey),
            _ => Ok(()),
        }
    }
}

/// How bad a failed check is, so that of several the same one wins whatever the order of the copies.
fn badness(check: &Check) -> u8 {
    match check {
        Check::Malformed(_) => 0,
        Check::Tampered(_) => 1,
        _ => 2,
    }
}

/// Checks the `Signed` copies of one key statement under [`Domain::KeyStatement`]. The order and the repeats of
/// `copies` never change the result: every copy is verified, and the result is read from the statement's roles and
/// the set of keys that signed.
///
/// Fails closed. No copies is `Unsigned`. Differing bodies, an invalid body ([`KeyStatement::validate`], checked
/// before signer coverage) and a missing signer are `Malformed`. Any copy that does not verify decides the result
/// instead: the worst failure (`Malformed`, then `Tampered`), ties broken by the failure's text.
///
/// Otherwise the result is `Valid`, read by role: `Rotate` and `Bind` report the old (or bound) key and the trust in
/// it; `Also` reports one key of the pair and that key's trust: a revoked key if either is, else a trusted one, else
/// the lower key (ties go to the lower key); `Revoke` reports the revoked key if it signed, else the lowest signing key. Where one key
/// signed under several names, the lowest name is reported.
pub fn check_statement(copies: &[Signed<KeyStatement>], trust: &dyn TrustStore) -> Check {
    let Some(first) = copies.first() else {
        return Check::Unsigned;
    };
    if copies.iter().any(|copy| copy.body != first.body) {
        return Check::Malformed("the signed copies carry different statements".to_owned());
    }
    if let Err(e) = first.body.validate() {
        return Check::Malformed(e.to_string());
    }
    let mut failure: Option<Check> = None;
    let mut signers: BTreeMap<KeyId, Identity> = BTreeMap::new();
    for copy in copies {
        match copy.check(KeyStatement::DOMAIN, trust) {
            Check::Valid { identity, .. } => match signers.entry(identity.key) {
                Entry::Vacant(slot) => {
                    slot.insert(identity);
                }
                Entry::Occupied(mut slot) => {
                    if identity.name < slot.get().name {
                        slot.insert(identity);
                    }
                }
            },
            other => {
                let worse = failure
                    .as_ref()
                    .is_none_or(|kept| (badness(&other), format!("{other:?}")) < (badness(kept), format!("{kept:?}")));
                if worse {
                    failure = Some(other);
                }
            }
        }
    }
    if let Some(failure) = failure {
        return failure;
    }
    for key in first.body.required_signers() {
        if !signers.contains_key(&key) {
            return Check::Malformed(format!("missing the signature of key {}", key.fingerprint()));
        }
    }
    let (reported, standing) = match &first.body {
        KeyStatement::Rotate { old, .. } => (*old, trust.trust(old)),
        KeyStatement::Bind { key, .. } => (*key, trust.trust(key)),
        KeyStatement::Also { a, b } => linked_pair(*a.min(b), *a.max(b), trust),
        KeyStatement::Revoke { key, .. } => {
            let by = if signers.contains_key(key) { Some(*key) } else { signers.keys().next().copied() };
            let Some(by) = by else { return Check::Malformed("a revocation has no valid signature".to_owned()) };
            (by, trust.trust(&by))
        }
    };
    match signers.remove(&reported) {
        Some(identity) => Check::Valid { identity, trust: standing },
        None => Check::Malformed("the reported key did not sign".to_owned()),
    }
}

/// The key to report for a linked pair, with that same key's trust, so `Valid.trust` always describes
/// `Valid.identity.key`. A revoked key wins (its signatures are not to be relied on), then a trusted one, then the
/// lower key; between two keys of the same standing the lower key wins.
fn linked_pair(lower: KeyId, higher: KeyId, trust: &dyn TrustStore) -> (KeyId, Trust) {
    let (low, high) = (trust.trust(&lower), trust.trust(&higher));
    let rank = |t: &Trust| match t {
        Trust::Revoked => 0,
        Trust::Trusted { .. } => 1,
        _ => 2,
    };
    if rank(&high) < rank(&low) { (higher, high) } else { (lower, low) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(n: u8) -> KeyId {
        KeyId([n; 32])
    }

    #[test]
    fn the_wire_form_is_tagged_snake_case_and_round_trips() {
        let all = [
            KeyStatement::rotate(key(1), key(2)),
            KeyStatement::also(key(1), key(2)),
            KeyStatement::revoke(key(1), RevokeReason::Lost),
            KeyStatement::bind("idp", "acct", key(3)),
        ];
        let names = ["rotate", "also", "revoke", "bind"];
        for (statement, name) in all.iter().zip(names) {
            let value = serde_json::to_value(statement).unwrap();
            assert_eq!(value["statement"], name);
            assert_eq!(&serde_json::from_value::<KeyStatement>(value).unwrap(), statement);
        }
    }

    #[test]
    fn unknown_fields_and_unknown_statements_are_refused() {
        let extra = serde_json::json!({"statement": "also", "a": key(1), "b": key(2), "c": 1});
        assert!(serde_json::from_value::<KeyStatement>(extra).is_err());
        let unknown = serde_json::json!({"statement": "forget", "a": key(1)});
        assert!(serde_json::from_value::<KeyStatement>(unknown).is_err());
    }

    #[test]
    fn required_signers_name_both_keys_of_a_two_key_statement() {
        assert_eq!(KeyStatement::rotate(key(1), key(2)).required_signers(), vec![key(1), key(2)]);
        assert_eq!(KeyStatement::also(key(3), key(4)).required_signers(), vec![key(3), key(4)]);
        assert_eq!(KeyStatement::revoke(key(1), RevokeReason::Compromised).required_signers(), Vec::<KeyId>::new());
        assert_eq!(KeyStatement::bind("i", "s", key(5)).required_signers(), vec![key(5)]);
    }
}
