//! The label index: the labels Omuzaki stored, kept in a few keychain entries.
//!
//! One entry per label cannot be listed portably, so the labels are also written to *shards*, and one *head* record
//! says which set of shards is current:
//!
//! - the head, user name `@labels`, holds a generation number;
//! - generation `g` is the shards `@labels.g.0`, `@labels.g.1`, and so on, each holding labels one per line in
//!   sorted order, contiguous from 0. A shard stays under [`SHARD_BUDGET`] because Windows Credential Manager caps one
//!   credential at 2,560 bytes; a long list becomes more shards, never a failed write;
//! - no head means no labels.
//!
//! A change writes the next generation's shards in full and only then switches the head, so a failure part-way
//! leaves the old index exactly as it was. The shards of the old generation are removed afterwards; they are
//! unreferenced, so a failure there leaves clutter and no wrong answer.

use std::collections::BTreeSet;

use omuzaki_secrets_api::error::SecretError;
use omuzaki_secrets_api::label::SecretLabel;

/// The user name of the head record. A label never contains `@`, so no label can be an index entry.
pub(crate) const HEAD_USER: &str = "@labels";

/// What the index is called in an error.
pub(crate) const INDEX_WHAT: &str = "Omuzaki's label list";

/// The most encoded bytes of text in one shard, counting every character at its width in UTF-16 (2 bytes for an
/// ASCII label): the widest any keychain stores it (Windows), and under that keychain's 2,560-byte limit with room
/// to spare.
pub(crate) const SHARD_BUDGET: usize = 2_000;

// The longest label has to fit a shard of its own, or `encode` could not place it.
const _: () = assert!(2 * SecretLabel::MAX_LEN <= SHARD_BUDGET);

/// Whether `user` names an entry of the index (the head or a shard) rather than a secret.
pub(crate) fn is_index_user(user: &str) -> bool {
    user.starts_with('@')
}

/// The user name of shard `n` of generation `generation`.
pub(crate) fn shard_user(generation: u64, n: usize) -> String {
    format!("{HEAD_USER}.{generation}.{n}")
}

/// The head record that makes `generation` current.
pub(crate) fn encode_head(generation: u64) -> String {
    generation.to_string()
}

/// Reads the head record. Anything but a plain number is [`SecretError::Corrupt`].
pub(crate) fn decode_head(text: &str) -> Result<u64, SecretError> {
    text.parse().map_err(|_| SecretError::corrupt(INDEX_WHAT))
}

/// The width of `text` as the widest keychain stores it, in bytes.
fn encoded_len(text: &str) -> usize {
    2 * text.encode_utf16().count()
}

/// The labels as shards, in order: each one label per line, no shard wider than [`SHARD_BUDGET`]. No labels, no
/// shards.
pub(crate) fn encode(labels: &BTreeSet<SecretLabel>) -> Vec<String> {
    let mut shards: Vec<String> = Vec::new();
    for label in labels {
        let text = label.as_str();
        match shards.last_mut() {
            // The `\n` is a character too.
            Some(shard) if encoded_len(shard) + encoded_len("\n") + encoded_len(text) <= SHARD_BUDGET => {
                shard.push('\n');
                shard.push_str(text);
            }
            _ => shards.push(text.to_owned()),
        }
    }
    shards
}

/// Reads [`encode`]'s output. A shard that is empty, or has an empty or invalid line (a blank line, a trailing
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

    /// A label of the longest length there is.
    pub(crate) fn long_label(i: usize) -> SecretLabel {
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
    fn every_shard_fits_the_smallest_keychain_limit_in_utf16() {
        // Ten maximum-length labels are 1,289 characters: 2,578 bytes in UTF-16, over Windows' 2,560 (the review).
        for count in [9, 10, 11, 20, 100] {
            let set: BTreeSet<_> = (0..count).map(long_label).collect();
            let shards = encode(&set);
            for shard in &shards {
                assert!(encoded_len(shard) <= SHARD_BUDGET, "{count} labels: {} bytes", encoded_len(shard));
                assert!(shard.encode_utf16().count() * 2 <= 2_560);
            }
            assert_eq!(decode(shards.iter().map(String::as_str)).unwrap(), set);
        }
        assert!(encode(&(0..10).map(long_label).collect()).len() > 1);
    }

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
    fn the_head_is_a_plain_number() {
        assert_eq!(decode_head(&encode_head(7)).unwrap(), 7);
        for text in ["", "x", "-1", "1\n", " 1"] {
            assert_eq!(decode_head(text).unwrap_err(), SecretError::corrupt(INDEX_WHAT), "{text:?}");
        }
    }

    #[test]
    fn index_entries_are_not_labels() {
        for user in [HEAD_USER.to_owned(), shard_user(3, 4)] {
            assert!(is_index_user(&user));
            assert!(SecretLabel::new(user).is_err());
        }
        assert!(!is_index_user("a/b"));
    }
}
