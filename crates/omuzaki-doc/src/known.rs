//! What each project file held the last time this process read or wrote it.
//!
//! Rendering a save against this text (and not against a fresh read of the disk) makes rendering a pure function of
//! memory, and the hash is what a save compares with the file on disk to notice that someone else changed it.

use std::collections::{BTreeMap, VecDeque};

use omuzaki_store_api::ContentHash;

use crate::layout::Segs;

/// One file's exact text and the hash of its bytes.
#[derive(Clone, Debug)]
struct KnownFile {
    text: String,
    hash: ContentHash,
}

/// How many earlier texts of each file are kept, for [`Known::earlier`].
const EARLIER_KEPT: usize = 4;

/// The last-known text and hash of every file, by document path, and the few texts each had before.
#[derive(Clone, Debug, Default)]
pub(crate) struct Known {
    files: BTreeMap<Segs, KnownFile>,
    /// Earlier texts of each file, newest first: a save that brings a file's table back to one of these (an undo)
    /// writes that text again, byte for byte, instead of a re-rendering that may order things differently (#2233).
    earlier: BTreeMap<Segs, VecDeque<String>>,
}

impl Known {
    /// Records `text` (whose bytes hash to `hash`) as the content of the file for `segs`, replacing what was known.
    pub(crate) fn record(&mut self, segs: Segs, text: String, hash: ContentHash) {
        if let Some(old) = self.files.insert(segs.clone(), KnownFile { text, hash }) {
            self.keep_earlier(segs, old.text);
        }
    }

    /// Forgets the file for `segs` (it was deleted); its text is kept among the earlier ones.
    pub(crate) fn forget(&mut self, segs: &[String]) {
        if let Some(old) = self.files.remove(segs) {
            self.keep_earlier(segs.to_vec(), old.text);
        }
    }

    fn keep_earlier(&mut self, segs: Segs, text: String) {
        let kept = self.earlier.entry(segs).or_default();
        kept.retain(|t| *t != text);
        kept.push_front(text);
        kept.truncate(EARLIER_KEPT);
    }

    /// Texts the file had before its last-known one (or before it was deleted), newest first.
    pub(crate) fn earlier(&self, segs: &[String]) -> impl Iterator<Item = &str> {
        self.earlier.get(segs).into_iter().flatten().map(String::as_str)
    }

    /// The exact text the file last had.
    pub(crate) fn text(&self, segs: &[String]) -> Option<&str> {
        self.files.get(segs).map(|f| f.text.as_str())
    }

    /// The hash of that text's bytes.
    pub(crate) fn hash(&self, segs: &[String]) -> Option<ContentHash> {
        self.files.get(segs).map(|f| f.hash)
    }

    /// Every known file's hash, in path order.
    pub(crate) fn hashes(&self) -> impl Iterator<Item = (&Segs, ContentHash)> {
        self.files.iter().map(|(segs, f)| (segs, f.hash))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(path: &str) -> Segs {
        path.split('/').map(str::to_owned).collect()
    }

    #[test]
    fn a_recorded_file_has_its_text_and_the_hash_of_its_bytes() {
        let mut known = Known::default();
        known.record(segs("a/b"), "x = 1\n".into(), ContentHash::of(b"x = 1\n"));
        assert_eq!(known.text(&segs("a/b")), Some("x = 1\n"));
        assert_eq!(known.hash(&segs("a/b")), Some(ContentHash::of(b"x = 1\n")));
        assert_eq!(known.hashes().count(), 1);
    }

    #[test]
    fn recording_again_replaces_and_forgetting_removes() {
        let mut known = Known::default();
        known.record(segs("a"), "1".into(), ContentHash::of(b"1"));
        known.record(segs("a"), "2".into(), ContentHash::of(b"2"));
        assert_eq!(known.text(&segs("a")), Some("2"));
        known.forget(&segs("a"));
        assert!(known.text(&segs("a")).is_none() && known.hash(&segs("a")).is_none());
    }
}
