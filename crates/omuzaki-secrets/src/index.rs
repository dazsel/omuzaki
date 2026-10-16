//! The label index: the labels Omuzaki stored, kept in a few keychain entries (shards).
//!
//! One entry per label cannot be listed portably, so the labels are also written to *shards*: entries `@labels.0`,
//! `@labels.1`, and so on, each holding labels one per line in sorted order. A shard stays under [`SHARD_BUDGET`]
//! bytes because Windows Credential Manager caps one generic credential at 2,560 bytes; a long list becomes more
//! shards, never a failed write. The shards are contiguous from 0, and an empty index has none.

use std::collections::BTreeSet;

use omuzaki_secrets_api::{SecretError, SecretLabel};

/// A user name starting with this is an index shard. A label never contains `@`, so no label can be one.
pub(crate) const INDEX_PREFIX: &str = "@labels.";

/// What the index is called in an error.
pub(crate) const INDEX_WHAT: &str = "Omuzaki's label list";

/// The most bytes of text in one shard. Well under the smallest keychain limit (Windows: 2,560 bytes), and room for
/// the longest label ([`SecretLabel::MAX_LEN`]) in any shard.
pub(crate) const SHARD_BUDGET: usize = 2_000;

/// The user name of shard `n`.
pub(crate) fn shard_user(n: usize) -> String {
    format!("{INDEX_PREFIX}{n}")
}

/// Whether `user` names an index shard.
pub(crate) fn is_shard_user(user: &str) -> bool {
    user.starts_with(INDEX_PREFIX)
}

/// The labels as shards, in order: each one label per line, no shard longer than [`SHARD_BUDGET`]. No labels, no
/// shards.
pub(crate) fn encode(labels: &BTreeSet<SecretLabel>) -> Vec<String> {
    let mut shards: Vec<String> = Vec::new();
    for label in labels {
        let text = label.as_str();
        match shards.last_mut() {
            Some(shard) if shard.len() + 1 + text.len() <= SHARD_BUDGET => {
                shard.push('\n');
                shard.push_str(text);
            }
            _ => shards.push(text.to_owned()),
        }
    }
    shards
}

/// Reads [`encode`]'s output. A shard that is empty, or has an empty or invalid line (a blank record, a trailing
/// newline, a `\r`), is [`SecretError::Corrupt`]: the encoder never writes one, so it is not skipped.
pub(crate) fn decode<'a>(shards: impl IntoIterator<Item = &'a str>) -> Result<BTreeSet<SecretLabel>, SecretError> {
    let mut labels = BTreeSet::new();
    for shard in shards {
        if shard.is_empty() {
            return Err(SecretError::corrupt(INDEX_WHAT));
        }
        for line in shard.split('\n') {
            labels.insert(SecretLabel::new(line).map_err(|_| SecretError::corrupt(INDEX_WHAT))?);
        }
    }
    Ok(labels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn labels(texts: &[&str]) -> BTreeSet<SecretLabel> {
        texts.iter().map(|t| SecretLabel::new(*t).unwrap()).collect()
    }

    fn long_label(i: usize) -> SecretLabel {
        // The longest label there is: `a/` + digits, padded to MAX_LEN.
        let head = format!("a/{i:04}-");
        SecretLabel::new(format!("{head}{}", "x".repeat(SecretLabel::MAX_LEN - head.len()))).unwrap()
    }

    #[test]
    fn no_labels_are_no_shards() {
        assert_eq!(encode(&BTreeSet::new()).len(), 0);
        assert_eq!(decode([]).unwrap(), BTreeSet::new());
    }

    #[test]
    fn labels_round_trip_through_one_shard() {
        let set = labels(&["a/b", "c", "d/e/f"]);
        let shards = encode(&set);
        assert_eq!(shards, ["a/b\nc\nd/e/f"]);
        assert_eq!(decode(shards.iter().map(String::as_str)).unwrap(), set);
    }

    #[test]
    fn a_long_list_becomes_more_shards_each_under_the_budget() {
        // Twenty maximum-length labels are 2,579 bytes in one blob: over Windows' 2,560-byte limit (the review).
        let set: BTreeSet<_> = (0..20).map(long_label).collect();
        let shards = encode(&set);
        assert!(shards.len() > 1);
        assert!(
            shards.iter().all(|s| s.len() <= SHARD_BUDGET),
            "{:?}",
            shards.iter().map(String::len).collect::<Vec<_>>()
        );
        assert_eq!(decode(shards.iter().map(String::as_str)).unwrap(), set);
    }

    // The longest label fits a shard of its own, or `encode` could not place it.
    const _: () = assert!(SecretLabel::MAX_LEN <= SHARD_BUDGET);

    #[test]
    fn an_empty_shard_is_corrupt() {
        assert_eq!(decode([""]).unwrap_err(), SecretError::corrupt(INDEX_WHAT));
        assert_eq!(decode(["a/b", ""]).unwrap_err(), SecretError::corrupt(INDEX_WHAT));
    }

    #[test]
    fn a_blank_record_is_corrupt_not_dropped() {
        for text in ["a/b\n\nc/d", "a/b\n", "\na/b", "a/b\r\nc/d", "a/b\nNOT A LABEL"] {
            assert_eq!(decode([text]).unwrap_err(), SecretError::corrupt(INDEX_WHAT), "{text:?}");
        }
    }

    #[test]
    fn shard_user_names_are_not_labels() {
        assert!(is_shard_user(&shard_user(0)));
        assert!(SecretLabel::new(shard_user(0)).is_err());
        assert!(!is_shard_user("a/b"));
    }
}
