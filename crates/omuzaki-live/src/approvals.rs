//! [`DiskApprovals`]: what a person approved for each show on this machine (design D11), in
//! `<data>/automation/approvals.toml`, keyed by [`ShowKey`]. Never in a project, so a shared, copied or imported show
//! starts with nothing approved.
//!
//! **Only a person approves, in Omuzaki's window.** [`DiskApprovals::allow`] and the other writing methods are
//! inherent: the app wires them into the approval card and Settings, and the type is never a service, so no op, CLI
//! or MCP call reaches them (an agent controls both sides of those surfaces, ADR 0020 §7.2). The engine reads the
//! store through the [`Approvals`] trait.
//!
//! [`ceiling`] turns a show's approvals into its automation session's grants: the base `read` and `perform`, the
//! approved scopes and the approved folders as fs roots; never `native`, `input`, `edit:library` or `edit:settings`,
//! even when a hand-edited file says so.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use omuzaki_live_api::need::{Invocation, Need};
use omuzaki_live_api::services::{Approvals, ApprovedProgram, ShowApprovals, ShowKey};
use omuzaki_live_api::show::Finding;
use omuzaki_ops_api::{Actor, Grants, Scope};
use omuzaki_store_api::ContentHash;
use serde::{Deserialize, Serialize};

use crate::compile::{FileHasher, ProgramHasher};
use crate::l10n;
use crate::vars::disk::write_atomic;

/// Why an approval wasn't written.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ApprovalError {
    /// Only a person approves; agents, automations and the system can't.
    #[error("only a person can approve what a show may do")]
    NotAPerson,
    /// A program or script to approve can't be read, so it can't be pinned.
    #[error("{0} can't be read, so it can't be approved")]
    Unreadable(String),
    /// The approvals file couldn't be written; nothing changed.
    #[error("the approvals couldn't be saved: {0}")]
    Write(String),
}

/// One show's entry in the file.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Entry {
    project: String,
    show: omuzaki_live_api::ids::ShowId,
    #[serde(flatten)]
    approvals: ShowApprovals,
}

/// The file: every show's approvals on this machine.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct File {
    #[serde(default)]
    shows: Vec<Entry>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The approvals of this machine, on disk, written atomically.
pub struct DiskApprovals {
    path: PathBuf,
    shows: Mutex<BTreeMap<ShowKey, ShowApprovals>>,
    hasher: Arc<dyn ProgramHasher>,
    findings: Vec<Finding>,
}

impl std::fmt::Debug for DiskApprovals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DiskApprovals").field("path", &self.path).finish_non_exhaustive()
    }
}

impl DiskApprovals {
    /// The approvals under `data` (the app's data folder), hashing programs from the real files.
    pub fn new(data: impl Into<PathBuf>) -> Self {
        Self::with_hasher(data, Arc::new(FileHasher))
    }

    /// As [`new`](Self::new), with programs hashed by `hasher`.
    pub fn with_hasher(data: impl Into<PathBuf>, hasher: Arc<dyn ProgramHasher>) -> Self {
        let path = data.into().join("automation").join("approvals.toml");
        // The app's own state in its data folder, not an op's file access (design D11).
        #[allow(clippy::disallowed_methods)]
        let read = std::fs::read_to_string(&path);
        let (shows, findings) = match read {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (BTreeMap::new(), Vec::new()),
            Err(e) => (BTreeMap::new(), vec![Finding::note(l10n::approvals_unreadable(e.to_string()))]),
            Ok(text) => match toml::from_str::<File>(&text) {
                Ok(file) => {
                    let shows = file
                        .shows
                        .into_iter()
                        .map(|entry| (ShowKey::new(entry.project, entry.show), entry.approvals))
                        .collect();
                    (shows, Vec::new())
                }
                // A damaged file approves nothing; the next approval writes a fresh one.
                Err(e) => (BTreeMap::new(), vec![Finding::note(l10n::approvals_unreadable(e.to_string()))]),
            },
        };
        Self { path, shows: Mutex::new(shows), hasher, findings }
    }

    /// Where the file is.
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }

    /// What loading found: a file that couldn't be read means nothing is approved.
    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Approves `needs` (and the folders `roots`, for file needs) for the show `key`, as the person `by` asked on the
    /// approval card. A program is pinned to its file's (and its script's) BLAKE3 hash now, so a changed file is no
    /// longer trusted. Declaration needs are the show's own and are skipped.
    ///
    /// # Errors
    /// [`ApprovalError::NotAPerson`] for any actor but a person; [`ApprovalError::Unreadable`] when a program or script
    /// can't be hashed; [`ApprovalError::Write`] when the file can't be saved (nothing changes then).
    pub fn allow(&self, key: &ShowKey, needs: &[Need], roots: &[String], by: &Actor) -> Result<(), ApprovalError> {
        if !matches!(by, Actor::Person { .. }) {
            return Err(ApprovalError::NotAPerson);
        }
        let mut programs = Vec::new();
        for need in needs {
            if let Need::Program(invocation) = need {
                programs.push(self.pin(invocation)?);
            }
        }
        self.edit(key, |approvals| {
            let mut approvals = approvals;
            for need in needs.iter().filter(|n| !n.is_declaration() && !matches!(n, Need::Program(_))) {
                approvals = approvals.with_granted(need.clone());
            }
            for program in programs {
                approvals.programs.retain(|p| p.invocation != program.invocation);
                approvals = approvals.with_program(program);
            }
            for root in roots {
                if !approvals.roots.contains(root) {
                    approvals = approvals.with_root(root.clone());
                }
            }
            approvals
        })
    }

    /// Takes back one approval (Settings' revoke button). A program's pin goes with it.
    ///
    /// # Errors
    /// [`ApprovalError::Write`] when the file can't be saved.
    pub fn revoke(&self, key: &ShowKey, need: &Need) -> Result<(), ApprovalError> {
        self.edit(key, |mut approvals| {
            approvals.granted.remove(need);
            if let Need::Program(invocation) = need {
                approvals.programs.retain(|p| &p.invocation != invocation);
            }
            approvals
        })
    }

    /// Takes back a folder chosen for file needs.
    ///
    /// # Errors
    /// [`ApprovalError::Write`] when the file can't be saved.
    pub fn revoke_root(&self, key: &ShowKey, root: &str) -> Result<(), ApprovalError> {
        self.edit(key, |mut approvals| {
            approvals.roots.retain(|r| r != root);
            approvals
        })
    }

    /// Starts the show when its project opens, on this machine only, or not.
    ///
    /// # Errors
    /// [`ApprovalError::Write`] when the file can't be saved.
    pub fn set_autostart(&self, key: &ShowKey, autostart: bool) -> Result<(), ApprovalError> {
        self.edit(key, |approvals| approvals.with_autostart(autostart))
    }

    /// The show's spend cap, in millionths of a US dollar.
    ///
    /// # Errors
    /// [`ApprovalError::Write`] when the file can't be saved.
    pub fn set_spend_cap(&self, key: &ShowKey, usd_micros: u64) -> Result<(), ApprovalError> {
        self.edit(key, |approvals| approvals.with_spend_cap(usd_micros))
    }

    fn pin(&self, invocation: &Invocation) -> Result<ApprovedProgram, ApprovalError> {
        let hash = |path: &str| self.hasher.hash(path).ok_or_else(|| ApprovalError::Unreadable(path.to_owned()));
        let program_hash = hash(&invocation.program)?;
        let script_hash = invocation.script.as_deref().map(hash).transpose()?;
        Ok(ApprovedProgram::new(invocation.clone(), program_hash, script_hash))
    }

    /// Changes `key`'s approvals and writes the whole file; on a failed write the memory keeps the old ones.
    fn edit(&self, key: &ShowKey, change: impl FnOnce(ShowApprovals) -> ShowApprovals) -> Result<(), ApprovalError> {
        let mut shows = lock(&self.shows);
        let mut next = shows.clone();
        let changed = change(next.remove(key).unwrap_or_default());
        if changed == ShowApprovals::default() {
            next.remove(key);
        } else {
            next.insert(key.clone(), changed);
        }
        let file = File {
            shows: next
                .iter()
                .map(|(key, approvals)| Entry {
                    project: key.project.clone(),
                    show: key.show.clone(),
                    approvals: approvals.clone(),
                })
                .collect(),
        };
        let text = toml::to_string(&file).map_err(|e| ApprovalError::Write(e.to_string()))?;
        write_atomic(&self.path, text.as_bytes(), || Ok(())).map_err(|e| ApprovalError::Write(e.to_string()))?;
        *shows = next;
        Ok(())
    }
}

impl Approvals for DiskApprovals {
    fn show(&self, key: &ShowKey) -> ShowApprovals {
        lock(&self.shows).get(key).cloned().unwrap_or_default()
    }

    fn program_trusted(
        &self,
        invocation: &Invocation,
        program_hash: &ContentHash,
        script_hash: Option<&ContentHash>,
    ) -> bool {
        // Machine-wide: the same program, with the same content, approved for any show.
        lock(&self.shows).values().flat_map(|a| &a.programs).any(|p| {
            &p.invocation == invocation && &p.program_hash == program_hash && p.script_hash.as_ref() == script_hash
        })
    }
}

/// Scopes no show's session ever holds, whatever its approvals say: they reach outside Omuzaki's window or change
/// what only the person may change.
const NEVER: [Scope; 4] = [Scope::Native, Scope::Input, Scope::EditLibrary, Scope::EditSettings];

/// A show's automation session grants (design D11): `read`, `perform`, the approved scopes and the approved folders
/// as fs roots, so `OpCtx::fs_access` enforces them in the op too. Never `native`, `input`, `edit:library` or
/// `edit:settings`.
#[must_use]
pub fn ceiling(approvals: &ShowApprovals) -> Grants {
    let mut grants = crate::dispatch::base_grants();
    for need in &approvals.granted {
        if let Need::Scope(scope) = need
            && !NEVER.contains(scope)
        {
            grants = grants.with(*scope);
        }
    }
    approvals.roots.iter().fold(grants, |grants, root| grants.with_fs_root(root.clone()))
}
