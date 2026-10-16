//! `eval sets verify`: every item the manifests name, checked against the store.

use std::path::PathBuf;

use super::store::{ItemState, SetStore, SetsError, parse_hash};
use crate::eval::Discovery;

/// What is wrong with one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Problem {
    /// The manifest's hash is not `b3:` and 64 lowercase hex digits.
    BadHash,
    /// The store has no such item: fetch or re-add it.
    Missing,
    /// The store has it, but its bytes no longer hash to its name.
    Damaged,
    /// Intact, but the SHA-256 index does not name it, so a model suite's pin cannot find it: add it again.
    NotIndexed,
}

impl Problem {
    /// One line saying what it is.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::BadHash => "the hash is not `b3:` and 64 lowercase hex digits",
            Self::Missing => "missing from the store",
            Self::Damaged => "damaged: its bytes no longer match its hash",
            Self::NotIndexed => "not in the SHA-256 index (add it again with `eval sets add`)",
        }
    }
}

/// One item that is not as its manifest says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// The manifest, relative to the workspace root.
    pub manifest: PathBuf,
    /// The set.
    pub set: String,
    /// The item's hash, as the manifest writes it.
    pub item: String,
    /// What is wrong.
    pub problem: Problem,
}

/// The result of checking every item of every manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// Items checked.
    pub checked: usize,
    /// Items that are not as their manifest says, in manifest order.
    pub findings: Vec<Finding>,
}

/// Checks every item of every set manifest in `discovery` against `store`.
///
/// # Errors
/// [`SetsError::Io`] when the store cannot be read.
pub fn verify(discovery: &Discovery, store: &SetStore) -> Result<Verification, SetsError> {
    let mut checked = 0;
    let mut findings = Vec::new();
    for (manifest, set) in &discovery.set_files {
        for item in &set.items {
            checked += 1;
            let problem = match parse_hash(&item.hash) {
                Err(_) => Some(Problem::BadHash),
                Ok(hash) => {
                    let inspection = store.inspect(&hash)?;
                    match inspection.state {
                        ItemState::Missing => Some(Problem::Missing),
                        ItemState::Damaged => Some(Problem::Damaged),
                        ItemState::Present => (!inspection.indexed).then_some(Problem::NotIndexed),
                    }
                }
            };
            findings.extend(problem.map(|problem| Finding {
                manifest: manifest.clone(),
                set: set.id.clone(),
                item: item.hash.clone(),
                problem,
            }));
        }
    }
    Ok(Verification { checked, findings })
}
