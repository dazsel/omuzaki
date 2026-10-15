//! The candidates tray's document contract (D6, D7): records at `tray/<id>` in the project document, bytes in the
//! store. Undo, branches, merge, the journal and recovery cover it with no new mechanism, because every change is a
//! [`Patch`].
//!
//! Every function reads through [`DocRead`] and returns a patch to apply; nothing here changes a document itself.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_doc_api::{DocPath, DocRead, Patch, Value};
use omuzaki_store_api::{ContentHash, Provenance, Store, StoreError};
use serde::Deserialize as _;

use crate::candidate::{Candidate, CandidateState, Usable};
use crate::id::{CandidateId, RunId};
use crate::request::Request;
use crate::rights::join_provenance;

/// The effective provenance (D6) of every asset a request names, keyed by hash: what the calling op resolves from
/// its document and hands to the host, which holds none.
pub type InputRights = BTreeMap<ContentHash, Provenance>;

/// The top-level node that holds the records: one entity per file, `tray/<id>.toml` in the project.
pub const ROOT: &str = "tray";

/// Why a tray operation failed. Each `Display` says what to do; none holds a secret.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TrayError {
    /// A record does not read as a candidate.
    #[error("{path} is not a tray record ({reason}); fix or remove it")]
    #[non_exhaustive]
    Shape {
        /// The record's path, `tray/<id>`.
        path: String,
        /// What is wrong.
        reason: String,
    },
    /// A record with this id exists already.
    #[error("tray record {0} exists already; use a new run's ids")]
    Duplicate(CandidateId),
    /// No record has this id.
    #[error("no tray record {0}; list the tray to see its candidates")]
    Missing(CandidateId),
    /// The record was decided already; a decision is made once.
    #[error("tray record {0} was decided already; regenerate it to try again")]
    AlreadyDecided(CandidateId),
    /// The new state is not a decision.
    #[error("a decision on {0} accepts or rejects it")]
    Undecided(CandidateId),
    /// The store does not hold an asset the request names.
    #[error("asset {0} is not in the store; import it first")]
    UnknownAsset(ContentHash),
    /// The store failed.
    #[error("the asset store failed: {0}")]
    Store(String),
}

impl TrayError {
    /// [`TrayError::Shape`].
    pub fn shape(path: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::Shape { path: path.into(), reason: reason.into() }
    }
}

fn root() -> DocPath {
    DocPath::root().join(ROOT)
}

/// The path of a record: `tray/<id>`.
pub fn path(id: &CandidateId) -> DocPath {
    root().join(id.as_str())
}

/// Every record, sorted by id. A record that does not read is an error naming its path, never skipped.
///
/// # Errors
/// [`TrayError::Shape`].
pub fn read_all(doc: &dyn DocRead) -> Result<Vec<Candidate>, TrayError> {
    let Some(node) = doc.get(&root()) else {
        return Ok(Vec::new());
    };
    let Value::Object(records) = node else {
        return Err(TrayError::shape(ROOT, "expected a table of records"));
    };
    let mut all = records.iter().map(|(key, value)| decode(key, value)).collect::<Result<Vec<_>, _>>()?;
    all.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(all)
}

fn decode(key: &str, value: &Value) -> Result<Candidate, TrayError> {
    let at = || format!("{ROOT}/{key}");
    let c = Candidate::deserialize(value).map_err(|e| TrayError::shape(at(), e.to_string()))?;
    if c.id.as_str() == key { Ok(c) } else { Err(TrayError::shape(at(), format!("its id says {}", c.id))) }
}

/// The record `id`, if there is one.
///
/// # Errors
/// [`TrayError::Shape`] if it does not read.
pub fn get(doc: &dyn DocRead, id: &CandidateId) -> Result<Option<Candidate>, TrayError> {
    doc.get(&path(id)).map(|value| decode(id.as_str(), value)).transpose()
}

/// Adds one record; [`land`] adds a run's.
///
/// # Errors
/// As [`land`].
pub fn add(doc: &dyn DocRead, c: &Candidate) -> Result<Patch, TrayError> {
    land(doc, std::slice::from_ref(c))
}

/// Every candidate of a run, as one patch: one undo step. Creates `tray` first when it is absent (a change's parent
/// must exist), then one change per record. What every caller op lands with.
///
/// # Errors
/// [`TrayError::Duplicate`] for an id the document or the batch already has.
pub fn land(doc: &dyn DocRead, candidates: &[Candidate]) -> Result<Patch, TrayError> {
    let mut patch = Patch::new();
    if doc.get(&root()).is_none() {
        patch = patch.with(root(), None, Some(Value::Object(serde_json::Map::new())));
    }
    let mut ids = BTreeSet::new();
    for c in candidates {
        if !ids.insert(&c.id) || doc.get(&path(&c.id)).is_some() {
            return Err(TrayError::Duplicate(c.id.clone()));
        }
        patch = patch.with(path(&c.id), None, Some(encode(&path(&c.id), c)?));
    }
    Ok(patch)
}

fn encode<T: serde::Serialize>(at: &DocPath, value: &T) -> Result<Value, TrayError> {
    serde_json::to_value(value).map_err(|e| TrayError::shape(at.to_string(), e.to_string()))
}

/// Moves a pending record to accepted or rejected: a patch of its `state` alone, so its inverse is an exact undo.
/// Who may decide is [`Candidate::may_decide`].
///
/// # Errors
/// [`TrayError::Missing`], [`TrayError::AlreadyDecided`], or [`TrayError::Undecided`] when `state` is pending.
pub fn decide(doc: &dyn DocRead, id: &CandidateId, state: &CandidateState) -> Result<Patch, TrayError> {
    let c = get(doc, id)?.ok_or_else(|| TrayError::Missing(id.clone()))?;
    if !c.state.is_pending() {
        return Err(TrayError::AlreadyDecided(id.clone()));
    }
    if state.is_pending() {
        return Err(TrayError::Undecided(id.clone()));
    }
    let at = path(id).join("state");
    Ok(Patch::new().with(at.clone(), Some(encode(&at, &c.state)?), Some(encode(&at, state)?)))
}

/// May an op use `asset`? [`Usable::Yes`] when no record names it (an import, a library item) or any record for it is
/// accepted; otherwise the first pending record's id, else the first rejected one's.
///
/// # Errors
/// As [`read_all`].
pub fn usable(doc: &dyn DocRead, asset: &ContentHash) -> Result<Usable, TrayError> {
    let named: Vec<Candidate> = read_all(doc)?.into_iter().filter(|c| c.asset == *asset).collect();
    if named.is_empty() || named.iter().any(|c| matches!(c.state, CandidateState::Accepted { .. })) {
        return Ok(Usable::Yes);
    }
    Ok(match named.iter().find(|c| c.state.is_pending()) {
        Some(c) => Usable::Pending(c.id.clone()),
        None => Usable::Rejected(named[0].id.clone()),
    })
}

/// The hashes the store's garbage collection must keep: those named by pending or accepted records (N3).
///
/// # Errors
/// As [`read_all`].
pub fn roots(doc: &dyn DocRead) -> Result<BTreeSet<ContentHash>, TrayError> {
    Ok(read_all(doc)?
        .into_iter()
        .filter(|c| !matches!(c.state, CandidateState::Rejected { .. }))
        .map(|c| c.asset)
        .collect())
}

/// The candidates of one run, sorted by id (replay, D10).
///
/// # Errors
/// As [`read_all`].
pub fn of_run(doc: &dyn DocRead, run: RunId) -> Result<Vec<Candidate>, TrayError> {
    Ok(read_all(doc)?.into_iter().filter(|c| c.run == run).collect())
}

/// `stored`, the store's record for `hash`, joined with the provenance of every tray record naming `hash`, in any
/// state, strictest wins ([`join_provenance`]). The store keeps the first provenance of identical bytes, so every
/// reader of a hash the tray names uses this, never `Store::provenance` alone (D6).
///
/// # Errors
/// As [`read_all`].
pub fn effective_provenance(
    doc: &dyn DocRead,
    hash: &ContentHash,
    stored: Provenance,
) -> Result<Provenance, TrayError> {
    let records = read_all(doc)?;
    Ok(join_provenance(stored, records.iter().filter(|c| c.asset == *hash).map(|c| &c.provenance)))
}

/// For each of `req.input_assets()`: the store's provenance joined by [`effective_provenance`]. What a caller op
/// resolves in `plan` and again when its job starts and finishes (D4.2, D4.3).
///
/// # Errors
/// [`TrayError::UnknownAsset`] for a hash the store does not hold, [`TrayError::Store`] if it fails.
pub fn input_rights(doc: &dyn DocRead, store: &dyn Store, req: &Request) -> Result<InputRights, TrayError> {
    let mut rights = InputRights::new();
    for hash in req.input_assets() {
        if rights.contains_key(&hash) {
            continue;
        }
        let stored = store.provenance(&hash).map_err(|e| match e {
            StoreError::Missing(h) => TrayError::UnknownAsset(h),
            other => TrayError::Store(other.to_string()),
        })?;
        rights.insert(hash, effective_provenance(doc, &hash, stored)?);
    }
    Ok(rights)
}
