//! Commits: [`CommitId`], the [`LogRange`] a log walks, what a commit holds and the files of its tree.

use std::borrow::Cow;

use omuzaki_core::time::Seconds;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};

use crate::error::GitError;
use crate::status::ChangedFile;

/// The largest file `tree_at` returns bytes for; bigger ones come back as [`TreeEntry::Skipped`] (design §5).
pub const MAX_IMPORT_FILE_BYTES: u64 = 8 * 1024 * 1024;

/// A full object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex characters.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct CommitId(String);

impl JsonSchema for CommitId {
    fn inline_schema() -> bool {
        false
    }

    fn schema_name() -> Cow<'static, str> {
        "CommitId".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "pattern": "^([0-9a-f]{40}|[0-9a-f]{64})$",
            "description": "A full object id: 40 (SHA-1) or 64 (SHA-256) lowercase hex characters."
        })
    }
}

impl CommitId {
    /// Accepts 40 or 64 lowercase hex characters and nothing else, so an id can never be read as a git option.
    ///
    /// # Errors
    /// [`GitError::BadRev`] for any other text.
    pub fn parse(s: &str) -> Result<Self, GitError> {
        let hex = s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
        if hex && matches!(s.len(), 40 | 64) { Ok(Self(s.to_owned())) } else { Err(GitError::BadRev) }
    }

    /// The whole id.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The first 8 characters, for display.
    pub fn short(&self) -> &str {
        &self.0[..8]
    }
}

impl<'de> Deserialize<'de> for CommitId {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        Self::parse(&s).map_err(|_| serde::de::Error::custom("not a 40 or 64 digit lowercase hex object id"))
    }
}

/// One commit in a log.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Commit {
    /// The commit's id.
    pub id: CommitId,
    /// Its parents, first parent first; empty for a root commit.
    pub parents: Vec<CommitId>,
    /// The author's name, as git records it.
    pub author: String,
    /// When it was authored, in seconds since the Unix epoch.
    pub at: Seconds,
    /// The first line of the message.
    pub subject: String,
}

/// Which commits [`Git::log`](crate::git::Git::log) walks.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum LogRange {
    /// Back from `HEAD`.
    #[default]
    Head,
    /// Back from one revision.
    Rev(String),
    /// The commits reachable from `to` but not from `from`.
    Between {
        /// The excluded end.
        from: String,
        /// The included end.
        to: String,
    },
}

/// A commit with the files it changed, against its first parent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CommitDetail {
    /// The commit.
    pub commit: Commit,
    /// Changed files; never [`ChangeKind::Untracked`](crate::status::ChangeKind::Untracked) or `Conflicted`.
    pub files: Vec<ChangedFile>,
}

/// The files of a commit, to import as a point. Carries bytes, so it never crosses an op.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitTree {
    /// The commit the tree belongs to.
    pub id: CommitId,
    /// Every file in the tree.
    pub entries: Vec<TreeEntry>,
}

/// One file of a [`CommitTree`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeEntry {
    /// A file read in full.
    File {
        /// Path relative to the folder, `/`-separated.
        path: String,
        /// The file's bytes.
        bytes: Vec<u8>,
    },
    /// A file left out; the import shows why.
    Skipped {
        /// Path relative to the folder, `/`-separated.
        path: String,
        /// Size in bytes.
        size: u64,
        /// Why it was left out.
        why: SkipReason,
    },
}

/// Why a file is not in a [`CommitTree`] (design §5).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum SkipReason {
    /// Over [`MAX_IMPORT_FILE_BYTES`].
    TooLarge,
    /// Not valid UTF-8.
    NotText,
}
