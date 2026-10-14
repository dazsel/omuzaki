# 0025: Branches are named heads over content-addressed points, each open branch runs in its own lane, and merges are three-way by unit with their bookkeeping in the tree (2026-10-12)

**Status:** accepted (2026-10-12). It adds to `omuzaki-history-api` and `omuzaki-ops-api`, and adds a
new App crate, `omuzaki-host`.
**Context:** VC-02, VC-03, VC-01, VC-04; spec v3 §6, §7, §12, §13 (TH-05, TH-07),
§18 (CB-13, CB-91, CB-93), §29; ADR 0019 (library records), ADR 0020 (agent sessions), ADR 0021 (the journal and
snapshots). **Design:** `docs/design/branches.md`.

## Context

VC-02 asks to branch a character or a project, compare branches side by side and merge part-level changes; VC-03
asks for before and after renders and a parameter diff between any two points. Agents should be able to make big
edits on a branch of their own while the person keeps working, and the person merges what they like (§12: no silent
overwrites; TH-05: explore variants in parallel). Restoring an old point must keep the abandoned future (principles
P3).

ADR 0021 gives a project one journal, content-addressed snapshots and a keeper that saves the files behind the
journal. It left branches open, with one constraint: one journal per working copy. ADR 0020 puts agent sessions,
grants and the stale-write guard in the executor. ADR 0019 keeps library application records in the document tree so
they are undone with what they wrote.

Three things make branches hard here: undo is per actor and patch-based, so histories of different branches must
never mix; a merge can be reverted in five ways (undo, undo-session, restore, backup restore, recovery), and the
merge base must forget it in all of them; and an agent's branch must run at the same time as the person's.

## Decision

1. **A branch is a named head over the project's history.** Its ref (`.omuzaki/branches/<id>/ref.toml`) points at a
   **point**: an immutable record in the snapshot store (canonical JSON, BLAKE3 id) naming a snapshot, the branch's
   previous point and, as a version vector, the newest point of every other branch the state includes. Points are made
   at forks, checkpoints, review requests, switches and lane closes, never per transaction. Snapshots and points share
   one store and deduplicate.
2. **Branches cover the whole project.** Branching a character is a branch with a **focus** on it, which the UI,
   compare, the default merge scope and (later) agent fences use. Per-use variants (CB-13) and part variants (CB-91)
   stay inside the document.
3. **Each open branch runs in its own lane:** an executor, a journal and a keeper, as ADR 0021 describes, on a branch
   root (`.omuzaki/` for `main`, `.omuzaki/branches/<id>/` for others). Only the folder branch's keeper saves the TOML
   files. A lane manager, `Branches` (contract in `omuzaki-history-api`, implemented in the new App crate `omuzaki-host`,
   which `omuzaki-cli` and `omuzaki-ui` share), opens and closes lanes and never holds one lane's lock while taking
   another's. Ops see their own lane live and other branches only through points.
4. **Switching re-points the folder.** The target lane's keeper rewrites the files that differ; no document
   transaction happens, each branch keeps its own undo history, and open transactions stay in their lanes (the
   person's own block the switch). A marker in `.omuzaki/branches/folder.toml` makes it crash-safe. ADR 0021's reserved
   record kind 10 stays unused.
5. **Compare uses state references** (`now`, a transaction, a snapshot, a point, a branch) for VC-02 and VC-03 alike:
   the parameter diff grouped by unit (a part, the face, a library application...) and renders at a matched camera.
   History at any transaction is rebuilt from the journal and its archive (`HistoryView`), not from in-memory inverse
   patches.
6. **Merge is `branch.merge`: a three-way merge of document trees, as one undoable transaction.** One-sided changes
   merge cleanly; same-value conflicts are listed per unit and never resolved silently; the person keeps mine or
   theirs per unit or per parameter, keeps both (theirs as a disabled part variant), or leaves units out (a pick). The
   result must validate. An agent's merge into the branch the person has open goes through ADR 0020's approval queue.
7. **The merge base comes from version vectors, and the merge record lives in the tree.** A full merge writes
   `merges.toml` (which points of which branches this branch includes) in the same transaction as the merged values,
   so every way of reverting a merge also forgets it, and the next merge offers the branch's changes again. A base is
   only ever too old, never too new: the worst case is an extra conflict, never a silent loss.
8. **Library application records merge as units** with the parts they created and their source rows; a record's pin
   is atomic; a record never disappears while anything it wrote remains, so the export gate always sees it.
9. **Agents work on their own branch and the person merges.** A session opens on one lane and stays there
   (`branch_work` reopens it on another). Agents may create branches, checkpoint, merge into their own branch and mark
   a point ready for review; the person compares, merges or sends it back with a comment.
10. **Restore never destroys.** `history.restore` is one transaction whose patch is the tree diff; before it runs, the
    lane manager keeps the current state as an automatic branch ("Before restore 14:32").
11. **Git stays separate.** Omuzaki branches are local, in `.omuzaki/`; the folder holds the current branch's text files
    for git; `merges.toml` is a normal project file; VC-04 can import a git commit as a point and reuse compare and
    merge.
12. **Contracts:** `omuzaki-history-api` gains branch and point types, `StateRef`, `Contains` and `merge_base`, the
    `BranchStore` and `HistoryView` services and the `Branches` trait with its events; `SnapshotInfo` gains `branch`,
    `Trigger` gains `Point` and `Snapshots` gains `take_at`. `omuzaki-doc-api` gains `merge3`. `omuzaki-ops-api` gains
    `OpCtx::require_scope`. No `Runner` change.

## Consequences

- ADR 0021 is amended, not superseded: one journal per branch under a branch root, refs carrying `branch`, points
  pinning their snapshots and counted as garbage-collection roots, the second backup location copying branches. The
  journal tasks take the root as a parameter.
- ADR 0020 holds within each lane; session ids are shared across a project's lanes. The person's
  Ctrl+Z addresses the folder branch only.
- The open-history task loses its `Runner` change: restore and named versions become ops over `HistoryView`, and branching from a point
  is `Branches::create`. The branch-listing task is re-cut to read the journal, and the merge tasks take state references.
- A new App crate, `omuzaki-host`, holds the project open for writing; the CLI's open sequence moves there
  registers it as an App (`crates/README.md`, `cargo xtask lint-deps`).
- Every open branch costs about twice the document's memory and three threads; at most 8 lanes are open at once.
- `merges.toml` appears in the project folder after the first merge, and in git if the person commits it.
- Rejected: a branch as a line of the op log; switching as a tree-diff transaction in one journal; one executor holding
  every branch; a per-character branch scope; a folder copy per branch; the merge base from the point graph alone;
  merging by replaying ops; automatic conflict resolution; git branches as Omuzaki branches; locking branches for
  agents. Reasons in the design, section 13.
- Tasks: design section 15 (and the re-cuts).
