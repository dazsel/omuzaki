//! The label index: the labels Omuzaki stored, kept in one keychain entry.

use std::collections::BTreeSet;

use omuzaki_secrets_api::{SecretError, SecretLabel};

/// The user name of the index entry. A label never contains `@`, so no label can be this entry.
pub(crate) const INDEX_USER: &str = "@labels";

/// What the index is called in an error.
pub(crate) const INDEX_WHAT: &str = "Omuzaki's label list";

/// The labels, one per line. Labels cannot contain a newline.
pub(crate) fn encode(labels: &BTreeSet<SecretLabel>) -> String {
    labels.iter().map(SecretLabel::as_str).collect::<Vec<_>>().join("\n")
}

/// Reads [`encode`]'s output. An unreadable line is [`SecretError::Corrupt`], not skipped.
pub(crate) fn decode(text: &str) -> Result<BTreeSet<SecretLabel>, SecretError> {
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| SecretLabel::new(line).map_err(|_| SecretError::corrupt(INDEX_WHAT)))
        .collect()
}
