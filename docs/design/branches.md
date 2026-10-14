# Branches: storage, switching, side-by-side compare and part-level merge (VC-02)

**Status:** accepted (2026-10-12). Additions to `omuzaki-history-api` and `omuzaki-ops-api`, a new
App crate.
**Feature:** VC-02; the compare half is shared with VC-03.
**Decision record:** ADR 0025. **Spec:** v3 §6 (ops, transactions), §7 (project layout), §12 (agents, no silent
overwrites), §13 (TH-05 branch exploration, TH-07 rewind), §18 (CB-13 per-use variants, CB-91 part variants, CB-93
explore mode), §29 (VC-01 to VC-04).
**Builds on:** ADR 0021 and `docs/design/journal.md` (the journal, snapshots, the keeper, recovery); ADR 0020 and
`docs/design/agent-sessions.md` (sessions, the stale-write guard, approvals); ADR 0019 and `docs/design/library.md`
(application records under `characters/<id>/applied/`); `docs/design/ux/shell.md` section 9 and
`docs/design/ux/principles.md` P3 (restore never destroys). Settles the branch half of VC-01 (restore).

This page explains the decisions so an implementer or a reviewer can disagree with a reason. The types it sketches are
finalised in their own tasks (section 15) and documented in the code.

**Decisions (2026-10-12):** one executor per open branch (isolation over one central executor), with the
number of open lanes capped and their memory cost stated (5.1, 11); `merges.toml` in the project folder, where git
sees it, as document history (7.3, 10); the new App crate `omuzaki-host`, registered as an App (its fence and
`cargo xtask lint-deps`).

## 0. The decisions in brief

1. **A branch is a named head over the project's history,** stored as a ref in `.omuzaki/branches/<id>/` that points at
   a **point**: an immutable, content-addressed record in the snapshot store that names a snapshot, the point before
   it and what the state includes of other branches. Branches cover the **whole project**. "Branch a character" is the
   same branch with a **focus** on that character, which the UI, compare and the default merge scope use.
2. **One lane per open branch.** An open branch runs in its own **lane**: an executor, a journal and a keeper, exactly
   as ADR 0021 describes for the project, each branch with its own journal folder. Lanes never call each other while
   holding a lock. An agent can work on its own branch in its own lane while the person edits another one. At most
   8 lanes are open at once (configurable); each costs about twice the document's size in memory and three threads.
3. **The folder shows one branch.** Switching re-points the project folder (and the person's window) at another lane:
   its keeper rewrites the TOML files that differ. No document transaction happens, each branch keeps its own undo
   history, and open transactions stay where they are: the person's own block the switch, agents' carry on in their
   lane. A two-phase marker makes the switch crash-safe.
4. **Ops see their own lane live and every other branch through points.** Compare and merge read other branches as
   immutable points, so a review always shows exactly what gets merged.
5. **Compare** reuses VC-03: a **state reference** (`now`, a transaction, a snapshot, a point or a branch) names either
   side of the parameter diff (grouped by **unit**: a part, the face, a library application...) and of the renders at a
   matched camera.
6. **Merge** is a three-way merge of document trees, unit by unit, as **one undoable transaction** (`branch.merge`).
   Changes made on one side merge cleanly; changes to the same value on both sides are conflicts that are listed per
   unit and never resolved silently. The person keeps theirs or mine per unit, or per parameter, or leaves a unit out.
7. **The merge base comes from history, and its bookkeeping lives in the tree.** Each point records which points of
   other branches it includes (a version vector). A merge writes that record into `merges.toml` in the same
   transaction, so undoing, restoring past or recovering from a merge also forgets it, and a later merge offers the
   branch's changes again instead of dropping them (git's "reverted merge" trap).
8. **Library records merge as units** (ADR 0019): an application record moves together with the parts it created and
   their source rows, and a record never disappears while anything it wrote remains, so rights never drop.
9. **Agents work on their own branch, the person merges.** A session opens on a branch's lane. An agent can create a
   branch, move its session there, merge others into it, and mark a point ready for review; merging into the branch the
   person has open asks the person (ADR 0020's approval queue, with the merge preview).
10. **Git is separate.** Omuzaki branches are local and live in `.omuzaki/`; the folder holds the current branch's text
    files, which git sees and commits as usual. VC-04 can import a git commit as a point and reuse compare and merge.

## 1. What has to be true

| Requirement | Source | Section |
|---|---|---|
| Branch a character or the project | VC-02 | 3, 5.4 |
| Compare branches side by side | VC-02, TH-05, CO-09 | 6 |
| Merge part-level changes | VC-02, CB-91 | 7 |
| Before/after renders and a parameter diff for any two points | VC-03 | 6 |
| Every transaction journalled; nothing lost in a crash; recovery restores the last transaction | VC-08, ADR 0021 | 4.4, 5.2, 5.5 |
| Undo is per actor and never undoes another branch's work | §6, ADR 0020 | 5.5 |
| Agents never overwrite blind; the person stays in charge | §12, ADR 0020 | 8 |
| "Branch from here" in History, the conflict card and the Theatre's rewind | shell.md §9 and §11, TH-07 | 5.4 |
| Restoring an old point keeps the abandoned future as a branch | principles P3 | 9 |
| Rights never drop when library output is merged | ADR 0019 D8 | 7.6 |
| The text project format commits cleanly; git stays optional | §7, VC-04 | 10 |
| Agents explore variants in parallel; the person picks one, the others stay as branches | TH-05, CB-93 (phase 3) | 5, 8 |

## 2. Words used here

| Word | Meaning |
|---|---|
| **Branch** | A named line of the project's history, with a stable id and a head point |
| **Point** | One immutable state of a branch: a snapshot plus the facts merging needs. Like a commit |
| **Lane** | A branch that is open for editing: its executor, journal writer, keeper and the sessions on it |
| **Folder branch** | The branch whose lane writes the project folder's files; the one the person's window shows |
| **Unit** | The piece of the document that merge and diff group by: a part, the face, a library application, a scene... |
| **State reference** | A name for a document state: `now`, a transaction, a snapshot, a point or a branch |

## 3. What a branch is

### 3.1 Whole project, with a focus

A branch covers the whole project document. A branch made "for a character" carries `focus = ["characters/<id>"]`,
which:
- labels it in the branch menu ("Aria: oval face");
- frames that character in compare views;
- pre-fills the merge scope, so merging it back takes only that character's units unless the person widens it;
- becomes the default fence of an agent session working on it, once fences exist (AG-04).

Edits outside the focus are allowed and show in compare and merge with a note ("this branch also changed the opening
scene").

**Why not a per-character scope:** references cross characters (scenes place characters, shows use scenes, library
records live under each character), snapshots are whole-folder manifests, and a state made of different branches per
character is a mix nobody can name or restore. The three-way merge already leaves other characters alone when only one
changed (section 7), so a focus plus a scoped merge gives the per-character experience without a second kind of branch.

### 3.2 Branches, variants and takes

| Need | Use | Where it lives |
|---|---|---|
| The same character in two uses at once (close-up and game face) | a per-use variant (CB-13) | inside the document, `variants/<name>.toml`, switched by shot distance |
| Hair A, B and C kept side by side on one character | part variants (CB-91): disabled alternative parts | inside the document (`enabled = false`) |
| Try a direction without touching what works; let an agent work apart; compare whole alternatives | a branch | `.omuzaki/branches/` and its lane |
| "Show me six takes" (CB-93), "oval face A/B/C" (TH-05) | one branch per take, side by side | branches; the kept take is merged |

A branch's part can become a part variant when merging: "Keep both" takes theirs as a disabled alternative part
(section 7.4), which is the bridge from branches to CB-91.

## 4. Storage

### 4.1 Layout

```
my-show.omuzaki/
  merges.toml                          what this branch has merged from others (7.3); a normal project file
  .omuzaki/
    journal/  log/                     the journal and archive of branch `main` (ADR 0021, unchanged)
    snapshots/                         one snapshot store for every branch: snapshots, packs, history blobs, points
    branches/
      folder.toml                      which branch the folder shows, and the switch marker (5.5)
      main/ref.toml                    the ref of `main`
      9c41d07a5e2b1f30/ref.toml        the ref of another branch, by id
      9c41d07a5e2b1f30/journal/ log/   that branch's journal and archive (same format as ADR 0021)
```

A project that never branches has no `branches/` folder (it appears with the first branch, when `main` gets its
ref) and behaves exactly as ADR 0021 describes. `.omuzaki/` is never committed and travels with a copied folder,
branches included.

### 4.2 Points

A point is one object in the snapshot store (`.omuzaki/snapshots/`, a `DiskStore`), canonical JSON (fields in this
order, no whitespace); its BLAKE3 hash is the **point id**:

```json
{"format":1,"branch":"9c41d07a5e2b1f30","seq":4,"snapshot":"…","lsn":1832,
 "parents":["…"],"contains":{"main":{"seq":12,"point":"…"}},
 "kind":"ready","label":"Oval face, version A","by":{"agent":{"id":"claude-code#12"}},
 "at":"2026-10-12T14:32:05Z","history":"…"}
```

| Field | Meaning |
|---|---|
| `branch`, `seq` | The branch and the point's number on it: 1, 2, 3... per branch, never reused |
| `snapshot` | The snapshot id (ADR 0021 section 7.1) holding the state's files |
| `lsn` | Where in the branch's journal the state was taken (informational) |
| `parents` | The branch's previous point, plus any points newly merged in since it: for the graph view only |
| `contains` | For every other branch, the newest of its points this state includes (7.3). What merging uses |
| `kind` | `fork`, `checkpoint`, `ready`, `switch`, `close`, `restore` or `import` (VC-04) |
| `label`, `by`, `at` | What, who, when |
| `history` | Optional: the executor's history export (undo and redo stacks) at that state, when no transaction was open. Local only, like a snapshot's |

A point's snapshot is taken with a new trigger, `point`, and its ref names the branch (`branch = "<id>"`; absent means
`main`).

**When points are made.** Points are few: one per fork, explicit checkpoint, review request, switch-away and lane
close. They are not made per transaction; the journal already holds every transaction. Each is made by the keeper of
the branch's lane (5.3), so a point is always exactly a state its journal reached.

### 4.3 Refs and the folder marker

`.omuzaki/branches/<id>/ref.toml`, written atomically, and only after the point it names is in the store:

```toml
id = "9c41d07a5e2b1f30"
name = "designer/oval-face"          # 4.7
title = "Oval face, version A"        # optional display title
head = "…"                           # the newest point
seq = 4                              # the head's seq
from = "…"                           # the fork point (on the source branch); absent for main
inherits = { main = { seq = 12, point = "…" } }   # what the branch started with: the fork point and what it contained
focus = ["characters/aria"]
created = 2026-10-12T14:30:00Z
created_by = { agent = { id = "claude-code#12" } }
auto = false                         # made by Omuzaki (restore, 9), not by someone
owner_session = 12                   # made for this agent session (8)
# deleted = 2026-10-13T09:00:00Z     # in the bin until purged (5.6)
[review]                             # the latest request for review, if any (8.3)
point = "…"
by = { agent = { id = "claude-code#12" } }
at = 2026-10-12T14:40:00Z
note = "Chin rounder, jaw narrower; likeness +0.04"
state = "ready"                      # ready | merged | sent-back
```

`.omuzaki/branches/folder.toml` names the folder branch: `branch = "main"`, `state = "ok"`. During a switch it reads
`state = "switching"` with `from` and `to` (5.5). Absent means `main`, `ok`.

### 4.4 Each branch has its own journal (an amendment to ADR 0021)

ADR 0021 gives a project one journal. With branches, **each branch has one**, in the same format: `main`'s stays at
`.omuzaki/journal/` and `.omuzaki/log/`, any other's lives at `.omuzaki/branches/<id>/journal/` and `log/`. Its LSNs are
numbered within that branch. Everything ADR 0021 builds (the writer, the keeper, snapshots, recovery, the history
list) takes the **branch root** as a parameter instead of assuming `.omuzaki/`; only the folder branch's keeper saves
the TOML files. Record kind 10 (reserved by ADR 0021 for a switch that replaces history stacks) stays reserved and
unused: a switch changes no lane's history.

### 4.5 Files outside the tree are shared

Branches version the **document tree**. Project files outside it (`assets.lock`, `performances/*/takes/*.track`) are
shared by all branches: they are content-addressed or append-only records that a branch either references from its
tree or doesn't, so an unreferenced take on one branch costs nothing. A switch never rewrites them. Snapshots still
include them (ADR 0021), so a backup restore stays exact. **Rule for later features:** anything that must differ per
branch belongs in the tree.

### 4.6 Retention, garbage collection, backups

- A point keeps its snapshot: retention never deletes a snapshot named by a point that a live branch reaches
  (its head, then parents, recursively), nor one reached from a branch in the bin.
- Garbage collection (the store's sweep) counts as roots the snapshot refs, every point reachable from a live or binned
  branch ref, and what those points name (snapshot, history blob).
- A second backup location copies branch refs and their reachable points with the snapshots; points' history
  blobs follow the same opt-in as snapshots' (they hold prompts).
- Settings → Storage shows the disk use per branch (its journal and archive).

### 4.7 Names

- **Ids** are stable: `main` for the first branch, 16 random hex characters for others. Folders and every reference
  use the id, so a rename touches one file.
- **Names** are unique in the project (case-insensitive): lowercase slug segments (`[a-z0-9][a-z0-9-]*`) joined by
  `/`, at most 64 characters, for example `aria/oval-face` or `designer/oval-a`. `main` names only the first branch.
  An optional free-text `title` is what the UI shows.
- **Automatic names:** `restore/<yyyy-mm-dd-hhmm>` for an abandoned future (9), titled "Before restore 14:32";
  `<client>/<topic>` for a branch an agent creates (`claude-code/oval-face`, deduplicated with `-2`).

## 5. Lanes

### 5.1 The shape

```
 person's window ─────────────────────────┐                    ┌──── agent session on oval-face (MCP)
 (shows the folder branch)                ▼                    ▼
              Branches (omuzaki-host: the lane manager; never inside an executor's lock)
                │                                  │
     lane: main (folder)                 lane: oval-face (side)
     Executor + sessions                 Executor + sessions
     journal .omuzaki/journal/             journal .omuzaki/branches/9c41…/journal/
     keeper → saves the TOML files       keeper → snapshots and points only
                │                                  │
                └──────── one snapshot store (.omuzaki/snapshots/): snapshots, points ┘
```

- **A lane is ADR 0021's stack for one branch:** an `Executor` (with its session registry), a `ProjectJournal` on the
  branch root, and a keeper whose shadow follows the journal. Only the folder lane's keeper saves files. Each lane has
  its own lock; a process holds at most 8 open lanes (configurable), closing idle ones first, and refuses to open a
  ninth with a reason. A lane costs about twice the document's size in memory (the executor's document and the
  keeper's shadow) plus three threads (journal writer, keeper, segment preparer).
- **The lane manager is `Branches`** (a trait in `omuzaki-history-api`, implemented in a new App crate, `omuzaki-host`,
  which both `omuzaki-cli` and `omuzaki-ui` link to open a project for writing). It opens and closes lanes, makes points,
  switches the folder, and hands surfaces the runner of a lane. It never holds one lane's lock while taking another's.
- **Ops never reach another lane.** An op runs inside its lane's lock and reads other branches only through points
  (immutable objects in the store), by the `BranchStore` service. That rule removes every cross-lane deadlock.
- **Sessions belong to a lane** (8.1). Session ids are allocated from one counter shared by the lanes of a project
  (a `omuzaki-ops` builder option), so they stay unique across branches. The Agent panel lists the sessions of every
  lane, each with its branch.

### 5.2 Opening and closing a lane

- **Open:** load the head point's snapshot (`Snapshotter::load`), restore the executor from the point's history export
  (or start empty when it has none), start the branch's journal with a session record whose base is that snapshot.
  If the branch's journal ended without `close`, recover it first (ADR 0021 section 8), headless rules: restore to the
  last committed transaction, roll open ones back, notice. At project open, every branch whose journal ended uncleanly
  is recovered in the background, and the branch menu shows the notice.
- **Close:** a side lane closes 60 s after it last had a session, a `Lane` handle (the Theatre's live view holds
  one) or the folder. Closing ends any sessions (ADR 0020: their open transactions roll back), makes a `close` point
  with the history export, writes the journal's `close` record and drops the executor.
- **The folder lane** opens at project open, from `folder.toml`, exactly as ADR 0021 opens the project; recovery with
  the "Restore to the last transaction" prompt applies to it alone, because only its files are in the folder.

### 5.3 Points from a lane: checkpoints

`Branches::checkpoint(branch, label, ready?)` makes a point of the branch's **committed** state now:
1. The lane's executor appends a `Mark` with its history export (`flush_history`), and the request is queued
   to the keeper with the Mark's LSN.
2. The keeper, at that LSN, renders the snapshot of the shadow **minus the open transactions** (their rollback patch,
   the same code recovery uses, ADR 0021 section 8.3). A point never contains half of someone's transaction. If a
   rollback patch doesn't apply (committed work by another actor is built on it), the checkpoint is refused, naming
   the transaction and its owner.
3. It writes the point (`seq` = head seq + 1, `contains` = the lane's `merges.toml` joined with the ref's `inherits`,
   `history` only when no transaction was open), reads it back, then updates the ref's `head`, and answers.

A checkpoint takes one off-thread snapshot (the snapshot budget: 50 ms for a 500-file project with 10 changed files). When
nothing changed since the head point, the head is returned and nothing is written.

### 5.4 Creating a branch

`Branches::create { name, title?, focus?, from: { branch, at: StateRef }, for_session? }`:
- **From now** (the default): a checkpoint of the source branch (5.3) is the fork point. The new branch's ref points at
  it: `from = head = fork point`, `inherits = fork.contains ⊔ {source: fork}`. Its undo history starts as a copy of the
  source's (the fork point's history export), so Ctrl+Z right after branching behaves the same on both sides; from
  then on they diverge.
- **From a history point** (History's "Branch from here", the undo conflict card, the Theatre's rewind, TH-07): the
  source lane's keeper makes a point from the tree at that state (`HistoryView::tree`, 6.1) by applying the tree diff to
  a clone of its shadow and snapshotting it. Its undo history starts empty.
- **From a point or a snapshot:** the same, from that tree.
- A branch can be created by a person or by an agent (8.2). Nothing in the document changes, so there is nothing to
  undo; deleting the branch is the way back.

### 5.5 Switching the folder

`Branches::switch(to, by)`, a person's action (UI, CLI, MCP for persons; agents move their own session instead, 8.1):
1. **Refuse** if the person has an open transaction on the folder lane ("finish or cancel 'Hair › bangs' first"), or
   if a file in the folder changed outside Omuzaki since the keeper last saved it (the external-change check: "reload or discard
   those changes first"; switching would overwrite them).
2. Open the target's lane if it isn't open (5.2).
3. Write `folder.toml` with `state = "switching"`, `from`, `to`, and flush it.
4. The old folder lane stops saving files and makes a `switch` point (with its history export) as its new head. If
   sessions are active on it, it stays open as a side lane; otherwise it closes (5.2).
5. The target lane's keeper becomes the folder keeper: it rewrites the folder to match its state exactly with
   the folder writer (only files whose bytes differ are replaced; tree files the target lacks are removed; files outside the tree
   are not touched, 4.5).
6. Write `folder.toml` with `state = "ok"`, `branch = to`. Emit `Switched`.

**What happens to journals and transactions:** nothing. Each lane keeps its own journal, undo history and open
transactions; the switch only changes which lane writes the folder and which one the person's window and Ctrl+Z
address. Agents' open transactions on the old branch stay open in its lane, and the confirmation names them ("Designer
is working on main and will keep working there. [Switch] [Pause Designer and switch]").

**Crash safety:** at open, `state = "switching"` means steps 5 and 6 are redone: recover both lanes if needed (the
target without the folder prompt, since the folder was being replaced), rewrite the folder from the target's state,
mark `ok`. Every step is idempotent, so a crash during the redo redoes it again. Without the marker, a crash after step
4 would make ADR 0021's "clean close: the folder is the truth" adopt the old branch's files as the new branch's state.

**Cost:** switching to an open lane is a rebind on the UI thread (the target's document is already in memory) and a
background rewrite of the differing files: 2 to 4 ms per file natively, 9 to 25 ms on drvfs (journal.md section 2).
Switching to a closed branch adds opening its lane (load a snapshot, restore the executor), off the UI thread with a
"Switching…" state.

### 5.6 Rename, delete and the bin

- **Rename** edits the ref's `name` and `title`; ids don't change.
- **Delete** puts the branch in the bin (`deleted` in its ref): refused for `main`, the folder branch and a branch with
  an open lane. The bin keeps it 30 days ("Restore branch"); purging removes its folder (journal and archive), and its
  points become collectable. A branch that is still `ready` for review asks first.
- Persons may rename and delete any branch; an agent only a branch made for its session.

### 5.7 Contracts (sketch; finalised)

In `omuzaki-history-api`:

```rust
pub struct BranchId(String);          // "main" or 16 hex
pub struct BranchName(String);        // validated as 4.7
pub struct PointId(ContentHash);
pub struct Version { pub seq: u64, pub point: PointId }
/// For each branch, the newest of its points a state includes. `join` takes the newer per branch.
pub struct Contains(BTreeMap<BranchId, Version>);
pub struct Point { /* the fields of 4.2 */ }
pub struct BranchInfo { /* the ref of 4.3, plus the lane's state: closed, or open (folder or side, sessions) */ }

/// A document state, for diff, compare, restore and branching (VC-02, VC-03). Transactions are those of the lane
/// the call runs on.
pub enum StateRef { Now, Tx { tx: TxId }, Snapshot { id: SnapshotId }, Point { id: PointId }, Branch { name: BranchName } }

/// What ops read (a `Service`): branches as refs and points, never another lane's live document.
pub trait BranchStore: Service {
    fn this(&self) -> Result<BranchInfo, BranchError>;        // the branch of the lane the op runs on
    fn list(&self) -> Result<Vec<BranchInfo>, BranchError>;
    fn branch(&self, name: &BranchName) -> Result<BranchInfo, BranchError>;
    fn point(&self, id: &PointId) -> Result<Point, BranchError>;
    fn tree(&self, id: &PointId) -> Result<Arc<Value>, BranchError>;   // cached
}

/// The tree of any state of this lane's branch (`Now` is the op's own document).
pub trait HistoryView: Service {
    fn tree(&self, at: &StateRef) -> Result<Arc<Value>, HistoryError>;
}

/// The merge base (7.3): pure, shared by the op and the mock.
pub fn merge_base(ours: &BranchId, ours_contains: &Contains, theirs: &Point,
                  points: &dyn Fn(&PointId) -> Option<Point>) -> Option<PointId>;

/// The lane manager (implemented by omuzaki-host). Surfaces call it; ops never do.
pub trait Branches: Send + Sync {
    fn list(&self) -> Vec<BranchInfo>;
    fn folder(&self) -> BranchId;
    fn create(&self, new: NewBranch, by: &Actor) -> Result<BranchInfo, BranchError>;
    fn checkpoint(&self, branch: &BranchId, label: &str, ready: Option<String>, by: &Actor) -> Result<PointId, BranchError>;
    fn switch(&self, to: &BranchId, by: &Actor) -> Result<Switched, BranchError>;
    fn rename(&self, branch: &BranchId, name: BranchName, title: Option<String>, by: &Actor) -> Result<BranchInfo, BranchError>;
    fn delete(&self, branch: &BranchId, by: &Actor) -> Result<(), BranchError>;
    fn undelete(&self, branch: &BranchId, by: &Actor) -> Result<BranchInfo, BranchError>;
    fn review(&self, branch: &BranchId, decision: ReviewDecision, by: &Actor) -> Result<BranchInfo, BranchError>;
    fn restore(&self, branch: &BranchId, to: StateRef, by: &Actor, grants: &Grants) -> Result<Restored, BranchError>;
    fn lane(&self, branch: &BranchId) -> Result<Lane, BranchError>;   // opens it: { runner, sessions }
    fn subscribe(&self) -> Receiver<BranchEvent>;
}
```

`BranchEvent`: `Created`, `Checkpointed { point, ready }`, `ReviewRequested`, `Reviewed`, `Switched { from, to }`,
`Renamed`, `Deleted`, `Undeleted`, `LaneOpened`, `LaneClosed`, `LaneRecovered { notice }`. `SnapshotInfo` gains
`branch` and `Trigger` gains `Point` (serde defaults); `Snapshots` gains `take_at(&StateRef, trigger, name)`, which
snapshots any state (named versions of past transactions, 9).

In `omuzaki-ops-api`, one addition: `OpCtx::require_scope(Scope) -> Result<(), OpError>`, which passes in a dry-run
(dry-runs need only `read`) and otherwise checks the request's effective grants without revealing them, refusing
with `Denied` naming the scope. A merge needs it because what it changes depends on the data (7.7).

## 6. Compare

### 6.1 State references

VC-03 compares "any two points"; VC-02 compares branches. Both use `StateRef`:

| Reference | Resolves to |
|---|---|
| `now` | the lane's live document (the op's own) |
| `{tx}` | the state just after that committed transaction of this lane's branch, in this session or an earlier one |
| `{snapshot}` | that snapshot's tree |
| `{point}` | that point's snapshot's tree |
| `{branch}` | the branch's head point; for the op's own branch, `now` |

`HistoryView` (implemented in `omuzaki-doc`) resolves `{tx}` from the branch's journal and archive: the newest snapshot
at or before the transaction's commit LSN, plus the journal records up to it (ADR 0021 section 9, "any point in the
archive can be rebuilt"), after a `sync` so the newest commits are on disk. This replaces the in-memory inverse
patches, which could reach only the current session's capped log.

A branch whose lane is open may be ahead of its head point. The op's output always names the point it used and its
time; the UI, which knows the lane, shows "as of 14:02 · Refresh", and Refresh checkpoints the branch (5.3) and
compares again. An agent can do the same with `branch_checkpoint`.

### 6.2 The parameter diff

`history.diff {from: StateRef, to: StateRef, scope?: [path]}` returns the changes grouped by **unit** (7.1),
with before and after values, maps compared by key and arrays as leaves. `merges.toml` is bookkeeping and is left
out. The same function serves the History panel's compare, the branch compare, the merge preview and the backup restore
preview (VC-09).

### 6.3 Renders at a matched camera

`render.compare {a: StateRef, b: StateRef, camera?, mode}` and `render.preview {state: StateRef, ...}` (over the character builder): each state is built through the caching builder from a `Scratch` of its tree, never touching the live document,
and rendered with the same camera recipe, framing, pose, expression and light rig. The default framing is the branch's
focus character. A character missing on one side renders as an empty frame with its name.

### 6.4 The compare view

From the branch menu ("Compare…") or by selecting two branches: 2 to 6 columns, each a branch (or `now`, or a point),
rendered at one matched camera with a shared orbit; under them, the parameter diff of each column against the chosen
reference column, grouped by unit, each row clickable to select the part. Branches whose lane is ahead show Refresh.
Each column has **Switch to**, **Merge into current** and **Delete**. TH-05's live side-by-side is the same view with
each column bound to an open lane's live document instead of a point (phase 3).

## 7. Merge

### 7.1 Units

A unit is what conflicts are shown by and what "keep mine" or "take theirs" applies to as a whole. `units.rs` in
`omuzaki-ops` (written for `history.diff`, used by the merge) maps every path to a unit:

| Paths | Unit (its key is the path in the left column's first entry) |
|---|---|
| `characters/<c>/parts/<p>/**`, `characters/<c>/sources/<p>`, `characters/<c>/weight_brushes/<p>` | the part |
| `characters/<c>/applied/<a>`, plus every path its `created` lists on either side and those parts' source rows | the library application (7.6) |
| `characters/<c>/face/**` | the face |
| `characters/<c>/palette/**`, `characters/<c>/skin_tone` | the palette |
| `characters/<c>/rig/**`, `characters/<c>/look/**` | the rig; the look |
| `characters/<c>/variants/<v>`, `characters/<c>/colourways/<k>`, `characters/<c>/poses/<x>`, `characters/<c>/emotes/<x>` | that variant, colourway, pose or emote |
| `characters/<c>/licence` | the character's licence record |
| `characters/<c>/{name,template,seed}` | the character's basics |
| `characters/<c>` added or removed as a whole | the character |
| `scenes/<s>/**` | the scene (ST-01 may split it into stage, cameras, lights and props) |
| `performances/<p>/**`, `shows/<s>/**` | that performance; that show |
| `project/**` | project settings |
| `merges` | not a unit (7.3) |
| anything else | its first two path segments |

### 7.2 The three-way merge

`merge3(base, ours, theirs)` in `omuzaki-doc-api` (next to `diff`): pure, deterministic, over `Value` trees.
Objects merge key by key; arrays and scalars are leaves (as in the library's update, ADR 0019 D10). Per path:

| Base | Mine | Theirs | Result |
|---|---|---|---|
| x | x | y | y (they changed it) |
| x | y | x | y (I changed it) |
| x | y | y | y (same change) |
| x | y | z | **conflict** |
| absent | absent | y | y (they added it) |
| absent | y | z | y if y = z, else **conflict** (both added) |
| x | absent | x | absent (I removed it) |
| x | absent | y | **conflict** (I removed what they changed) |
| x | y | absent | **conflict** (they removed what I changed) |

A subtree removed on one side and changed anywhere on the other is one conflict at the subtree's root. With **no base**
(7.3), every difference is a conflict, shown with "no common version: each difference is a choice".

### 7.3 The base, and why its bookkeeping lives in the tree

**The problem.** git finds the base from the commit graph. If a merge is later reverted, the graph still says the
branch was merged, so merging it again silently drops its changes. In Omuzaki, a merge can be undone, undone as part of
an agent's session (ADR 0020), restored past (VC-01), restored from a backup (VC-09) or rolled back by recovery. The
base must forget the merge in every one of those cases, and they all work by patches.

**The answer.** Each state records which points of other branches it includes, as a version vector:
- a point stores `contains` (4.2);
- a branch's ref stores `inherits`: what it started with (the fork point and what that point contained);
- the document holds `merges.toml`: for each branch merged in, the newest point merged, with its seq and name. A full
  merge rewrites it **in the same transaction** as the merged values, so every way of reverting the merge (undo,
  undo-session, restore, backup restore, recovery) reverts the record with it, and nothing else needs a special case.
  This is the argument ADR 0019 used for keeping application records in the tree.

```toml
# Written by Omuzaki when another branch is merged in: the newest version of it this branch includes.
# It makes the next merge start where this one ended. Deleting it is safe: the next merge asks about more.
format = 1
[9c41d07a5e2b1f30]
name = "designer/oval-face"
seq = 4
point = "…"
```

(`format` keeps it one file: by `omuzaki-doc`'s layout rules a new object holding only tables would become a folder.)

**Finding the base** of merging point `T` (branch `t`) into the live document of branch `o`:
1. Ours' vector `O` = `merges.toml` ⊔ the ref's `inherits`, with `o` itself counted as "everything". Theirs' vector
   `V` = `T.contains` with `t` at `T.seq`.
2. **Candidates:** for every branch `b` both vectors name, the point of `b` at `min(O[b], V[b])`. Each is included in
   both states, so any of them is a safe base: a difference from it is a real change on that side.
3. Drop candidates whose point object is missing (collected, or the project was copied without `.omuzaki/`).
4. Pick the candidate that includes all the others (`c ⊒ d` when `d` is on `c`'s branch at a lower or equal seq, or
   `c.contains[d.branch].seq ≥ d.seq`). If none does (both branches merged each other at once), take the newest by
   time. No candidate: no base (7.2).

A full merge then sets ours' record to `merges.toml ⊔ V`, without `o` itself.

**Why it is safe.** A base can hide theirs' changes only if it claims ours received something it never did. Every
candidate is something both sides received: ours' own points, what ours inherited at its fork, and what ours merged
while the record of that merge is still in the document; on theirs' side, the same through its point's `contains`.
Content received and later removed (by hand, or by restoring to a state without it) is a change on that side, as in
any three-way merge, and the preview shows it. Undoing a merge, or restoring to before it, removes its record too, so
the next merge offers the branch's changes again. A candidate is therefore never too new; the worst an old one does is
add a conflict, never lose a change silently. The property test (12.2) checks exactly that.

### 7.4 Resolutions, picks and full merges

Per unit, the result is one of:
- **auto:** the three-way result inside the unit; the default when the unit has no conflict;
- **mine** or **theirs:** that side's whole unit;
- **both** (parts only): ours stays, and theirs is added as a disabled alternative part under a new id
  (`<id>-<branch>`), with its source row and weight brushes: a CB-91 part variant;
- **per parameter:** the unit's conflicting paths each set to mine or theirs, the rest auto;
- **left out:** ours stays as it is.

A merge with no unit left out is **full**: it updates `merges.toml`. A merge that leaves any changed unit out is a
**pick**: it applies the chosen units and records nothing, so the next merge offers the rest again. "Keep mine" on a
conflict is a decision and counts as included; "left out" means "not now".

### 7.5 Validation

The result must still build. For every character the merge changes, the op checks the result with
`CharacterDef::from_node`, the part tree (parents exist, no cycles) and each changed part's kind `check`
(`PartKindRegistration`). A failure is reported on the units involved (the part, and its parent's unit for a missing
parent) as **invalid**, which, like a conflict, must be resolved by choosing a side. The merge view also renders the
result before it is applied (6.3), so a part that validates but builds badly is seen first.

### 7.6 Library application records (ADR 0019)

- An application unit holds the record, the parts it created (from both sides' `created`) and their source rows, so a
  hairstyle item merges as one thing.
- The record's pin (`item`, `version`, `hash`, `publisher`, `deps`, `rights`, `created`, `slot`) is atomic: if both
  sides changed it differently (updated to different versions, or replaced the slot with different items), the unit
  is a conflict and takes one side whole. Other differences (params, `wrote`) merge by the table.
- Two applications claiming the same slot after the merge make the result invalid (7.5), naming both.
- **Rights never drop.** If the result keeps any path an application created but not its record, the op keeps the
  record (from the side that has it) and says so. `licence.derive` and `io.check_export` therefore see
  every item whose output is in the merged character, and the summary warns when the merge brings in an item that
  blocks publishing.
- Item manifests and assets are already in the project's store, which all branches share (4.5).

### 7.7 The op

`branch.merge` (edit, scope `edit:character`, undo automatic):

```
args:   { from: BranchName,          // the branch to merge from
          at?: PointId,               // theirs; default: from's head point. The UI always passes the reviewed one
          scope?: [DocPath],          // only units under these paths (the UI pre-fills the focus)
          exclude?: [UnitKey],        // leave out: makes it a pick
          resolve?: { UnitKey: "mine" | "theirs" | "both" | { paths: { DocPath: "mine" | "theirs" } } },
          base?: StateRef,            // override the computed base (VC-04 imports; tests)
          expect?: DocVersion }       // refuse if this branch changed since the preview
output: { base: PointId | null, theirs: PointId, version: DocVersion, full: bool,
          units: [{ key, title, status: clean | conflict | invalid | excluded | unchanged,
                    taken: auto | mine | theirs | both | left-out,
                    changes: [{ path, base, mine, theirs, result }], note? }],
          rights: [notes], needs: [scopes], summary: "12 changes merge cleanly; 2 need a choice" }
```

- The dry-run is the merge preview: everything above, nothing applied.
- Apply refuses with `Conflict` (detail listing the units) while any conflict or invalid unit has no resolution, and
  with `Conflict` if `expect` no longer matches the branch's version ("this branch changed while you were reviewing;
  preview again").
- Units outside `characters/` also need `edit:scene` (`ctx.require_scope`), so a merge can't widen an agent's grants.
- **An agent merging into the folder branch asks the person** (`ctx.require_person`, ADR 0020 section 7): the request
  queues with the dry-run as its preview, and the person approves it in Omuzaki's window. Merging into any other branch,
  such as the agent's own, needs no person. The op learns which branch it runs on, and whether it is the folder branch,
  from `BranchStore::this`.
- The label names what was merged: "Merge oval-face into main (Aria: hair, face)".

### 7.8 The merge view (conflict presentation)

A full-window view opened from the branch menu, a compare column or a review card:
- a header: "Merge **oval-face** (version 4, 14:40) into **main**", the base ("since 14:02") and the summary;
- **Needs a choice:** one card per conflicting or invalid unit with mine and theirs rendered side by side at the
  compare camera (base on request), the conflicting parameters as rows with base, mine and theirs values, and the
  buttons **Keep mine**, **Take theirs**, **Keep both** (parts) and **Choose per parameter**. Nothing is pre-selected;
  the Merge button stays disabled until every card has a choice;
- **Merges cleanly:** the other changed units, collapsed, each with a checkbox (unticking makes the merge a pick, and
  the button says "Merge selected");
- rights notes and the result render, refreshed as choices change;
- **Merge** applies one transaction; a toast offers Undo. Ctrl+Z undoes the whole merge like any step.

For agents and the CLI the same data is the op's output, and the same choices are its `resolve` and `exclude`.

### 7.9 Undo, journal, recovery

The merge is one op, so it is one transaction, one undo step, one journal record with its patch (ADR 0021), recovered
like any other. Undoing it also reverts `merges.toml` (7.3). Nothing about merging is special to the journal.

## 8. Agents on their own branch (ADR 0020)

### 8.1 A session works on one branch

- A session opens on a lane: the transport asks `Branches::lane(branch)` and opens the session there. Which branch is
  set by the person when they start or pair the agent ("Work on: [the current branch] [its own branch]"), by a role
  file (AG-03), or by the agent itself (8.2). The in-app assistant works on the current branch unless the person ticks
  "own branch" in the prompt box.
- Everything ADR 0020 gives a session holds within its lane: grants, budget, the stale-write guard against the other
  actors on that branch, pause, revoke, undo-session (which reverts the agent's steps on its branch).
- A session does not move between lanes. `branch_work {branch}` (an MCP built-in) ends the session (`Closed`) and opens
  a new one on the target lane for the same connection, with the same credential and grants, the remaining budget and
  `parent` set to the old session; the agent is told its new branch. Its steps on the old branch stay its old
  session's, so undo-session still reverts exactly them.
- The person's Ctrl+Z addresses the folder lane only. Agents on other branches are not in that timeline; the person
  reviews them by compare and merge.

### 8.2 What an agent may do

| Action | Agent | Person |
|---|---|---|
| List branches, compare, diff, render any point | yes (`read`, `render`) | yes |
| Create a branch, from now or any point | yes (any edit scope); `owner_session` is set | yes |
| Move its own session to a branch (`branch_work`) | yes | n/a |
| Checkpoint a branch; mark its own branch's point ready for review | yes | yes |
| Merge another branch into the branch it works on (bring main in) | yes; ops run on the session's own lane | yes, into any branch |
| ... when the branch it works on is the folder branch | asks the person (7.7) | yes |
| Switch the folder | no (`person_only`) | yes |
| Rename or delete | only a branch made for its session | yes |
| Send back, mark merged | no | yes |

### 8.3 Review: the agent proposes, the person merges

1. The agent finishes a step of its task and calls `branch_checkpoint {label, ready: true, note}`: a `ready` point,
   and the ref's `[review]` becomes `ready`.
2. The Agent panel (and the Theatre) shows a card: "Designer: **Oval face, version A** is ready · Compare · Merge ·
   Send back", with the point's thumbnail.
3. **Merge** opens the merge view at that point (`at` is the reviewed point, so later edits by the agent are not taken
   by accident). When a `branch.merge` with that point commits, the host marks the review `merged`, and the agent's
   heads-up says so.
4. **Send back** takes a comment; the review becomes `sent-back`, and the agent's next result carries "the person sent
   back version A: 'chin still too pointy'", the same channel ADR 0020 uses for approval outcomes.

This is the safe pattern for big agent edits: the person's branch changes only when the person merges, every merge is
one undo step, and every reviewed state is a point that stays.

### 8.4 Many agents (phase 3)

TH-05 and CB-93 open one branch per take, each with a sub-agent session (AG-03, `parent` set), side by side. Picking a
take merges it; the others stay as branches. A Producer (AG-05) running on the folder branch merges results through
the same approval. Nothing here needs more than this design.

## 9. Restore never destroys; branch from history

- **Restore** (VC-01 re-cut) is the op `history.restore {to: StateRef}`: one transaction "Restore to 14:32
  'Hair › bangs 9 locks'" whose patch is the tree diff from now to that state. Ctrl+Z undoes it ("Go back").
- **The abandoned future is kept** (principles P3): `Branches::restore` first makes a `restore` point of the current
  committed state and an auto branch from it ("Before restore 14:32", `restore/2026-10-12-1432`, `auto = true`), then
  runs the op. The History panel, MCP's `history_restore` and the CLI go through it. Auto branches are grouped apart in
  the branch menu; the person deletes them when done.
- **Named versions** (VC-01) stay pinned snapshots (ADR 0021); naming a past transaction snapshots that state through
  `HistoryView`.
- **Branch from here** (History rows, the undo conflict card, the Theatre's rewind) is `Branches::create` with
  `from: {branch, at: {tx}}` (5.4).

## 10. The text project format and git (VC-04)

- **Omuzaki branches are not git branches.** They are local, in `.omuzaki/`, which is never committed. Git is optional and
  Omuzaki never runs it for branching.
- **The folder holds the folder branch's text files.** Git sees them as usual. Switching rewrites only the TOML files
  that differ, so `git status` shows exactly the branch difference. People who use both commit while on the branch
  they want in git; VC-04's commit view shows the current Omuzaki branch.
- **`merges.toml` is a normal project file** (one entity, stable keys) and is committed by default. On another machine
  its point ids are unknown and skipped (7.3), which costs extra conflicts, never data. VC-04 may choose to ignore it.
- **Files changed while Omuzaki was closed** (a `git pull`, a checkout) become the folder branch's next state, as ADR 0021
  says for a clean close. While Omuzaki is open, the external-change check applies: a file changed behind the keeper's back is never
  overwritten and is read back through an op.
- **VC-04 can reuse merge for git.** A point can be made from any tree (`kind = "import"`): VC-04 can import a git
  commit as a point on a `git/<ref>` branch, compare it, and merge it with `branch.merge`, passing the git merge base
  as `base`. Mapping Omuzaki branches onto git branches automatically is rejected (section 13).
- **VC-04's decisions are in `docs/design/git.md` (ADR 0045):** git is the person's own program, `merges.toml` stays
  committed, `.omuzaki/.gitignore` holds `*`, and pull is fast-forward only (anything else is an import plus `branch.merge`).

## 11. Performance budgets

| What | Budget | Measured by |
|---|---|---|
| Hot path | nothing added to `Runner::run`: a lane is chosen once per session, not per op | `omuzaki-ops` run bench unchanged |
| Checkpoint (off-thread) | one snapshot: ≤ 50 ms for a 500-file project with 10 changed files on a local SSD | keeper point bench |
| Switch to an open lane | ≤ 16 ms on the UI thread; files rewritten off-thread | host switch bench |
| Switch to a closed branch | ≤ 150 ms to a usable lane for a 500-file project on a local SSD, off the UI thread | host switch bench |
| Merge preview (dry-run) | `merge3`, units and validation ≤ 30 ms for a 500-file project; tree loads cached by point | `omuzaki-doc-api` merge bench, op bench |
| Memory | about twice the document size per open lane (executor and keeper shadow); at most 8 lanes | host test |
| Threads | 3 per open lane (writer, keeper, segment preparer) | host test |

## 12. Test plan

Fixtures are synthetic projects (never the sample character or private material). Deterministic: seeded RNG, `FakeClock`.

1. **`merge3` (property, `omuzaki-doc-api`).** `merge3(b, o, b) = o`; `merge3(b, b, t) = t`; `merge3(b, x, x) = x`;
   swapping mine and theirs swaps the sides of every conflict and leaves clean results equal; every path where both
   sides changed to different values is a conflict (no silent loss); resolving every conflict "mine" yields mine on
   those paths.
2. **The merge base (property, `omuzaki-history-api`).** Random sequences of fork, edit, checkpoint, full merge, pick,
   undo of a merge, restore and recovery over 2 to 5 branches, against a content oracle in which every edit adds a
   unique token. The oracle tracks what each side has received (its own edits, its fork, its merges whose record is
   still in its tree). The chosen base's tokens are always a subset of both sides' received tokens, and after merging,
   every token theirs holds that the base lacks is in the result or in a reported conflict. **Mutation checks** (each
   must fail): take `merges.toml` from the newest point instead of the live tree; skip step 3; prefer the newest
   candidate by time over inclusion.
3. **The key case (integration).** Branch, edit on both sides, merge, undo the merge, merge again: the branch's changes
   are offered again.
4. **Switch crash (failpoints, `omuzaki-test`).** Kill at each step of 5.5 and at the folder rewrite; reopen: the switch
   completes, the folder equals the target branch exactly, both branches keep every committed transaction, files
   outside the tree are untouched.
5. **Agent on its own branch (MCP end to end).** An agent works on its branch while a person edits main; it marks a
   point ready; the person merges two of three changed parts (a pick), then the third (a full merge); undo and redo
   work; undo-session on the agent's branch reverts only its steps; an agent's merge into main queues for the person.
6. **Library records.** A hairstyle applied on theirs merges with its parts and source rows; both sides updating the
   same item to different versions conflict as a unit; a merge never leaves an item's parts without their record.

## 13. Rejected alternatives

- **A branch as a line of the op log (the first idea):** patches replay only onto their exact base, so lines can't
  diverge for long, can't run at the same time and can't merge.
- **Switching as one tree-diff transaction in a single journal (the simplest option journal.md offered):** undo stacks
  of different branches would mix (an undo on one branch could revert another's step wherever values match), Ctrl+Z
  could "undo" a switch while the branch name stayed, and an agent and the person could never be on different branches
  at once.
- **One executor holding every branch's document:** one lock and one session registry are attractive, but every
  `Runner` call, event and journal record would need a branch, and every accepted ADR 0021 task would change.
  A lane per branch reuses ADR 0021 as it is.
- **A per-character branch scope:** section 3.1.
- **A copy of the project folder per branch:** no deduplication, slow on drvfs, a second lock, a second asset store.
- **The merge base from the point graph alone (git's way):** an undone merge would still count, and the next merge
  would silently drop the branch's changes. The record in the tree fixes it (7.3).
- **Merging by replaying theirs' ops onto ours (rebase):** ops replay differently on a different base, which is why
  ADR 0021 journals patches, and replay would interleave actors' histories.
- **Resolving conflicts automatically (mine wins, newest wins):** §12 promises no silent overwrites.
- **Git branches as Omuzaki branches:** git would become required, a checkout would rewrite the folder under a running
  keeper, and git branches carry files that aren't the project.
- **Locking a branch for an agent:** a session on its own lane is isolated without a lock the person must wait on.

## 14. Changes to other designs

| Design | Change |
|---|---|
| ADR 0021, `journal.md` 3, 4.4, 9 | One journal per branch, under a branch root; only the folder lane saves files; snapshot refs carry `branch`; trigger `point`; kind 10 stays unused |
| `journal.md` 7.3, VC-09 | Points pin their snapshots; GC roots include branch refs and points; the second location copies branches |
| ADR 0020, `agent-sessions.md` 3.1 | Session ids shared across a project's lanes; a session lives in one lane |
| `ops-api.md` | Adds `OpCtx::require_scope` |
| `HistoryView` (VC-03) | It reads the journal and archive, behind a `omuzaki-history-api` service |
| History diff and render ops (VC-03) | `StateRef` on both sides; units per 7.1 |
| Restore (VC-01) | `history.restore` and `history.name` ops; no `Runner` change |
| Project open (VC-08) | The open sequence goes into the new App crate `omuzaki-host` |

## 15. Tasks

Tasks of VC-02 unless noted, in "Phase 1: core + character". Each is a fenced task with its contract and
acceptance checks.

| Task | Pri |
|---|---|
| Contract: branches, points, `StateRef`, `BranchStore`, `HistoryView`, `Branches`, events (`omuzaki-history-api` + mock); `Snapshots::take_at`; `OpCtx::require_scope` | P0 |
| `merge_base` over version vectors, with the property test of section 12.2 | P0 |
| `omuzaki-doc-api`: `merge3` | P0 |
| `omuzaki-doc`: branch refs, points, `folder.toml`, the `BranchStore` service, GC roots and retention pins | P0 |
| `omuzaki-doc`: points from the keeper (checkpoints of the committed state, points from any tree), `Snapshots::take_at` | P0 |
| re-cut: `HistoryView` over the journal and archive (VC-03) | P0 |
| re-cut: `history.diff` over `StateRef`, the units module (VC-03) | P0 |
| re-cut: render ops over `StateRef` (VC-03) | P0 |
| `omuzaki-host`: lanes (open, close, recover, a runner per branch, shared session ids) | P0 |
| `omuzaki-host`: create, checkpoint, rename, delete, review | P0 |
| `omuzaki-host`: switch the folder, crash-safely | P0 |
| re-cut: `history.restore` and `history.name` ops (VC-01) | P0 |
| `omuzaki-host`: restore keeps the abandoned future as an automatic branch | P0 |
| `omuzaki-ops`: `branch.merge` | P0 |
| `omuzaki-ops`: `branch.list` and `branch.status` | P0 |
| `omuzaki-mcp`: sessions on a branch, branch built-ins, review feedback | P0 |
| `omuzaki-cli`: `omuzaki branch` commands | P0 |
| `omuzaki-ui`: branch menu, switch, review cards | P0 |
| `omuzaki-ui`: compare branches side by side | P0 |
| `omuzaki-ui`: the merge view | P0 |
| `omuzaki-test`: branch switch crashes and the agent-on-its-own-branch session | P0 |
| `branch.merge`: "Keep both" (theirs as a disabled part variant) | P1 |

Comparing two History rows (VC-03) stays as filed; the branch compare view reuses its component.
