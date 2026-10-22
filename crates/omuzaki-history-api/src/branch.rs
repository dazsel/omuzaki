//! Branches and points (VC-02; ADR 0025; design `docs/design/branches.md` sections 4.2, 4.3, 4.7 and 5.7).
//!
//! A **branch** is a named head over the project's history. Its history is a chain of **points**: immutable records
//! in the snapshot store, each naming a snapshot, the branch's previous point and, as a [`Contains`] version vector,
//! the newest point of every other branch the state includes. This module holds the types and the lane manager
//! trait [`Branches`]; `omuzaki-history-mock` has an in-memory stand-in, and `omuzaki-host` implements it.

use std::collections::BTreeMap;
use std::fmt;
use std::str::FromStr;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

use omuzaki_ops_api::{Actor, Grants, Runner, SessionId, Sessions};
use omuzaki_store_api::{ContentHash, HashParseError};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::state::StateRef;
use crate::{Lsn, SnapshotId, WallTime};

/// The id of the first branch, which is also its name.
pub const MAIN_BRANCH: &str = "main";
/// How many hex characters the id of every branch but `main` has.
pub const BRANCH_ID_HEX_LEN: usize = 16;
/// The longest a [`BranchName`] may be, separators included.
pub const MAX_BRANCH_NAME_LEN: usize = 64;
/// The most lanes (open branches) a project has open at once (ADR 0025).
pub const MAX_OPEN_LANES: usize = 8;
/// The version of the canonical point encoding ([`Point::format`]).
pub const POINT_FORMAT: u32 = 1;

/// Why a string is not a [`BranchId`].
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("a branch id is `{MAIN_BRANCH}` or {BRANCH_ID_HEX_LEN} lowercase hex characters; got {0:?}")]
pub struct BranchIdError(pub String);

/// The stable id of a branch: `main` for the first branch, [`BRANCH_ID_HEX_LEN`] lowercase hex characters for the
/// others. Folders and every reference use the id, so a rename touches one file.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct BranchId(String);

impl BranchId {
    /// The id of the first branch.
    pub fn main() -> Self {
        Self(MAIN_BRANCH.to_owned())
    }

    /// Checks `id` against the rule above.
    ///
    /// # Errors
    /// [`BranchIdError`] when `id` is neither `main` nor 16 lowercase hex characters.
    pub fn new(id: impl Into<String>) -> Result<Self, BranchIdError> {
        let id = id.into();
        let hex = id.len() == BRANCH_ID_HEX_LEN && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
        if id == MAIN_BRANCH || hex { Ok(Self(id)) } else { Err(BranchIdError(id)) }
    }

    /// Whether this is the first branch.
    pub fn is_main(&self) -> bool {
        self.0 == MAIN_BRANCH
    }

    /// The id as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for BranchId {
    fn default() -> Self {
        Self::main()
    }
}

impl fmt::Display for BranchId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BranchId {
    type Err = BranchIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for BranchId {
    type Error = BranchIdError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<BranchId> for String {
    fn from(id: BranchId) -> String {
        id.0
    }
}

/// Why a string is not a [`BranchName`]. Each message states the rule that was broken.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BranchNameError {
    /// Nothing was given.
    #[error("a branch name cannot be empty")]
    Empty,
    /// More than [`MAX_BRANCH_NAME_LEN`] characters.
    #[error("a branch name has at most {MAX_BRANCH_NAME_LEN} characters; this one has {0}")]
    TooLong(usize),
    /// A `/` with nothing before or after it.
    #[error("a branch name is segments joined by `/`; a segment cannot be empty")]
    EmptySegment,
    /// A segment that is not lowercase letters, digits and `-`, starting with a letter or digit.
    #[error("segment {0:?} must be lowercase letters, digits and `-`, starting with a letter or digit")]
    BadSegment(String),
}

/// The name people and agents use for a branch: lowercase slug segments (`[a-z0-9][a-z0-9-]*`) joined by `/`, at
/// most [`MAX_BRANCH_NAME_LEN`] characters, for example `aria/oval-face`. Names are unique in a project; because they
/// are lowercase, uniqueness needs no case folding. `main` names only the first branch.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String")]
pub struct BranchName(String);

impl BranchName {
    /// Checks `name` against the rule above.
    ///
    /// # Errors
    /// [`BranchNameError`] naming the rule `name` breaks.
    pub fn new(name: impl Into<String>) -> Result<Self, BranchNameError> {
        let name = name.into();
        if name.is_empty() {
            return Err(BranchNameError::Empty);
        }
        let length = name.chars().count();
        if length > MAX_BRANCH_NAME_LEN {
            return Err(BranchNameError::TooLong(length));
        }
        for segment in name.split('/') {
            if segment.is_empty() {
                return Err(BranchNameError::EmptySegment);
            }
            let mut bytes = segment.bytes();
            let first_ok = bytes.next().is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
            if !first_ok || !bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
                return Err(BranchNameError::BadSegment(segment.to_owned()));
            }
        }
        Ok(Self(name))
    }

    /// The first branch's name.
    pub fn main() -> Self {
        Self(MAIN_BRANCH.to_owned())
    }

    /// The name as text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BranchName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl FromStr for BranchName {
    type Err = BranchNameError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

impl TryFrom<String> for BranchName {
    type Error = BranchNameError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        Self::new(s)
    }
}

impl From<BranchName> for String {
    fn from(name: BranchName) -> String {
        name.0
    }
}

/// The id of a point: the BLAKE3 hash of its canonical JSON ([`Point::canonical_json`]), so equal content always has
/// the same id.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct PointId(pub ContentHash);

impl fmt::Display for PointId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for PointId {
    type Err = HashParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

/// One branch's point as another state includes it: the point and its number on its branch.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Version {
    /// The point's number on its branch: 1, 2, 3... per branch, never reused.
    pub seq: u64,
    /// The point.
    pub point: PointId,
}

impl Version {
    /// The point `point`, number `seq` on its branch.
    pub fn new(seq: u64, point: PointId) -> Self {
        Self { seq, point }
    }

    /// Whether this is newer than `other` on the same branch. Equal numbers (which a branch never produces for
    /// different points) are ordered by point id, so [`Contains::join`] stays commutative.
    fn newer_than(&self, other: &Version) -> bool {
        (self.seq, self.point) > (other.seq, other.point)
    }
}

/// For each branch, the newest of its points a state includes. This is what merging starts from (design 7.3).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct Contains(BTreeMap<BranchId, Version>);

impl Contains {
    /// A state that includes nothing of any other branch.
    pub fn new() -> Self {
        Self::default()
    }

    /// The same set, now also including `version` of `branch` (kept only when newer than what is there).
    #[must_use]
    pub fn with(mut self, branch: BranchId, version: Version) -> Self {
        match self.0.get(&branch) {
            Some(known) if !version.newer_than(known) => {}
            _ => {
                self.0.insert(branch, version);
            }
        }
        self
    }

    /// The newest version of `branch` this includes.
    pub fn get(&self, branch: &BranchId) -> Option<&Version> {
        self.0.get(branch)
    }

    /// Every branch with the newest version of it this includes, in branch id order.
    pub fn iter(&self) -> impl Iterator<Item = (&BranchId, &Version)> {
        self.0.iter()
    }

    /// Whether nothing is included.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The union: for each branch, the newer of the two versions. Commutative, associative and idempotent.
    #[must_use]
    pub fn join(&self, other: &Contains) -> Contains {
        other.0.iter().fold(self.clone(), |acc, (branch, version)| acc.with(branch.clone(), *version))
    }

    /// Whether this state includes `version` of `branch`: it holds that version or a newer one. Numbers are never
    /// reused on a branch, so the number decides.
    pub fn includes(&self, branch: &BranchId, version: &Version) -> bool {
        self.0.get(branch).is_some_and(|known| known.seq >= version.seq)
    }
}

/// Why a point was made (design 4.2). Serialised lowercase.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum PointKind {
    /// The start of a branch.
    Fork,
    /// An explicit checkpoint.
    Checkpoint,
    /// A request for review (the agent proposes, the person merges).
    Ready,
    /// The folder branch was switched away from.
    Switch,
    /// The branch's lane was closed.
    Close,
    /// The state before a restore replaced it.
    Restore,
    /// A commit imported from git (VC-04).
    Import,
}

/// A point: an immutable record naming a snapshot of a branch's state. Its [`id`](Self::id) is the hash of its
/// [canonical JSON](Self::canonical_json), so anything that changes a field makes a different point.
///
/// Non-exhaustive: build one with [`Point::new`] and the `with_*` methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Point {
    /// The encoding version ([`POINT_FORMAT`]).
    pub format: u32,
    /// The branch the point belongs to.
    pub branch: BranchId,
    /// The point's number on its branch.
    pub seq: u64,
    /// The snapshot holding the state's files.
    pub snapshot: SnapshotId,
    /// Where in the branch's journal the state was taken (informational).
    #[schemars(with = "u64")]
    pub lsn: Lsn,
    /// The branch's previous point, plus any points newly merged in since it: for the graph view only.
    #[serde(default)]
    pub parents: Vec<PointId>,
    /// For every other branch, the newest of its points this state includes: the document's `merges` record joined with
    /// the branch's `inherits` (design 5.3).
    #[serde(default)]
    pub contains: Contains,
    /// Why the point was made.
    pub kind: PointKind,
    /// A short description.
    #[serde(default)]
    pub label: String,
    /// Who made it.
    pub by: Actor,
    /// When.
    #[schemars(with = "u64")]
    pub at: WallTime,
    /// The executor's history export at that state, when no transaction was open. Local only, like a snapshot's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<ContentHash>,
}

impl Point {
    /// A point with the mandatory facts; the optional ones start empty.
    pub fn new(
        branch: BranchId,
        seq: u64,
        snapshot: SnapshotId,
        lsn: Lsn,
        kind: PointKind,
        by: Actor,
        at: WallTime,
    ) -> Self {
        Self {
            format: POINT_FORMAT,
            branch,
            seq,
            snapshot,
            lsn,
            parents: Vec::new(),
            contains: Contains::new(),
            kind,
            label: String::new(),
            by,
            at,
            history: None,
        }
    }

    /// The same point with `parents`.
    #[must_use]
    pub fn with_parents(mut self, parents: Vec<PointId>) -> Self {
        self.parents = parents;
        self
    }

    /// The same point with `contains`.
    #[must_use]
    pub fn with_contains(mut self, contains: Contains) -> Self {
        self.contains = contains;
        self
    }

    /// The same point with `label`.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = label.into();
        self
    }

    /// The same point with the history export `history`.
    #[must_use]
    pub fn with_history(mut self, history: ContentHash) -> Self {
        self.history = Some(history);
        self
    }

    /// The canonical JSON: fields in the order of the type, no whitespace, branches in id order.
    ///
    /// # Errors
    /// [`BranchError::Io`] if encoding fails, which no point built here can cause.
    pub fn canonical_json(&self) -> Result<String, BranchError> {
        serde_json::to_string(self).map_err(|e| BranchError::Io(format!("encoding a point failed: {e}")))
    }

    /// The point's id: the BLAKE3 hash of its canonical JSON.
    ///
    /// # Errors
    /// As [`canonical_json`](Self::canonical_json).
    pub fn id(&self) -> Result<PointId, BranchError> {
        self.canonical_json().map(|json| PointId(ContentHash::of(json.as_bytes())))
    }
}

/// What a branch's lane is doing.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LaneState {
    /// No executor, journal or keeper is running for the branch.
    #[default]
    Closed,
    /// The lane is open.
    #[non_exhaustive]
    Open {
        /// Whether this lane writes the project folder.
        folder: bool,
        /// How many agent sessions are working on it.
        sessions: u32,
    },
}

impl LaneState {
    /// An open lane.
    pub fn open(folder: bool, sessions: u32) -> Self {
        Self::Open { folder, sessions }
    }
}

/// Where a review stands (design 8.3). Serialised kebab-case.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum ReviewState {
    /// Waiting for the person.
    Ready,
    /// The person merged the reviewed point.
    Merged,
    /// The person sent it back with a comment.
    SentBack,
}

/// The latest request for review on a branch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Review {
    /// The reviewed point: later edits on the branch are not part of the review.
    pub point: PointId,
    /// Who asked.
    pub by: Actor,
    /// When.
    #[schemars(with = "u64")]
    pub at: WallTime,
    /// What the requester says about it.
    #[serde(default)]
    pub note: Option<String>,
    /// Where it stands.
    pub state: ReviewState,
}

impl Review {
    /// A request for review of `point`, waiting for the person.
    pub fn new(point: PointId, by: Actor, at: WallTime, note: Option<String>) -> Self {
        Self { point, by, at, note, state: ReviewState::Ready }
    }
}

/// What the person decides about a review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReviewDecision {
    /// Back to the requester, with a comment.
    #[non_exhaustive]
    SendBack {
        /// What to change.
        comment: String,
    },
    /// The reviewed point was merged (the host records it when a merge of that point commits).
    MarkMerged,
}

impl ReviewDecision {
    /// Send the review back with `comment`.
    pub fn send_back(comment: impl Into<String>) -> Self {
        Self::SendBack { comment: comment.into() }
    }
}

/// A branch: its ref (design 4.3) and what its lane is doing.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct BranchInfo {
    /// The stable id.
    pub id: BranchId,
    /// The unique name.
    pub name: BranchName,
    /// A free-text title the UI shows.
    #[serde(default)]
    pub title: Option<String>,
    /// The newest point.
    pub head: PointId,
    /// The head's number on the branch.
    pub seq: u64,
    /// The highest number a point of this branch has taken, the head's or one made from an older state (which
    /// doesn't move the head): the next point's number is one more, so numbers are never reused. 0 when only the head
    /// has numbered.
    #[serde(default)]
    pub last_seq: u64,
    /// The fork point on the source branch; absent for `main`.
    #[serde(default)]
    pub from: Option<PointId>,
    /// What the branch started with: the fork point and what it contained.
    #[serde(default)]
    pub inherits: Contains,
    /// The parts of the project the branch is about (document paths such as `characters/aria`).
    #[serde(default)]
    pub focus: Vec<String>,
    /// When it was made.
    #[schemars(with = "u64")]
    pub created: WallTime,
    /// Who made it.
    pub created_by: Actor,
    /// Whether Omuzaki made it (a restore's backup), not someone.
    #[serde(default)]
    pub auto: bool,
    /// The agent session it was made for.
    #[serde(default)]
    pub owner_session: Option<SessionId>,
    /// When it went into the bin; absent while it is live.
    #[serde(default)]
    #[schemars(with = "Option<u64>")]
    pub deleted: Option<WallTime>,
    /// The latest request for review, if any.
    #[serde(default)]
    pub review: Option<Review>,
    /// What its lane is doing.
    #[serde(default)]
    pub lane: LaneState,
}

impl BranchInfo {
    /// A live, closed branch with the mandatory facts; the optional ones start empty.
    pub fn new(id: BranchId, name: BranchName, head: PointId, seq: u64, created: WallTime, created_by: Actor) -> Self {
        Self {
            id,
            name,
            title: None,
            head,
            seq,
            last_seq: 0,
            from: None,
            inherits: Contains::new(),
            focus: Vec::new(),
            created,
            created_by,
            auto: false,
            owner_session: None,
            deleted: None,
            review: None,
            lane: LaneState::Closed,
        }
    }
}

/// A request to make a branch (design 5.4). Build it with [`NewBranch::new`] and the `with_*` methods.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct NewBranch {
    /// The new branch's name.
    pub name: BranchName,
    /// A display title.
    #[serde(default)]
    pub title: Option<String>,
    /// What the branch is about.
    #[serde(default)]
    pub focus: Vec<String>,
    /// The branch to fork from.
    pub from_branch: BranchId,
    /// The state of that branch to fork at; `now` checkpoints it first. A reference to the source branch itself, by
    /// name, is `now`: its live document, not its head.
    pub at: StateRef,
    /// The agent session the branch is made for.
    #[serde(default)]
    pub for_session: Option<SessionId>,
}

impl NewBranch {
    /// A branch called `name`, forked from the current state of `from_branch`.
    pub fn new(name: BranchName, from_branch: BranchId) -> Self {
        Self { name, title: None, focus: Vec::new(), from_branch, at: StateRef::Now, for_session: None }
    }

    /// The same request with a display `title`.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// The same request with a `focus`.
    #[must_use]
    pub fn with_focus(mut self, focus: Vec<String>) -> Self {
        self.focus = focus;
        self
    }

    /// The same request forking at `at`.
    #[must_use]
    pub fn at(mut self, at: StateRef) -> Self {
        self.at = at;
        self
    }

    /// The same request made for agent session `session`.
    #[must_use]
    pub fn for_session(mut self, session: SessionId) -> Self {
        self.for_session = Some(session);
        self
    }
}

/// The result of switching the folder.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Switched {
    /// The folder branch before.
    pub from: BranchId,
    /// The folder branch now.
    pub to: BranchId,
}

impl Switched {
    /// The folder moved from `from` to `to`.
    pub fn new(from: BranchId, to: BranchId) -> Self {
        Self { from, to }
    }
}

/// The result of a restore (design 9): what was kept, and what was restored.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Restored {
    /// The automatic branch holding the state from before the restore.
    pub backup: BranchInfo,
    /// The state that was restored.
    pub to: StateRef,
}

impl Restored {
    /// A restore to `to`, after keeping the old state on `backup`.
    pub fn new(backup: BranchInfo, to: StateRef) -> Self {
        Self { backup, to }
    }
}

/// An open lane: the runner and the sessions of one branch.
#[non_exhaustive]
pub struct Lane {
    /// Runs ops on the branch's document.
    pub runner: Arc<dyn Runner>,
    /// The agent sessions working on the branch.
    pub sessions: Arc<dyn Sessions>,
}

impl Lane {
    /// A lane of `runner` and `sessions`.
    pub fn new(runner: Arc<dyn Runner>, sessions: Arc<dyn Sessions>) -> Self {
        Self { runner, sessions }
    }
}

impl fmt::Debug for Lane {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Lane").finish_non_exhaustive()
    }
}

/// What happened to the branches, for the UI, the Theatre and agents (design 5.7). Serialised with an `event` tag.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "event", rename_all = "snake_case")]
#[non_exhaustive]
pub enum BranchEvent {
    /// A branch was made.
    #[non_exhaustive]
    Created {
        /// The new branch.
        branch: Box<BranchInfo>,
    },
    /// A branch got a point.
    #[non_exhaustive]
    Checkpointed {
        /// Which branch.
        branch: BranchId,
        /// The new head.
        point: PointId,
        /// Whether it asks for review.
        ready: bool,
    },
    /// A branch asked for review.
    #[non_exhaustive]
    ReviewRequested {
        /// Which branch.
        branch: BranchId,
        /// The reviewed point.
        point: PointId,
    },
    /// The person decided a review.
    #[non_exhaustive]
    Reviewed {
        /// Which branch.
        branch: BranchId,
        /// The review's new state.
        state: ReviewState,
    },
    /// The folder moved to another branch.
    #[non_exhaustive]
    Switched {
        /// The folder branch before.
        from: BranchId,
        /// The folder branch now.
        to: BranchId,
    },
    /// A branch was renamed.
    #[non_exhaustive]
    Renamed {
        /// Which branch.
        branch: BranchId,
        /// Its new name.
        name: BranchName,
    },
    /// A branch went into the bin.
    #[non_exhaustive]
    Deleted {
        /// Which branch.
        branch: BranchId,
    },
    /// A branch came back from the bin.
    #[non_exhaustive]
    Undeleted {
        /// Which branch.
        branch: BranchId,
    },
    /// A branch's lane opened.
    #[non_exhaustive]
    LaneOpened {
        /// Which branch.
        branch: BranchId,
    },
    /// A branch's lane closed.
    #[non_exhaustive]
    LaneClosed {
        /// Which branch.
        branch: BranchId,
    },
    /// A branch's lane was recovered after a crash.
    #[non_exhaustive]
    LaneRecovered {
        /// Which branch.
        branch: BranchId,
        /// What the person should know.
        notice: String,
    },
}

impl BranchEvent {
    /// [`BranchEvent::Created`].
    pub fn created(branch: BranchInfo) -> Self {
        Self::Created { branch: Box::new(branch) }
    }

    /// [`BranchEvent::Checkpointed`].
    pub fn checkpointed(branch: BranchId, point: PointId, ready: bool) -> Self {
        Self::Checkpointed { branch, point, ready }
    }

    /// [`BranchEvent::ReviewRequested`].
    pub fn review_requested(branch: BranchId, point: PointId) -> Self {
        Self::ReviewRequested { branch, point }
    }

    /// [`BranchEvent::Reviewed`].
    pub fn reviewed(branch: BranchId, state: ReviewState) -> Self {
        Self::Reviewed { branch, state }
    }

    /// [`BranchEvent::Switched`].
    pub fn switched(from: BranchId, to: BranchId) -> Self {
        Self::Switched { from, to }
    }

    /// [`BranchEvent::Renamed`].
    pub fn renamed(branch: BranchId, name: BranchName) -> Self {
        Self::Renamed { branch, name }
    }

    /// [`BranchEvent::Deleted`].
    pub fn deleted(branch: BranchId) -> Self {
        Self::Deleted { branch }
    }

    /// [`BranchEvent::Undeleted`].
    pub fn undeleted(branch: BranchId) -> Self {
        Self::Undeleted { branch }
    }

    /// [`BranchEvent::LaneOpened`].
    pub fn lane_opened(branch: BranchId) -> Self {
        Self::LaneOpened { branch }
    }

    /// [`BranchEvent::LaneClosed`].
    pub fn lane_closed(branch: BranchId) -> Self {
        Self::LaneClosed { branch }
    }

    /// [`BranchEvent::LaneRecovered`].
    pub fn lane_recovered(branch: BranchId, notice: impl Into<String>) -> Self {
        Self::LaneRecovered { branch, notice: notice.into() }
    }
}

/// Why a branch call failed. [`hint`](Self::hint) says what to do about it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum BranchError {
    /// No such branch (by id or name).
    #[error("no branch {0}")]
    NotFound(String),
    /// No such point.
    #[error("no point {0}")]
    PointNotFound(PointId),
    /// A branch already has the name.
    #[error("the name {0} is taken")]
    NameTaken(BranchName),
    /// The name breaks the naming rule.
    #[error("invalid branch name: {0}")]
    InvalidName(#[from] BranchNameError),
    /// Something in progress stops the call (an open transaction, files changed outside Omuzaki).
    #[error("busy: {0}")]
    Busy(String),
    /// A rule refuses the call (deleting `main`, deleting the folder branch).
    #[error("refused: {0}")]
    Refused(String),
    /// [`MAX_OPEN_LANES`] lanes are already open.
    #[error("at most {MAX_OPEN_LANES} branches can be open at once")]
    LaneLimit,
    /// The actor may not do this.
    #[error("not allowed: {0}")]
    Denied(String),
    /// A call argument is wrong.
    #[error("invalid request: {0}")]
    Invalid(String),
    /// The disk or the store failed.
    #[error("branch storage failed: {0}")]
    Io(String),
}

impl BranchError {
    /// What the caller can do about it, when there is something.
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::NotFound(_) => Some("list the branches to see their names".to_owned()),
            Self::PointNotFound(_) => Some("a point id comes from a branch's head or a checkpoint".to_owned()),
            Self::NameTaken(_) => Some("pick another name, or rename or delete the branch that has it".to_owned()),
            Self::InvalidName(_) => Some(format!(
                "use lowercase letters, digits and `-`, in segments joined by `/`, at most {MAX_BRANCH_NAME_LEN} characters"
            )),
            Self::Busy(_) => Some("finish or cancel what is open, then retry".to_owned()),
            Self::LaneLimit => Some("close a branch that nobody is working on, then retry".to_owned()),
            Self::Denied(_) => Some("ask the person to do this, or to grant it".to_owned()),
            Self::Refused(_) | Self::Invalid(_) | Self::Io(_) => None,
        }
    }
}

/// The lane manager, implemented by `omuzaki-host`. Surfaces call it; ops never do (design 5.7).
pub trait Branches: Send + Sync {
    /// Every branch, in the bin or not.
    fn list(&self) -> Vec<BranchInfo>;

    /// The branch the project folder shows.
    fn folder(&self) -> BranchId;

    /// Makes a branch (design 5.4). The fork point is a point of the source branch, so versions of it that others
    /// record carry the source's own numbers. `new.at` must be a state of the source branch (its live document, one
    /// of its transactions, snapshots or points in its history); a state of another branch is refused, and the
    /// caller forks that branch instead. An agent makes branches for its own session only (design 8.2).
    ///
    /// # Errors
    /// [`BranchError::NameTaken`], [`BranchError::NotFound`] for the source branch, [`BranchError::Busy`] when an open
    /// transaction on the source blocks the fork, [`BranchError::Invalid`] for a state that is not the source's,
    /// [`BranchError::Denied`] for an agent that makes a branch for another session or has no session to make it
    /// for, [`BranchError::PointNotFound`]
    /// when the source's head (or a point named by `new.at`) is not in the store, or [`BranchError::Io`].
    fn create(&self, new: NewBranch, by: &Actor) -> Result<BranchInfo, BranchError>;

    /// Makes a point of `branch`'s committed state now (design 5.3). `ready` asks for review with that note; an agent
    /// may ask only on a branch it works on (design 8.2).
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Busy`] when an open transaction blocks it,
    /// [`BranchError::Denied`] for an agent asking for review on another branch, [`BranchError::PointNotFound`] when
    /// the branch's head is not in the store (the previous point cannot be read, so none is written after it), or
    /// [`BranchError::Io`].
    fn checkpoint(
        &self,
        branch: &BranchId,
        label: &str,
        ready: Option<String>,
        by: &Actor,
    ) -> Result<PointId, BranchError>;

    /// Moves the folder to `to` (design 5.5), a person's action. The old folder lane stays open as a side lane while
    /// sessions work on it, and closes otherwise; [`LaneState::Open`] says how many do.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Busy`] for an open transaction or a changed file,
    /// [`BranchError::LaneLimit`], [`BranchError::Denied`] for an agent or a plugin, or
    /// [`BranchError::PointNotFound`] when the head of the folder branch or of `to` is not in the store. Everything
    /// that can fail is checked before anything changes, so a failed switch changes nothing.
    fn switch(&self, to: &BranchId, by: &Actor) -> Result<Switched, BranchError>;

    /// Renames a branch and sets its title. A person may rename any branch; an agent only one made for its session
    /// (design 5.6, 8.2). `main` keeps its name.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::NameTaken`], [`BranchError::Denied`], or [`BranchError::Refused`]
    /// for `main`.
    fn rename(
        &self,
        branch: &BranchId,
        name: BranchName,
        title: Option<String>,
        by: &Actor,
    ) -> Result<BranchInfo, BranchError>;

    /// Puts a branch in the bin. A person may delete any branch; an agent only one made for its session.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Refused`] for `main`, the folder branch or an open lane, or
    /// [`BranchError::Denied`].
    fn delete(&self, branch: &BranchId, by: &Actor) -> Result<(), BranchError>;

    /// Brings a branch back from the bin, on the terms of [`delete`](Self::delete).
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::NameTaken`] when its name was taken meanwhile, or
    /// [`BranchError::Denied`] for an agent that the branch was not made for.
    fn undelete(&self, branch: &BranchId, by: &Actor) -> Result<BranchInfo, BranchError>;

    /// Records the person's decision on the branch's latest review. Only a person decides (design 8.2).
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Refused`] when there is no review waiting, or
    /// [`BranchError::Denied`] for an agent or a plugin.
    fn review(&self, branch: &BranchId, decision: ReviewDecision, by: &Actor) -> Result<BranchInfo, BranchError>;

    /// Restores `branch` to `to`, first keeping the current state on an automatic branch (design 9). `grants` are
    /// the caller's, for the restore op: it needs the scopes of what it changes, as a merge does (`edit:character`,
    /// plus `edit:scene` for anything outside `characters/`, design 7.7). Everything that can refuse does so before
    /// anything changes. `to` must be a state of `branch`, as for [`create`](Self::create); a reference to `branch`
    /// itself is `now`, which changes nothing.
    ///
    /// # Errors
    /// [`BranchError::NotFound`], [`BranchError::Busy`] when an open transaction blocks recording the current state,
    /// [`BranchError::Denied`] naming a scope the grants lack, [`BranchError::PointNotFound`] when the branch's head
    /// is not in the store, [`BranchError::Invalid`] or [`BranchError::Io`].
    fn restore(&self, branch: &BranchId, to: StateRef, by: &Actor, grants: &Grants) -> Result<Restored, BranchError>;

    /// The branch's lane, opening it if it is closed.
    ///
    /// # Errors
    /// [`BranchError::NotFound`] or [`BranchError::LaneLimit`].
    fn lane(&self, branch: &BranchId) -> Result<Lane, BranchError>;

    /// A stream of what happens to the branches from now on.
    fn subscribe(&self) -> Receiver<BranchEvent>;
}

#[cfg(test)]
mod tests {
    use schemars::JsonSchema;
    use serde::de::DeserializeOwned;
    use serde_json::{Value, json};

    use super::*;

    type Edit = Box<dyn Fn(&mut Point)>;

    fn pid(tag: &str) -> PointId {
        PointId(ContentHash::of(tag.as_bytes()))
    }

    fn hex_id(n: u8) -> BranchId {
        BranchId::new(format!("{n:016x}")).unwrap_or_default()
    }

    fn agent() -> Actor {
        Actor::Agent { id: "claude-code#12".to_owned(), model: None }
    }

    fn point() -> Point {
        Point::new(
            hex_id(9),
            4,
            SnapshotId(ContentHash::of(b"snap")),
            Lsn(1832),
            PointKind::Ready,
            agent(),
            WallTime(1_760_279_525_000),
        )
        .with_parents(vec![pid("prev")])
        .with_contains(Contains::new().with(BranchId::main(), Version::new(12, pid("m12"))))
        .with_label("Oval face, version A")
        .with_history(ContentHash::of(b"history"))
    }

    fn info() -> BranchInfo {
        let mut info = BranchInfo::new(
            hex_id(9),
            BranchName::new("aria/oval-face").unwrap_or_else(|_| BranchName::main()),
            pid("head"),
            4,
            WallTime(5),
            agent(),
        );
        info.title = Some("Oval face, version A".to_owned());
        info.from = Some(pid("fork"));
        info.inherits = Contains::new().with(BranchId::main(), Version::new(12, pid("m12")));
        info.focus = vec!["characters/aria".to_owned()];
        info.owner_session = Some(SessionId(12));
        info.review = Some(Review::new(pid("head"), agent(), WallTime(6), Some("rounder chin".to_owned())));
        info.lane = LaneState::open(false, 1);
        info
    }

    /// Serialises `value`, reads it back equal, and checks the JSON Schema describes every field it wrote.
    fn round_trips<T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug + JsonSchema>(value: &T) -> Value {
        let json = serde_json::to_value(value).unwrap_or_default();
        let back: Result<T, _> = serde_json::from_value(json.clone());
        assert_eq!(back.as_ref().ok(), Some(value), "round trip of {json}");
        let schema = serde_json::to_value(schemars::schema_for!(T)).unwrap_or_default();
        assert!(schema.as_object().is_some_and(|o| !o.is_empty()), "{schema}");
        if let (Some(fields), Some(props)) = (json.as_object(), schema.get("properties").and_then(Value::as_object)) {
            for key in fields.keys() {
                assert!(props.contains_key(key), "schema lacks {key}: {schema}");
            }
        }
        json
    }

    #[test]
    fn branch_ids_are_main_or_sixteen_lowercase_hex() {
        for good in ["main", "9c41d07a5e2b1f30", "0000000000000000"] {
            assert_eq!(BranchId::new(good).map(|i| i.to_string()), Ok(good.to_owned()), "{good}");
        }
        for bad in
            ["", "Main", "9C41D07A5E2B1F30", "9c41d07a5e2b1f3", "9c41d07a5e2b1f300", "9c41d07a5e2b1fg0", "aria/x"]
        {
            assert_eq!(BranchId::new(bad), Err(BranchIdError(bad.to_owned())), "{bad}");
        }
        assert!(BranchId::main().is_main() && BranchId::default().is_main());
        assert_eq!(round_trips(&hex_id(3)), json!("0000000000000003"));
        assert!(serde_json::from_value::<BranchId>(json!("nope")).is_err());
        assert_eq!("main".parse::<BranchId>(), Ok(BranchId::main()));
    }

    #[test]
    fn branch_names_follow_design_4_7() {
        let long_ok = "a".repeat(MAX_BRANCH_NAME_LEN);
        let long_bad = "a".repeat(MAX_BRANCH_NAME_LEN + 1);
        let accepted =
            ["main", "aria", "aria/oval-face", "designer/oval-a", "a1/b2/c3", "0/9", "a-", "x--y", long_ok.as_str()];
        for name in accepted {
            assert_eq!(BranchName::new(name).map(|n| n.to_string()), Ok(name.to_owned()), "{name}");
        }
        let refused = [
            ("", BranchNameError::Empty),
            (long_bad.as_str(), BranchNameError::TooLong(MAX_BRANCH_NAME_LEN + 1)),
            ("/", BranchNameError::EmptySegment),
            ("/aria", BranchNameError::EmptySegment),
            ("aria/", BranchNameError::EmptySegment),
            ("aria//face", BranchNameError::EmptySegment),
            ("Aria", BranchNameError::BadSegment("Aria".to_owned())),
            ("aria/Oval", BranchNameError::BadSegment("Oval".to_owned())),
            ("-aria", BranchNameError::BadSegment("-aria".to_owned())),
            ("aria/-x", BranchNameError::BadSegment("-x".to_owned())),
            ("aria_face", BranchNameError::BadSegment("aria_face".to_owned())),
            ("aria face", BranchNameError::BadSegment("aria face".to_owned())),
            ("aria/é", BranchNameError::BadSegment("é".to_owned())),
        ];
        for (name, error) in refused {
            assert_eq!(BranchName::new(name), Err(error), "{name:?}");
        }
    }

    #[test]
    fn name_errors_state_the_rule() {
        let rule = BranchName::new("Aria").err().map(|e| e.to_string()).unwrap_or_default();
        assert!(rule.contains("lowercase"), "{rule}");
        let long = BranchName::new("a".repeat(65)).err().map(|e| e.to_string()).unwrap_or_default();
        assert!(long.contains(&MAX_BRANCH_NAME_LEN.to_string()), "{long}");
        let segment = BranchName::new("a//b").err().map(|e| e.to_string()).unwrap_or_default();
        assert!(segment.contains("segment"), "{segment}");
    }

    #[test]
    fn a_name_is_measured_in_characters_not_bytes() {
        // 40 characters are 80 bytes: inside the limit, so what fails is the alphabet, not the length.
        let narrow = "é".repeat(40);
        assert_eq!(BranchName::new(narrow.clone()), Err(BranchNameError::BadSegment(narrow)));
        // 65 characters are too long, and the error counts 65, not 130.
        assert_eq!(BranchName::new("é".repeat(65)), Err(BranchNameError::TooLong(65)));
        // 64 characters of which one is two bytes: the limit is met, the alphabet is not.
        let edge = format!("{}é", "a".repeat(MAX_BRANCH_NAME_LEN - 1));
        assert!(matches!(BranchName::new(edge), Err(BranchNameError::BadSegment(_))));
    }

    #[test]
    fn a_name_round_trips_and_a_bad_one_is_refused_on_read() {
        let name = BranchName::new("aria/oval-face").unwrap_or_else(|_| BranchName::main());
        assert_eq!(round_trips(&name), json!("aria/oval-face"));
        assert!(serde_json::from_value::<BranchName>(json!("Aria")).is_err());
    }

    #[test]
    fn ids_versions_and_kinds_round_trip() {
        let id = pid("p");
        assert_eq!(round_trips(&id), json!(id.to_string()));
        assert_eq!(id.to_string().parse::<PointId>(), Ok(id));
        let version = Version::new(3, id);
        assert_eq!(round_trips(&version), json!({"seq": 3, "point": id.to_string()}));
        for (kind, text) in [
            (PointKind::Fork, "fork"),
            (PointKind::Checkpoint, "checkpoint"),
            (PointKind::Ready, "ready"),
            (PointKind::Switch, "switch"),
            (PointKind::Close, "close"),
            (PointKind::Restore, "restore"),
            (PointKind::Import, "import"),
        ] {
            assert_eq!(round_trips(&kind), json!(text));
        }
    }

    #[test]
    fn contains_joins_to_the_newer_version_per_branch() {
        let (a, b, c) = (hex_id(1), hex_id(2), hex_id(3));
        let left =
            Contains::new().with(a.clone(), Version::new(5, pid("a5"))).with(b.clone(), Version::new(2, pid("b2")));
        let right = Contains::new()
            .with(a.clone(), Version::new(3, pid("a3")))
            .with(b.clone(), Version::new(7, pid("b7")))
            .with(c.clone(), Version::new(1, pid("c1")));
        let joined = left.join(&right);
        assert_eq!(joined.get(&a), Some(&Version::new(5, pid("a5"))));
        assert_eq!(joined.get(&b), Some(&Version::new(7, pid("b7"))));
        assert_eq!(joined.get(&c), Some(&Version::new(1, pid("c1"))));
        assert_eq!(joined, right.join(&left), "commutative");
        assert_eq!(joined.join(&joined), joined, "idempotent");
        assert_eq!(left.join(&Contains::new()), left);
        assert!(Contains::new().is_empty() && !joined.is_empty());
        assert_eq!(joined.iter().count(), 3);
    }

    #[test]
    fn with_never_replaces_a_newer_version() {
        let a = hex_id(1);
        let contains =
            Contains::new().with(a.clone(), Version::new(5, pid("a5"))).with(a.clone(), Version::new(4, pid("a4")));
        assert_eq!(contains.get(&a), Some(&Version::new(5, pid("a5"))));
    }

    #[test]
    fn includes_asks_whether_a_version_or_a_newer_one_is_held() {
        let a = hex_id(1);
        let contains = Contains::new().with(a.clone(), Version::new(5, pid("a5")));
        assert!(contains.includes(&a, &Version::new(5, pid("a5"))));
        assert!(contains.includes(&a, &Version::new(4, pid("a4"))));
        assert!(!contains.includes(&a, &Version::new(6, pid("a6"))));
        assert!(!contains.includes(&hex_id(2), &Version::new(1, pid("b1"))));
    }

    #[test]
    fn contains_round_trips_as_a_map_of_branch_to_version() {
        let contains = Contains::new().with(BranchId::main(), Version::new(12, pid("m12")));
        assert_eq!(round_trips(&contains), json!({"main": {"seq": 12, "point": pid("m12").to_string()}}));
    }

    #[test]
    fn a_point_round_trips_with_the_fields_of_design_4_2_in_order() {
        let p = point();
        round_trips(&p);
        let text = p.canonical_json().unwrap_or_default();
        let keys = [
            "format", "branch", "seq", "snapshot", "lsn", "parents", "contains", "kind", "label", "by", "at", "history",
        ];
        let positions: Vec<usize> = keys.iter().filter_map(|k| text.find(&format!("\"{k}\":"))).collect();
        assert_eq!(positions.len(), keys.len(), "{text}");
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "fields out of order: {text}");
        let bare = Point::new(BranchId::main(), 1, p.snapshot, Lsn(0), PointKind::Fork, Actor::System, WallTime(0));
        let compact = bare.canonical_json().unwrap_or_default();
        assert!(!compact.contains(' ') && !compact.contains('\n'), "no whitespace: {compact}");
        assert!(!bare.canonical_json().unwrap_or_default().contains("history"), "an absent history is left out");
        round_trips(&bare);
    }

    #[test]
    fn a_point_id_is_equal_for_equal_content_and_differs_when_any_field_changes() {
        let base = point();
        let id = base.id();
        assert!(id.is_ok());
        assert_eq!(point().id(), id, "equal content, equal id");
        assert_eq!(
            id.as_ref().ok().map(|i| i.0),
            Some(ContentHash::of(base.canonical_json().unwrap_or_default().as_bytes()))
        );
        let changes: Vec<(&str, Edit)> = vec![
            ("format", Box::new(|p| p.format += 1)),
            ("branch", Box::new(|p| p.branch = BranchId::main())),
            ("seq", Box::new(|p| p.seq += 1)),
            ("snapshot", Box::new(|p| p.snapshot = SnapshotId(ContentHash::of(b"other")))),
            ("lsn", Box::new(|p| p.lsn = Lsn(1833))),
            ("parents", Box::new(|p| p.parents.push(pid("extra")))),
            ("contains", Box::new(|p| p.contains = Contains::new())),
            ("kind", Box::new(|p| p.kind = PointKind::Checkpoint)),
            ("label", Box::new(|p| p.label.push('!'))),
            ("by", Box::new(|p| p.by = Actor::System)),
            ("at", Box::new(|p| p.at = WallTime(p.at.0 + 1))),
            ("history", Box::new(|p| p.history = None)),
        ];
        for (field, change) in changes {
            let mut edited = point();
            change(&mut edited);
            assert_ne!(edited.id(), id, "changing {field} must change the id");
        }
    }

    #[test]
    fn lane_states_reviews_and_decisions_round_trip() {
        assert_eq!(round_trips(&LaneState::Closed), json!("closed"));
        assert_eq!(round_trips(&LaneState::open(true, 2)), json!({"open": {"folder": true, "sessions": 2}}));
        assert_eq!(LaneState::default(), LaneState::Closed);
        for (state, text) in
            [(ReviewState::Ready, "ready"), (ReviewState::Merged, "merged"), (ReviewState::SentBack, "sent-back")]
        {
            assert_eq!(round_trips(&state), json!(text));
        }
        let review = Review::new(pid("head"), agent(), WallTime(6), Some("note".to_owned()));
        assert_eq!(review.state, ReviewState::Ready);
        round_trips(&review);
        assert_eq!(round_trips(&ReviewDecision::MarkMerged), json!("mark_merged"));
        assert_eq!(round_trips(&ReviewDecision::send_back("rounder")), json!({"send_back": {"comment": "rounder"}}));
    }

    #[test]
    fn branch_info_round_trips_and_reads_a_ref_without_the_optional_facts() {
        let full = info();
        round_trips(&full);
        let mut json = serde_json::to_value(&full).unwrap_or_default();
        let optional = ["title", "from", "inherits", "focus", "auto", "owner_session", "deleted", "review", "lane"];
        if let Some(map) = json.as_object_mut() {
            for key in optional {
                map.remove(key);
            }
        }
        let back: Result<BranchInfo, _> = serde_json::from_value(json);
        let bare = BranchInfo::new(
            full.id.clone(),
            full.name.clone(),
            full.head,
            full.seq,
            full.created,
            full.created_by.clone(),
        );
        assert_eq!(back.ok(), Some(bare));
    }

    #[test]
    fn requests_and_results_round_trip() {
        let name = BranchName::new("aria/oval-face").unwrap_or_else(|_| BranchName::main());
        let new = NewBranch::new(name, BranchId::main())
            .with_title("Oval face")
            .with_focus(vec!["characters/aria".to_owned()])
            .at(StateRef::tx(omuzaki_ops_api::TxId(7)))
            .for_session(SessionId(12));
        let json = round_trips(&new);
        assert_eq!(json["at"], json!({"tx": 7}));
        let bare = NewBranch::new(new.name.clone(), BranchId::main());
        assert_eq!(bare.at, StateRef::Now);
        round_trips(&bare);
        round_trips(&Switched::new(BranchId::main(), hex_id(9)));
        round_trips(&Restored::new(info(), StateRef::point(pid("old"))));
    }

    #[test]
    fn every_event_round_trips_with_its_tag() {
        let name = BranchName::new("aria/oval-face").unwrap_or_else(|_| BranchName::main());
        let events = [
            (BranchEvent::created(info()), "created"),
            (BranchEvent::checkpointed(hex_id(9), pid("p"), true), "checkpointed"),
            (BranchEvent::review_requested(hex_id(9), pid("p")), "review_requested"),
            (BranchEvent::reviewed(hex_id(9), ReviewState::SentBack), "reviewed"),
            (BranchEvent::switched(BranchId::main(), hex_id(9)), "switched"),
            (BranchEvent::renamed(hex_id(9), name), "renamed"),
            (BranchEvent::deleted(hex_id(9)), "deleted"),
            (BranchEvent::undeleted(hex_id(9)), "undeleted"),
            (BranchEvent::lane_opened(hex_id(9)), "lane_opened"),
            (BranchEvent::lane_closed(hex_id(9)), "lane_closed"),
            (BranchEvent::lane_recovered(hex_id(9), "recovered 3 records"), "lane_recovered"),
        ];
        for (event, tag) in events {
            assert_eq!(round_trips(&event)["event"], json!(tag));
        }
    }

    #[test]
    fn errors_name_what_failed_and_hint_at_the_fix() {
        let name = BranchName::new("aria").unwrap_or_else(|_| BranchName::main());
        let limit = BranchError::LaneLimit;
        assert!(limit.to_string().contains(&MAX_OPEN_LANES.to_string()), "{limit}");
        assert!(BranchError::NameTaken(name).to_string().contains("aria"));
        let invalid = BranchError::from(BranchNameError::Empty);
        assert!(matches!(invalid, BranchError::InvalidName(_)));
        assert!(invalid.hint().is_some_and(|h| h.contains(&MAX_BRANCH_NAME_LEN.to_string())));
        assert!(BranchError::Busy("open transaction".to_owned()).hint().is_some());
        assert!(BranchError::NotFound("x".to_owned()).hint().is_some());
        assert!(BranchError::Io("disk".to_owned()).hint().is_none());
    }
}
