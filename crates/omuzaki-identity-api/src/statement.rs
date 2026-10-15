//! Key statements: rotate, link, revoke and bind, signed under [`Domain::KeyStatement`] (design D5).
//!
//! One body type for every consumer (`identity.rotate`, `identity.revoke`, `trust.import`, team rosters), so no crate
//! defines a second serialisation under the same security domain.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::domain::Domain;
use crate::key::KeyId;
use crate::service::{Check, TrustStore};
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

/// Checks the `Signed` copies of one key statement under [`Domain::KeyStatement`]: every copy must verify, carry the
/// same body, and together they must cover every key in [`KeyStatement::required_signers`]. Fails closed: the first
/// copy that is not valid decides the outcome, differing bodies and a missing signer are `Malformed`, and no copies
/// at all is `Unsigned`. On success the result is the first copy's `Valid`.
pub fn check_statement(copies: &[Signed<KeyStatement>], trust: &dyn TrustStore) -> Check {
    let Some(first) = copies.first() else {
        return Check::Unsigned;
    };
    let mut result = None;
    for copy in copies {
        if copy.body != first.body {
            return Check::Malformed("the signed copies carry different statements".to_owned());
        }
        match copy.check(KeyStatement::DOMAIN, trust) {
            valid @ Check::Valid { .. } => {
                result.get_or_insert(valid);
            }
            other => return other,
        }
    }
    for key in first.body.required_signers() {
        if !copies.iter().any(|copy| copy.signer.key == key) {
            return Check::Malformed(format!("missing the signature of key {}", key.fingerprint()));
        }
    }
    result.unwrap_or(Check::Unsigned)
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
            let value = serde_json::to_value(statement).unwrap_or_default();
            assert_eq!(value["statement"], name);
            assert_eq!(serde_json::from_value::<KeyStatement>(value).ok().as_ref(), Some(statement));
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
        assert!(KeyStatement::revoke(key(1), RevokeReason::Compromised).required_signers().is_empty());
        assert_eq!(KeyStatement::bind("i", "s", key(5)).required_signers(), vec![key(5)]);
    }
}
