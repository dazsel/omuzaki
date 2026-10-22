//! What the person approved for a show on this machine (design D11): [`ShowApprovals`], keyed by [`ShowKey`], read
//! through [`Approvals`]. Approvals live in the user's data folder, never in a project, and only a person grants them,
//! in Omuzaki's window: no op, CLI or MCP call can.

use std::collections::BTreeSet;

use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ids::ShowId;
use crate::need::{Invocation, Need};

/// A show on this machine: the project folder's real path and the show's id.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ShowKey {
    /// The project folder's real path.
    pub project: String,
    /// The show.
    pub show: ShowId,
}

impl ShowKey {
    /// The show `show` of the project at `project`.
    pub fn new(project: impl Into<String>, show: ShowId) -> Self {
        Self { project: project.into(), show }
    }
}

/// A program a person approved, pinned by its content (and its script's).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ApprovedProgram {
    /// The invocation approved.
    pub invocation: Invocation,
    /// The program file's BLAKE3 hash when approved.
    pub program_hash: ContentHash,
    /// The script's hash, when the program runs one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_hash: Option<ContentHash>,
}

impl ApprovedProgram {
    /// `invocation`, pinned to `program_hash` (and `script_hash`).
    pub fn new(invocation: Invocation, program_hash: ContentHash, script_hash: Option<ContentHash>) -> Self {
        Self { invocation, program_hash, script_hash }
    }
}

/// Everything approved for one show on this machine. Built with `ShowApprovals::default()` and the `with_*` methods.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ShowApprovals {
    /// What the person allowed. A [`Need::Program`] here is pinned by an entry of [`programs`](Self::programs).
    #[serde(default)]
    pub granted: BTreeSet<Need>,
    /// The approved programs, with their hashes.
    #[serde(default)]
    pub programs: Vec<ApprovedProgram>,
    /// The folders the person chose for file needs (ADR 0013); the session's fs roots.
    #[serde(default)]
    pub roots: Vec<String>,
    /// The show's spend cap, in millionths of a US dollar (default 0: no spending).
    #[serde(default)]
    pub spend_cap_usd_micros: u64,
    /// Start this show when the project opens, on this machine only.
    #[serde(default)]
    pub autostart: bool,
}

impl ShowApprovals {
    /// With `need` allowed.
    #[must_use]
    pub fn with_granted(mut self, need: Need) -> Self {
        self.granted.insert(need);
        self
    }

    /// With this program approved (and its need granted).
    #[must_use]
    pub fn with_program(mut self, program: ApprovedProgram) -> Self {
        self.granted.insert(Need::Program(program.invocation.clone()));
        self.programs.push(program);
        self
    }

    /// With this fs root.
    #[must_use]
    pub fn with_root(mut self, root: impl Into<String>) -> Self {
        self.roots.push(root.into());
        self
    }

    /// With this spend cap.
    #[must_use]
    pub fn with_spend_cap(mut self, usd_micros: u64) -> Self {
        self.spend_cap_usd_micros = usd_micros;
        self
    }

    /// Starting with the project, or not.
    #[must_use]
    pub fn with_autostart(mut self, autostart: bool) -> Self {
        self.autostart = autostart;
        self
    }

    /// The needs among `needs` that aren't granted, in order. Declaration needs ([`Need::is_declaration`]) are met
    /// by the show itself and never listed.
    pub fn missing<'a>(&self, needs: impl IntoIterator<Item = &'a Need>) -> Vec<Need> {
        needs.into_iter().filter(|need| !need.is_declaration() && !self.granted.contains(need)).cloned().collect()
    }
}

/// The approvals store, as the engine and the `exec.run` op read it. The store's own method that grants an approval
/// is wired into the UI and is not part of this trait, so no op can call it.
pub trait Approvals: Send + Sync {
    /// What is approved for `key`; nothing (the default) for a show never approved.
    fn show(&self, key: &ShowKey) -> ShowApprovals;

    /// Whether this exact invocation, with the program's (and script's) content hashed as given, was approved on
    /// this machine.
    fn program_trusted(
        &self,
        invocation: &Invocation,
        program_hash: &ContentHash,
        script_hash: Option<&ContentHash>,
    ) -> bool;
}
