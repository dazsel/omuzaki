# 0021: Changes are journalled ahead of the files; snapshots are content-addressed manifests that backups reuse (2026-10-12)

**Status:** accepted (2026-10-12). It adds a new API crate, `omuzaki-history-api`, and a `omuzaki-store-api`
addition.
**Context:** VC-08, VC-09, VC-01, VC-02; spec v3 §6, §7, §8, §29; ADR 0013 (file roots; the temp-name and `O_TMPFILE` notes). **Design:** `docs/design/journal.md`.

## Context

The executor keeps the document, the op log and the undo history in memory. `omuzaki-doc` writes the TOML files only when
`Project::save` is called, so the CLI wraps the project to save after every op. Several files are never saved as
one unit, nothing persists history, and nothing survives a crash except whatever the last save left. VC-08
asks that every transaction be journalled without blocking the real-time path, that a relaunch after a crash or power
cut offer "restore to the last transaction" and lose at most the op in flight, and that a `kill -9` at a random point of
a 1,000-op session recover exactly. VC-09 asks for verified, deduplicated rolling backups in the same format.

Measured on 2026-10-12 (design section 2): appending a record and flushing it costs 0.3 to 0.8 ms at the median and at
most 3 ms at p99 on ext4, native NTFS and drvfs, and 64 KiB costs about the same as 512 bytes. Replacing one file
atomically costs 2 to 4 ms natively and 9 to 25 ms on drvfs.

## Decision

1. **A write-ahead journal.** Every change the executor makes (applied op, commit, rollback, undo, redo, a dropped
   step) is appended to `.omuzaki/journal/` as a CRC-32-framed JSON record carrying the **patch** it applied, numbered by
   a project-wide LSN. Records are physical: replay needs no op code. Write-through saving is rejected.
2. **Never on a latency-sensitive thread.** The executor hands each entry to a `JournalSink` under its lock in O(1)
   (budget 2 µs p99, no I/O). One writer thread group-commits: `fdatasync` on Linux, `FlushFileBuffers` on Windows,
   and on Apple `F_FULLFSYNC` for any batch an external caller waits on (group-committed, so concurrent acknowledgements
   share one flush) and at snapshots and close, `F_BARRIERFSYNC` for interactive edits. A commit never creates a
   file: the next segment is created and flushed ahead of time by a low-priority preparer. A full queue refuses new
   edits with a reason; it never blocks and never drops. The frame, tracking and audio threads never touch the
   executor or the journal.
3. **Loss is bounded and stated.** A crash loses open transactions and at most the last few milliseconds of commits.
   MCP, the CLI and scripts are acknowledged only after the record is durable, so nothing acknowledged to them is ever
   lost on any platform; the UI shows changes at once (on Apple, a power cut can take the person's interactive edits
   since the last full flush, never out of order).
4. **Files follow the journal.** A keeper thread applies durable entries to a shadow `Project` and saves the TOML files
   (250 ms after the last change, at most every 2 s). Files on disk never hold a change the journal could lose, and
   recovery never trusts them.
5. **Snapshots** are content-addressed manifests (path to BLAKE3 hash, canonical JSON; the id is the manifest's hash)
   in a per-project `DiskStore` at `.omuzaki/snapshots/`, with small files packed one object per snapshot and assets
   referenced, never copied. Every project-owned file is included, deduplicated by hash; only derived paths (caches,
   renders) declared with `register_derived_path!` are left out; a file over 1 GB is included with a warning. A small
   ref per snapshot holds its LSN (the persistent document version), trigger, label, history export, verification and
   pins. Taken at session start, every 10 minutes or 5,000 records of editing, at a
   clean close, and before migrations, restores, recoveries and bulk agent sessions.
6. **Recovery** after an unclean shutdown (no `close` record; one writer per folder through an OS lock): the newest
   verified snapshot of the crashed session, plus the journal after it up to the first bad frame. Open transactions are
   closed with synthetic entries (rolled back, or kept as "Recovered: <label>" when committed work depends on them), and
   the executor replays the result with its own state code, so the document and the undo stacks both come back. The
   relaunch offers "Restore to the last transaction" or "Open the files as they are"; both keep the other version as a
   snapshot. The CLI and MCP restore automatically, with a notice naming what was rolled back. Every step is durable
   before the next and idempotent, so a crash during recovery recovers again.
7. **Versions:** the journal format and the document schema are versioned apart; every shipped journal format keeps a
   decoder; a journal from a newer build is never replayed; a session's records share its base's schema, so replay
   never crosses a migration.
8. **History and backups reuse the same pieces.** Snapshots carry the undo and redo stacks; sealed segments are
   the op log archive (VC-01), kept 90 days by default (configurable; pinned and named versions never pruned; disk use
   shown in Settings); named versions are pinned snapshots; VC-09 backups are snapshots under retention, copied
   to a second location in the same layout.
9. **Contracts:** a new API crate, `omuzaki-history-api` (+ mock): `HistoryEntry`, `JournalSink`, `HistoryExport`, `Lsn`,
   snapshot ids and refs, and the `Snapshots` service the `backup.*` and `project.save` ops use. `omuzaki-store-api` gains
   listing and removing objects for garbage collection. The journal, keeper, snapshots, recovery and backups live in
   `omuzaki-doc`.

## Consequences

- Existing tasks are re-cut: the CLI opens projects through the journal (the `SavedProject` wrapper goes); the folder
  is written to match a project exactly; the undo-depth and in-memory-log caps; the executor's
  history export, restore and replay; and "changed on disk" stays, with the folder lock split out as P0.
  Implementation tasks are filed under VC-08, VC-09 and VC-01, and
  the VC-09 tasks stay, with the edits in design section 10. The list is design section 16.
- Files on disk trail the document by up to about 2 s. Agents read through ops; `project.save` forces a save.
- `.omuzaki/` grows by the archive and the snapshots; retention (90-day archive, VC-09 buckets) and garbage collection
  bound it.
- Unverified until measured: NTFS keeping a prepared segment's directory entry through a power cut (a power-cut test
  joins), and Apple flush costs (estimates until an M-series machine runs the benches).
- A `kill -9` test cannot see a missing flush (the page cache survives), so a deterministic simulated-power-loss harness
  is part of the test plan.
- Rejected: write-through saving; a commit-only journal (interleaved transactions don't replay); logical (op) records;
  SQLite; one store object per file in a snapshot; the TOML files as the recovery base. Reasons in the design, section
  14.
