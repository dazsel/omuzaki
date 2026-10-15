---
title: "The journal, snapshots and crash recovery (VC-08), and the snapshot format backups reuse (VC-09)"
summary: "The journal, snapshots and crash recovery (VC-08), and the snapshot format backups reuse (VC-09)"
area: design
---

# The journal, snapshots and crash recovery (VC-08), and the snapshot format backups reuse (VC-09)

**Status:** accepted (2026-10-12; decisions in section 15). Adds a new API crate and a
`omuzaki-store-api` addition.
**Decision record:** ADR 0021. **Spec:** v3 §6 (ops, transactions, undo), §7 (project layout, migrations, the
content-addressed store), §8 (the real-time path never blocks), §29 (VC-01, VC-02, VC-08, VC-09).
**Settles:** write-through saving, an interrupted layout-changing save, a replayable op log, one
writer per folder and persisted history. **Code today:** `omuzaki-doc` (`Project`), `omuzaki-store` (`DiskStore`),
`omuzaki-ops` (`Executor`), `omuzaki-ops-api` (`Runner`, `OpRecord`, `Event`).

This page explains the decisions so an implementer or a reviewer can disagree with a reason. The types it sketches are
finalised in their own tasks (section 16) and documented in the code.

## 0. The decisions in brief

1. **A write-ahead journal, not write-through saving.** Every change the executor makes (applied op, commit, rollback,
   undo, redo) is appended to `.omuzaki/journal/` as a checksummed record carrying its **patch**. The TOML files are
   written behind it, at most about 2 s later, by a background **keeper**. Recovery never trusts the TOML files.
2. **Off every latency-sensitive thread.** The executor hands each entry to the journal under its lock in O(1), with no
   I/O and no syscall in the common case (budget: 2 µs p99). One **writer** thread writes and flushes batches (group
   commit); the frame, tracking and audio threads never touch the executor or the journal.
3. **What a crash can lose:** open transactions (by definition), plus committed work whose record had not reached the
   disk, which is the last few milliseconds (measured flush p99: 0.6 to 2.9 ms). MCP, the CLI and scripts are told
   "done" only after the record is durable, so nothing acknowledged to them is ever lost, on every platform (on Apple
   their commits get a full flush; only the person's own interactive edits use the cheaper barrier, 5.2 and 5.3). A
   commit never creates a file.
4. **Snapshots** are immutable manifests (file path to BLAKE3 hash) in a per-project content-addressed store, with small
   files packed into one object per snapshot. A snapshot is taken when a session starts, every 10 minutes or 5,000
   records of editing, at a clean close, and before migrations, restores, recoveries and bulk agent sessions. VC-09
   backups are these snapshots under a retention policy; nothing is copied twice.
5. **Recovery** starts from the newest snapshot of the crashed session and replays the journal after it. Records are
   physical (patches), so replay needs no op code and can't drift between builds. A torn tail ends the log; nothing
   after a bad record is ever applied. Open transactions are rolled back, or kept as "Recovered: <label>" when later
   committed work depends on them. The relaunch offers "Restore to the last transaction", keeps whatever was on disk
   as a snapshot either way, and is itself crash-safe.
6. **History survives sessions.** Each snapshot carries the executor's undo and redo stacks; sealed journal segments
   become the op log archive that VC-01's history reads. Named versions are pinned snapshots.

## 1. What has to be true

| Requirement | Source | Where |
|---|---|---|
| Every transaction journalled without ever blocking the real-time path | VC-08, §8 | 5.1, 5.2, 13 |
| After a crash or power loss, the relaunch offers "restore to the last transaction" | VC-08 | 8.4 |
| Nothing the person saw applied is lost (at most the op in flight) | VC-08 | 5.3 |
| `kill -9` at a random point of a 1,000-op session: the document equals the state after the last completed transaction | VC-08 | 12 |
| Old projects always open (migrations); replay never crosses a migration | §7, VC-08 | 4.5, 8.6 |
| Rolling local snapshots, retention, a second location, verified backups, preview before restore, binaries deduplicated | VC-09 | 7, 10 |
| Undo and redo history persists (`omuzaki undo` in a new process) | VC-01 | 9 |
| One writer per project folder | VC-08 | 8.1 |
| A crash during a save never leaves a project that won't open | VC-08 | 6, 8.5 |

## 2. Measured costs (the numbers this design rests on)

Measured on the development machine on 2026-10-12 (WSL2 on Windows 11, NVMe `C:`, SSD `D:`, under a background load
average of 15 to 22). A scratch script, not committed: append a record and flush it with `fdatasync` (Linux) or
`FlushFileBuffers` (Windows, via PowerShell `FileStream.Flush(true)`); replace a 4 KiB file atomically (temp file,
flush, rename, plus a folder fsync on Linux); write and flush a 1 MiB file. Milliseconds.

| Path | append 512 B + flush, p50 / p99 (max) | append 64 KiB + flush, p50 / p99 | atomic replace 4 KiB, p50 / p99 | 1 MiB file, p50 |
|---|---|---|---|---|
| ext4 (WSL, `~/work`) | 0.75 / 2.9 (4.7) | 0.89 / 2.2 | 1.7 / 3.6 | 1.3 |
| NTFS native, `C:` | 0.31 / 0.59 (17) | 0.40 / 0.61 | 2.1 / 4.3 | 2.2 |
| NTFS native, `D:` | 0.34 / 1.5 (56) | 0.40 / 0.61 | 3.0 / 4.2 | 2.6 |
| drvfs 9p, `/mnt/c` | 0.65 / 1.5 (1.7) | 1.1 / 1.7 | 9.0 / 15.1 | 127 |
| drvfs 9p, second drive | 0.81 / 2.5 (3.2) | 1.2 / 1.8 | 17.6 / 24.8 | 156 |

What follows from them:
- **A durable append costs about a millisecond everywhere, and a batch 128 times larger costs about the same.** Group
  commit therefore keeps the loss window at a few milliseconds without a timer. Native NTFS shows rare stalls (17 and
  56 ms max), which the budget in section 13 allows for.
- **Write-through saving** (save the touched files after every op) costs 2 to 4 ms per file natively and 9 to
  25 ms on drvfs, per op and per file, on the thread that ran the op, and is still not atomic across files. Rejected.
- **drvfs is throughput-bound** (about 7 to 8 MB/s for large writes), so snapshots and saves run off-thread. Its flush
  appears to pass through to Windows (latency tracks native `FlushFileBuffers`); a power-cut test was not possible.
  `O_TMPFILE` is unsupported there and link counts read 1 (ADR 0013); the journal uses neither.

## 3. The shape

```
 UI thread · MCP sessions · CLI · scripts
        │ Runner::run / begin / commit / rollback / undo / redo
        ▼
 Executor (omuzaki-ops) ── one state lock: plan, apply to the in-memory Project, log ──┐
        │ under the lock, after a successful apply: sink.append(entry)  (O(1), no I/O)  │
        ▼                                                                              │ events, as today
 JournalSink (omuzaki-history-api) ◄── implemented by omuzaki-doc's ProjectJournal        ▼
        │ queue (entries in history order)                                   UI, Build Theatre, MCP
        ▼
 writer thread: encode → one write → sync_data → durable LSN → wake sync() waiters
        │ durable entries, in order
        ▼
 keeper thread (low priority): shadow Project ← apply each entry's patch
        ├─ saves the TOML files (debounced: 250 ms idle, 2 s max)
        ├─ snapshots (cadence and Marks) into .omuzaki/snapshots/ (DiskStore)
        └─ compaction: seals segments behind a snapshot into the archive .omuzaki/log/
```

- **The executor's document stays in memory and never touches the disk.** It is still a `Project` (so the storability
  and layout rules of `Project::apply` refuse the same patches they do today), opened once and cloned for the keeper.
- **The keeper owns a shadow copy** and applies the same patches, in the same order, with the same `before` checks.
  Saving and snapshotting therefore never take the executor's lock, and the shadow checks continuously that the journal
  replays: a divergence is a bug, reported on the journal status (section 5.4).
- **Write-ahead order:** the keeper only sees entries after they are durable, so the files on disk never hold a change
  the journal could lose.
- **Crates.** The contract (`HistoryEntry`, `JournalSink`, `HistoryExport`, snapshot ids and refs, the `Snapshots`
  service) is a new API crate, `omuzaki-history-api`, with `omuzaki-history-mock`. It depends on `omuzaki-ops-api`,
  `omuzaki-doc-api` and `omuzaki-store-api`. The executor (`omuzaki-ops`) depends on it to call the sink. The journal,
  keeper, snapshots, recovery and backups live in `omuzaki-doc` (`src/journal/`, `src/snapshot/`, `src/recovery/`,
  `src/backup/`), because they need `Project`'s rendering, loading and layout and the dependency rule
  (`crates/README.md`) forbids another implementation crate from using it. Apps (`omuzaki-cli`, `omuzaki-ui`,
  `omuzaki-test`) wire them together.

### Layout of `.omuzaki/`

```
my-show.omuzaki/.omuzaki/
  id                                     random 128-bit project id (hex): names this project in a second location
  lock                                   one writer per folder (8.1)
  journal/segment-000000000042.tjl       live segments, by sequence number (first LSN in the header), plus one prepared
  log/segment-000000000001.tjl           sealed segments behind a snapshot: the op log archive (history, provenance)
  snapshots/objects/ab/cdef…/{data,provenance.json}   a DiskStore: manifests, packs, large files, history blobs
  snapshots/refs/20261012T143205Z-1a2b3c4d5e6f.toml   one per snapshot (7.1)
  snapshots/quarantine/                  DiskStore::repair's
  recovery/20261012T143300Z/             the journal of a recovered session, kept 30 days
  replaced/characters/mia/face.toml.<time>-<pid>-<n>.<hash>.kept   a file a save replaced, kept 10 minutes (6.1)
  replaced/characters/mia/face.toml.<time>-<pid>-<n>.displaced     a file that raced a save; never deleted automatically
```

`.omuzaki/` is never committed and travels with a copied folder (`docs/design/ux/shell.md` section 17).

## 4. The journal format

### 4.1 Segments

The journal is a sequence of **segment files**. Every entry gets a **log sequence number (LSN)**: a `u64` that starts at
1 for a project, increases by one per record, continues across sessions and is never reused. A segment is named by a
sequence number (`segment-<12 digits>.tjl`), so name order is log order, and its header holds its first LSN. A segment
is sealed at 16 MiB (about 40,000 op records) or when a session ends. Every session starts a new segment, whose first
record is `session`.

**A commit never creates a file.** The next segment is prepared ahead of time, off the commit path, by a low-priority
preparer thread: created with a header whose first LSN is 0 ("prepared, unused"), flushed, and its folder synced on
Unix. One prepared segment always waits, from session start on. Rotation writes the real header and the first batch
into it at offset 0 in one write and flushes it: a data flush of a file whose directory entry has long been durable.
If the preparer were ever behind (it has 16 MiB of commits to catch up), the writer creates the file itself and records
a warning on the status.

### 4.2 Segment header (32 bytes, little-endian)

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | magic `TUBRJNL\0` |
| 8 | 2 | format version (1) |
| 10 | 2 | header length (32) |
| 12 | 4 | reserved, 0 |
| 16 | 8 | first LSN; 0 while the segment is prepared and unused |
| 24 | 4 | reserved, 0 |
| 28 | 4 | CRC-32 of bytes 0 to 27 |

### 4.3 Record frame (20-byte header, then the payload)

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | payload length (`u32`); 0 means "no more records here" (a zero-filled tail) |
| 4 | 4 | CRC-32 over bytes 0 to 3, bytes 8 to 19 and the payload |
| 8 | 8 | LSN |
| 16 | 1 | kind (4.4) |
| 17 | 1 | flags: bit 0 = *skippable* (a reader that doesn't know the kind may skip it); others 0 |
| 18 | 2 | reserved, 0 |
| 20 | n | payload: one JSON object (4.5) |

- **Checksum:** CRC-32 (the `crc32fast` crate: MIT or Apache-2.0, SIMD-accelerated, already in `Cargo.lock`, so only a
  workspace edge and a register row). It is what catches torn and garbled writes. BLAKE3 would work too, but a 32-bit
  CRC is the standard torn-write detector, costs less per small record, and the LSN sequence and length bounds back it
  up. Rejected: chaining each CRC to the previous one; it would make every record after a bad one unreadable, and
  independent CRCs let the damage report say how many later records looked intact (8.2).
- **Payload cap:** 64 MiB. A patch that would exceed it is refused by `admit` before it is applied (no op is near it; a
  whole-document replacement of a very large project is the only candidate).

### 4.4 Record kinds

| Kind | Name | Payload | Written when |
|---|---|---|---|
| 1 | `session` | session id, app version, document schema, base snapshot id and LSN, start time, pid, host. The executor's `HistoryEntry::Session { session, started }` supplies the id and `started`, the wall time at runner-clock zero, so an entry's wall time is `started + at`; the runner's `Seconds` stay session-relative and finite (a writer refuses a non-finite time) | a session starts (after its base snapshot is durable) |
| 2 | `begin` | tx, actor, label, time | `Runner::begin` |
| 3 | `op` | the `OpRecord` (seq, op, args, actor, cause, tx, time, **patch**, version), `commits` | an op is applied; `commits: true` for a one-op transaction, which has no `begin` or `commit` |
| 4 | `commit` | tx | `Runner::commit` |
| 5 | `rollback` | tx, the revert **patch** applied | `Runner::rollback` succeeds |
| 6 | `undo` | actor, tx, the **patch** applied | `Runner::undo` applies a step |
| 7 | `redo` | actor, tx, the **patch** applied | `Runner::redo` applies a step |
| 8 | `dropped` | actor, tx, stack (undo or redo) | a step is dropped after a conflict (the executor's no-wedge rule) |
| 9 | `close` | the close snapshot id | a clean shutdown, last record of the session |
| 10 | reserved | | a branch switch that replaces the history stacks, if the branches design chooses that (section 9) |
| 11 | `audit` (skippable) | one `AuditFact`: session opened or ended, grants or budget changed, paused, resumed, approval requested or resolved | the executor changes a session or resolves an approval, under its lock (`team-review.md` §9) |

Queries are not journalled, as they are not logged today. Failed applies and refused undos never reach the
journal: the executor appends only after a change succeeded.

### 4.5 Encoding and versioning

- **JSON payloads** (`serde_json`). Patches already hold `serde_json::Value`, so JSON round-trips them exactly
  (shortest-round-trip floats, sorted object keys), it is deterministic, and a damaged journal can be read by a person.
  Encoding happens on the writer thread, never on the hot path. Rejected: `bincode` or `postcard` (brittle under
  schema evolution, and the size gain is irrelevant at about 400 bytes per op record); CBOR (a new dependency for no
  gain here).
- **Two versions, kept apart.** The *format* version (segment header) covers framing and record kinds. The *document
  schema* (in each `session` record) covers what the patches mean. A new field in a payload is added with a serde
  default (no format bump). A new kind is either skippable or bumps the format version.
- **Old journals always read.** Every format version ever shipped keeps a decoder, guarded by golden segment files
  under `crates/omuzaki-doc/tests/fixtures/journal/v<N>/`. A journal from a *newer* format, or with an unknown kind that
  isn't skippable, is never replayed: the project opens read-only with "this project was being edited by a newer
  Omuzaki; open it there to recover the last changes", and nothing is changed.
- **Replay never crosses a migration.** Records in a session are patches against that session's schema, and a
  migration only happens when a project is opened, before its session starts (8.6).

## 5. The write path

### 5.1 The hand-off (contract in `omuzaki-history-api`; sketch)

```rust
/// One change to history, in the order the executor made it. What the journal writes and what recovery replays.
pub enum HistoryEntry {
    Begin { tx: TxId, actor: Actor, label: String, at: Seconds },
    Op { record: Arc<OpRecord>, commits: bool },
    Commit { tx: TxId },
    Rollback { tx: TxId, patch: Patch },
    Undo { actor: Actor, tx: TxId, patch: Patch },
    Redo { actor: Actor, tx: TxId, patch: Patch },
    Dropped { actor: Actor, tx: TxId, stack: Stack },
    /// The executor's whole history right now (cheap: steps are shared), so a snapshot can carry it. Not written to
    /// the journal file.
    Mark { history: Arc<HistoryExport> },
}

pub trait JournalSink: Send + Sync {
    /// Takes `entry`. Called under the executor's lock, in history order: never blocks, never does I/O.
    fn append(&self, entry: HistoryEntry) -> Lsn;
    /// Whether a new edit may be applied; the error says why not and what to do (5.4).
    fn admit(&self) -> Result<(), OpError>;
    /// True when a snapshot is due and the journal wants a `Mark`.
    fn wants_mark(&self) -> bool;
    /// Waits until everything appended before the call is durable; returns the durable LSN.
    fn sync(&self, timeout: Duration) -> Result<Lsn, SyncError>;
    /// Health, for the status banner and the MCP reply.
    fn status(&self) -> JournalStatus;
}
```

The executor (`Executor::with_journal(Arc<dyn JournalSink>)`):
- calls `admit()` before planning an applied edit, an undo or a redo (never before `commit` or `rollback`: closing a
  transaction must always be possible);
- appends after each successful change, still under its lock, so journal order is history order;
- after an append, if `wants_mark()` is true, appends a `Mark` with `history_export()`. For that to be cheap, the
  executor's steps become `Arc`-shared (an export clones pointers, about a microsecond for 1,000 steps);
- exposes `flush_history()`, which appends a `Mark` now (the clean close uses it).

Rejected: feeding the journal from `Runner::subscribe()`. Events are delivered outside the lock over unbounded channels
with no back-pressure, `Undone` and `Redone` carry no patch, and a dropped step emits nothing, so the stream can't
rebuild history.

### 5.2 The writer: group commit and flushing

```
loop:
    batch = take everything queued (sleep on a condvar only when the queue is empty)
    encode the batch into one reused buffer (frames of 4.3)
    one write() at the end of the current segment
    file.sync_data()
    publish the durable LSN; wake sync() waiters; pass the batch to the keeper
```

- **Group commit without a timer.** Whatever arrives while one flush runs goes out in the next. At most one flush is in
  flight; a batch is capped at 4 MiB. The executor wakes the writer only if it flagged itself asleep, so a busy writer
  costs the hand-off no syscall.
- **Flushing per platform.** Linux: `fdatasync` (`File::sync_data`). Windows: `FlushFileBuffers` (`File::sync_data`).
  Apple: the writer keeps two marks, the *ordered* LSN and the *durable* LSN. A batch nobody external waits on (the
  person's own interactive edits) gets `fcntl(F_BARRIERFSYNC)` where the OS has it: an ordering barrier (the drive writes
  the batch after everything before it) that doesn't empty the drive's cache. Whenever a `sync()` caller (MCP, the CLI,
  a script) is waiting on an LSN above the durable mark, the next flush is `F_FULLFSYNC`, group-committed: every
  waiter whose LSN it covers is released by that one flush, and if no new data is queued the writer issues the full
  flush on its own. Snapshots and the clean close also use `F_FULLFSYNC`; without `F_BARRIERFSYNC`, every flush is full.
  Apple costs are not measured (no Mac here); the Apple rows of section 13 are estimates until an M-series machine
  measures them.
- **New files are off the commit path** (4.1). On Unix a prepared segment's folder is synced when it is prepared
  (`File::open(dir)?.sync_all()`, as `omuzaki-store` does). Windows has no folder sync, so the design never relies on a
  directory entry made at commit time; whether NTFS keeps a prepared segment's entry through a power cut without a
  folder flush is **unverified** until the power-cut test on a Windows host runs (section 12).
- **No preallocation in v1.** Appends grow the file; on ext4 `fdatasync` then also flushes the size, which the
  numbers in section 2 already include. The reader handles zero-filled tails anyway (4.3), so preallocation can be
  added later without a format change if a benchmark shows a gain.
- **One file handle, one writer.** Only the writer thread writes segments; the keeper and recovery only read them, and
  compaction renames sealed ones.

### 5.3 What can be lost

| Case | Lost | Why it's acceptable |
|---|---|---|
| Open transactions | all of them | the spec's definition: "the state after the last completed transaction" |
| Committed, record not yet durable | the last few ms of commits (measured flush p99 0.6 to 2.9 ms, plus encoding) | "at most the op in flight"; below what a person can perceive as applied |
| Anything acknowledged to MCP, the CLI or a script | nothing, on every platform | they are told "done" only after `sync`, which on Apple waits for an `F_FULLFSYNC` (5.2, 5.5) |
| Apple, power cut only: the person's own interactive edits | those made since the last full flush (the last external acknowledgement, snapshot or close); never a torn or reordered log | `F_BARRIERFSYNC` keeps order without emptying the drive cache (decided 2026-10-12); a process or OS crash loses none of them |
| Records after a damaged one (bit rot in old data) | those records | never guessed at; kept aside and reported (8.2), and backups hold earlier states |

The UI does not wait for `sync`: the UI thread must never block on I/O, and the change is already on screen. Its loss
window is the flush latency.

### 5.4 Back-pressure and failures

- **Queue limits, by entries:** soft 50,000 (status `Behind`, banner "Saving is behind"), hard 200,000 (`admit`
  refuses new edits with `Unavailable`: "changes can't be saved: <last error>; nothing new is applied until saving
  catches up"). Commits and rollbacks are always admitted. Nothing is ever dropped and nothing ever blocks.
- **Write errors** (disk full, drive gone, permission): the writer retries with backoff (100 ms doubling to 5 s) and
  sets status `Failing { reason }` at the first failure, so the banner shows at once, long before the hard limit. The
  banner offers **Save a copy elsewhere** (writes the in-memory document to a new folder with `Project::create` and a
  save), because the in-memory document may be the only up-to-date copy.
- **A dead writer or keeper** (a panic, or the shadow diverging from the executor) sets `Failing` at once with "saving
  stopped: internal error, please report it", and `admit` refuses new edits immediately: no further work should pile up
  that cannot be saved.

### 5.5 Acknowledgements per surface

| Surface | When it says "done" |
|---|---|
| UI | at once (the change is on screen; durable within the flush latency) |
| MCP (stdio, HTTP) | after `sync` for any call that changed the document; on timeout (default 5 s) the reply carries a warning "applied, not yet saved: <status>" instead of failing, because the change did apply |
| CLI | after `sync`, then a clean close (section 6) before the process exits |
| Scripts and tests | after `sync` |

## 6. Saving the project files (the keeper)

The keeper applies durable entries to its shadow `Project` and writes the files:
- **When:** after 250 ms with no new entries, or at most every 2 s while edits continue, and at a clean close. Files
  on disk therefore trail the document by about 2 s at most. Agents should read state through ops (`project.get`); the
  `project.save` op (a small task) forces a save and returns when the files match, for agents about to edit files
  directly and for VC-04 commits.
- **Rendering without reading the disk.** `Project` keeps the text and hash of every file as it last read or wrote it,
  and renders edits against that text (comments and layout survive as today). Rendering and snapshotting are then pure
  functions of memory, and the "changed on disk" check is one hash comparison before a file is replaced: a file
  edited outside Omuzaki is never overwritten; the banner offers to read it back in through an ordinary op
  (`project.reload`), so the change is journalled and undoable.
- **Open transactions are saved too.** The files show what the app shows. A snapshot records which transactions are
  open, so recovery can still roll them back (8.3).
- **Durable before `close`.** Before the keeper asks the writer for the `close` record, it syncs the folders it renamed
  files into, so a clean close implies the files on disk match the close snapshot.
- **A crash during a save** leaves a mix of old and new files, possibly a file and a folder with one name.
  Nothing reads that mix as truth: recovery rebuilds from the snapshot and the journal and writes the files back
  exactly (8.5).

### 6.1 What a save keeps of the files it replaces (`.omuzaki/replaced/`)

A file is replaced only if its bytes are the last-known ones, and the check runs on the file the save actually
displaces (`omuzaki-doc` `disk::replace_checked`). An editor can still hold that file open and write into it after the
check; no check can see a write through a handle that is already open. So the displaced file is **kept**, not deleted:

- **Kept as the same file.** Before the swap the target is hard-linked into `.omuzaki/replaced/<path>.<time>-<pid>-<n>.<hash>.kept`
  (same file system, no bytes copied), where `<hash>` is the BLAKE3 hash of its bytes, which the save has just checked
  are the last-known ones. After the swap that entry is the displaced file, so a late write lands in it. Where the
  file system has no hard links the backup is a verified copy, and after the swap the displaced file itself is renamed
  over it, keeping the name.
- **Pruned by later saves.** Every save deletes `.kept` entries that are at least **10 minutes** old on both the
  monotonic clock (counted from when this process made the entry, or first found one it didn't make) and the wall
  clock (the time in the name), and whose bytes still have the hash in their name: at most as many as were kept since
  the last save, plus **16**, so pruning keeps pace with saves and works off a backlog. A wall clock that jumps
  forward never makes an entry old early, and one stamped in the future waits for the wall clock too. A project's
  first save walks the folder once for entries an earlier process left; after that a save's pruning costs what it
  deletes plus a small fixed amount.
- **Written-to entries are never pruned.** An entry whose bytes no longer match its hash holds edits someone made after
  the save; only a person deletes it. `.displaced` entries (a file that raced a save, or a temporary file an
  interrupted save left) carry no hash and are never deleted automatically either.
- **The residual window.** An editor that holds the old file open for more than 10 minutes after a save and only then
  writes into it can lose that write, once a later save prunes the entry. Editors that write in place do so within
  seconds of a save; a longer grace time is a one-constant change (`RETAIN_UNCHANGED`).
- **No atomic swap, no save over a file.** A file that exists is replaced only by an atomic swap (`RENAME_EXCHANGE`
  on Linux, `RENAME_SWAP` on macOS); where the system or file system has none, the save refuses with
  `StoreError::NoSafeChange` before the file is touched, never falling back to a plain rename. A new file needs a hard
  link or a rename that won't replace, and a deletion the same, or it is refused the same way. The capability table is
  in the `omuzaki-doc` `disk` module.
- **Windows, the one non-atomic cell.** Windows has no atomic swap, so on NTFS a save keeps the original by hard link
  and then replaces it with `std::fs::rename`; an editor's atomic save in the same instant can be overwritten (the
  checked file survives in the `.kept` entry), and the first use in a process says so on standard error. A writer
  holding the file open without delete sharing (most editors) makes the rename fail, and the save reports it. On FAT
  and exFAT (no hard links) the save is refused.
- **What is tested where.** CI (Linux) tests a write through a handle opened before the save: made before the check
  it is refused and nothing is lost, made after the check its bytes are in the `.kept` entry and pruning keeps it. It
  also tests the pruning rules: young entries, changed entries,
  `.displaced` entries and other files stay, and one pass deletes at most the entries kept since the last pass plus
  16 (`owed + PRUNE_CATCH_UP`), which keeps pace with a burst of saves. The Windows behaviour is not tested in CI.
- **Crash safety.** The hard-linked entry's folder is not flushed (that would add a folder sync to every save): after a
  power loss the handles that could write into it are gone too, and what it held when kept is the text the save
  replaced. A copied backup's folder is flushed once, after the displaced file takes its name.

## 7. Snapshots

### 7.1 What a snapshot is

A snapshot is the content of the project folder at one LSN: **every project-owned file**, deduplicated by content
hash, so a big file is stored once however many snapshots hold it. That includes the TOML files of the tree and the files
outside it (`library.lock`, `assets.lock`, `takes/*.track`). Left out: `.omuzaki/`, names starting with `.`, symbolic
links, names that aren't UTF-8 (reported), and **derived paths**: regenerable caches and renders, which the crate that
writes them declares next to its code with `register_derived_path!("<glob>")` in `omuzaki-history-api` (self-registered,
§39 rule 2). Nothing else is ever dropped: a file over 1 GB is included, with a warning naming it.

- **Manifest** (one store object; its BLAKE3 hash is the **snapshot id**), canonical JSON (fields in this order, files
  sorted by path, no whitespace), so identical content always gets the same id:

  ```json
  {"format":1,"schema":3,
   "files":[{"path":"characters/a/face.toml","hash":"…","size":1234,"pack":"…","offset":0},
            {"path":"performances/p/takes/1.track","hash":"…","size":52428800}],
   "assets":["…"]}
  ```
  `pack` and `offset` locate a small file inside a pack; without them the file is its own object, whose hash is the
  file's hash. `assets` lists the asset-store hashes the project references (from `assets.lock`); assets are
  **referenced, never copied**, because the asset store already holds them immutably and deduplicated.
- **Pack:** one object holding the bytes of every file under 1 MiB that changed since the parent snapshot, simply
  concatenated. Unchanged files keep pointing into older packs. One pack per snapshot means one store write (about
  five flushes) however many files changed; one object per file would cost five flushes per file, which is seconds
  for a first snapshot of a 500-file project on NTFS and much more on drvfs.
- **Large files** (1 MiB and over) are objects of their own, so a take is stored once however many snapshots include
  it. They are read whole until `omuzaki-store` streams, which multi-GB files need.
- **Ref** (`snapshots/refs/<UTC time>-<id prefix>.toml`, written atomically): the mutable facts about one snapshot.

  ```toml
  id = "…"                     # the manifest's hash
  lsn = 18234                  # the journal position it represents
  session = "5f0c…"            # the session it was taken in
  created = 2026-10-12T14:32:05Z
  trigger = "auto"             # session, auto, close, pre-migration, pre-restore, pre-recovery,
                               # pre-agent-session, manual or named
  label = "Hair › bangs 9 locks"   # the last transaction's label
  history = "…"                # the history export at lsn (local only; not mirrored by default)
  thumbnail = "…" # optional: the last transaction's thumbnail, for the restore preview
  name = "before the oval face"    # named versions only (VC-01)
  keep_until = 2027-01-10      # pins: pre-migration +90 days, pre-recovery +30 days; absent: retention decides
  [verified]
  at = 2026-10-12T14:32:06Z
  how = "hash"                 # hash | trial-open (VC-09)
  ok = true
  ```
  Several refs may share one id. The **document version** history needs is the ref's `lsn`: `DocVersion`
  restarts at 0 whenever a project opens, so it can't name a state across sessions.
- **Where:** a `DiskStore` rooted at `.omuzaki/snapshots/`, separate from the asset store, so snapshot garbage collection
  can never touch an asset. Its objects get a fixed internal provenance (`Source::Authored`,
  `LicenseRef-Omuzaki-Internal`); they are never published.
- **Two ways to make one.** *Rendered*: from the shadow project's in-memory texts at an LSN (the normal case; the
  bytes are what a save writes). *Raw*: the bytes on disk, unparsed (pre-recovery and pre-migration snapshots, which
  must keep exactly what was there, even a broken or ambiguous folder).
- **Published last.** The keeper writes the pack and large objects, then the manifest, reads each back through
  `Store::get` (which re-hashes), and only then writes the ref. A ref therefore never names an incomplete snapshot.
- **Loading one** reads the manifest and builds a `Project` from its file bytes through the same loader as a folder
  (a file source that is a map instead of a directory), so migrations, layout and validation are the same code.

#### 7.1.1 The `assets.lock` file

`assets.lock` is the project's asset list (spec v3 §7; git design §3.2): one row per asset the project uses, keyed by
content hash, with the provenance that lets a reader check licences without opening the asset store. The snapshot
manifest's `assets` list, the commit view's units and missing blobs, the push advisory and the asset sweep all read it.

- **Format:** TOML, one `[[asset]]` table per row, with the keys in this order: `hash` (64 lowercase hex characters,
  `omuzaki_store_api::ContentHash`), `name`, `licence` (SPDX), `commercial` (`ok`, `no` or `unknown`), `source`, and
  `private_test` (written only when `true`; absent means `false`). Unknown keys are ignored when read and dropped when
  written.
- **Sort rule:** rows are sorted by hash, both when written and when read, so two people who add assets change
  different lines and git merges them cleanly. Equal rows in any order give the same bytes.
- **Text:** LF endings and one trailing newline; a file with CRLF endings reads the same as its LF form, and the
  writer's output is LF, so a caller that must keep CRLF converts after writing (git design §3.3).
- **One owner:** the `omuzaki-assets-lock` crate is this file's only reader and only canonical writer (`AssetsLock::parse`
  and `AssetsLock::to_text`, re-exported by `omuzaki-doc`). Nothing else parses or formats it, so the format cannot drift
  between the loader, git and the snapshot code. The crate does no I/O: callers read and write the text.

### 7.2 When

| Trigger | Snapshot | Notes |
|---|---|---|
| session start | the files as opened (rendered after any migration) | the base the session's journal replays over; reused at no cost when nothing changed since the last one |
| editing | every 10 minutes or 5,000 records, whichever comes first, if anything changed | the keeper raises `wants_mark`; the executor's next append adds the `Mark` |
| clean close | the final state | after it, the next open replays nothing |
| migration | raw, before migrating | pinned 90 days (VC-09) |
| backup restore | the current state, before replacing anything | VC-09 |
| recovery | raw, before writing anything | pinned 30 days |
| bulk agent session | on request (`backup.snapshot`) | the session controls call it (VC-09) |
| named version | on request | pinned until the name is removed (VC-01) |

VC-09's "hourly" and "daily" are retention buckets over these: the 10-minute cadence guarantees at least one
snapshot in every hour with changes.

### 7.3 Compaction and garbage collection

- **The journal:** once a snapshot is published, segments wholly before the *previous* published snapshot of the
  session are sealed and moved to `.omuzaki/log/` (one rename each). Keeping one snapshot of slack means a damaged newest
  snapshot still has a journal to replay from. The live journal is therefore at most two snapshot intervals long.
- **The archive** (`.omuzaki/log/`) is the op log: VC-01's history and §6's provenance read it with the same reader.
  Default retention: 90 days, configurable. Pinned and named versions are never pruned, nor the archive their sessions
  need; a session's archive goes only with its base snapshot (below), so any retained point stays restorable. Settings
  shows the disk use of the journal, the archive and the snapshots.
- **Snapshot retention** (VC-09) deletes refs. Never deleted: the newest verified snapshot, pinned and named
  ones, and the base snapshot of any session whose archive is kept.
- **Garbage collection** sweeps `.omuzaki/snapshots/` for objects reachable from no ref and older than one hour (a grace
  period, so an object written for a ref not yet published is never taken). It needs `omuzaki-store-api` to list and
  remove objects (a task); it runs on the keeper, the only writer of that store.

## 8. Recovery

### 8.1 Clean or not

- **One writer per folder.** Opening for writing takes an OS advisory lock on `.omuzaki/lock` (`flock` on Unix,
  `LockFileEx` on Windows; the OS drops it when a process dies, so a stale lock is never a problem) and writes the pid,
  host and start time into it for the "open elsewhere" message. Limit: a lock taken in WSL on a `/mnt` folder is
  invisible to a Windows process and the other way round.
- **Clean** means the last record of the newest session is `close`. Then the folder is the truth: it opens normally,
  and if its files differ from the close snapshot (a `git pull` or an editor while Omuzaki was closed), that is normal
  and simply becomes the next session's base.
- **Unclean** means the newest session has no `close`: recovery runs before the executor accepts anything.

### 8.2 Reading the journal: torn writes and damage

The reader starts at the segment holding the base snapshot's LSN + 1 (earlier segments are not needed, so old damage
can't block recovery) and reads segments in sequence order. A prepared, unused segment (first LSN 0) or one whose header
CRC fails ends the log; nothing in it was ever acknowledged, because its first flush covered the header. Within a
segment, a frame **ends the log** when:
- fewer than 20 bytes remain, or the length is 0 (a zero-filled tail);
- the length is over 64 MiB or past the end of the file;
- the CRC doesn't match;
- the LSN isn't exactly one past the previous record's.

Nothing after a bad frame is ever applied: after a power cut, any part of the last unflushed batch may have reached the
disk, in any order of sectors, and the first bad frame is the only safe end. A segment that ends exactly at its end of
file continues into the next segment if that one starts one past its last record (a rotation). One exception keeps a
recovery crash-safe: after a bad frame, if the next segment starts one past the last good record and its first record
is `session`, it is a session started by an earlier recovery (8.5), and reading continues there.

The reader then looks past the bad frame for more valid frames. If it finds some, this was damage, not a torn tail: the
report says "the journal was damaged after <time>; N later changes couldn't be trusted", and the raw segment is kept in
`.omuzaki/recovery/`.

### 8.3 Replay and open transactions

1. **Base:** the newest ref of the crashed session whose manifest and objects verify (`Store::get` re-hashes), falling
   back to older ones of the same session down to its base. Load it as a `Project` (7.1) and load its history export.
2. **Read** the records after the base's LSN (8.2).
3. **Close every open transaction, by appending synthetic entries,** so replay itself never decides anything. Newest
   first: if its rollback patch (the inverse of its ops so far, as `Runner::rollback` computes it) applies to the
   replayed document, add `Rollback`; if it doesn't (committed work by someone else was built on it), add a `Commit`
   and label the step "Recovered: <label>". Committed work is never lost; open work is dropped unless committed work
   needs it. The single-actor sessions of the `kill -9` acceptance test always take the first branch.
4. **Replay twice, check once.** The keeper side replays the document (patches only) on a clone of the base, which
   becomes the new shadow. The executor replays the same entries with `Executor::restore(base, history)` and
   `Executor::replay(entries)`, which run the same state code as live operation (apply each recorded patch with its
   `before` check, move steps between stacks, drop dropped steps), so both the document and the undo and redo stacks
   come back. The two documents must be equal; a mismatch is a bug, and recovery stops with the files untouched.
5. **A record that doesn't apply** (its `before` doesn't match) means the base and the journal disagree: replay stops
   before it, and the report says so, as for damage.

Replay is mechanical and needs no op code: a journal written by any earlier build replays in a later one, and an op
that changed behaviour since cannot make recovery drift.

### 8.4 Comparing with the files; the prompt

The recovered tree is compared with the folder as it is now (loaded leniently: a folder that won't parse simply
"differs").

- **Same** (the crash came after a save and nothing changed since): no dialog, a toast: "Omuzaki closed unexpectedly.
  Nothing was lost."
- **Different:** a dialog (a dialog because the choice rewrites files; `docs/design/ux/shell.md` section 15):

  > **Omuzaki closed unexpectedly**
  > Your last change was "Hair › bangs 9 locks" by You at 14:32.
  > **[Restore to 14:32]** (primary) · [Open the files as they are] · Details
  >
  > Both choices keep the other version in Backups.

  Details lists what differs (the parameter diff), any open transactions that were dropped ("an agent's
  unfinished change 'retopo hair' (12 ops) was not kept"), any "Recovered:" transactions, and any damage. "Open the
  files as they are" appears only if the folder loads.
- **Headless** (CLI, MCP, scripts): restore automatically to the last committed transaction and roll back the open
  ones, the same result as the prompt's default (decided 2026-10-12). The CLI prints and MCP returns a notice naming
  what was rolled back and where the pre-recovery state is archived (the pinned pre-recovery snapshot); it is never
  discarded silently. `--recover disk` opts into the files as they are.

### 8.5 Applying the choice, crash-safely

In this order, each step durable before the next:
1. a **raw** pre-recovery snapshot of the folder (pinned 30 days);
2. *restore:* write the recovered files so the folder matches exactly: every file of the recovered project, and tree
   files it doesn't have removed (this also clears a leftover file-and-folder pair), `project.toml` last as in
   `Project::save`; *disk:* nothing;
3. the new session's base snapshot (rendered from the recovered project, or from the folder);
4. a new segment whose first record is the new `session`;
5. copy the old session's last segment, raw, to `.omuzaki/recovery/<UTC time>/` (kept 30 days), truncate it to its valid
   prefix, and move the old session's segments into the archive like any sealed segment, so the op log stays
   continuous.

A crash before step 4 leaves the old journal in charge, so the next open runs the same recovery again and gets the same
result (every step is idempotent and deterministic). A crash after step 4 leaves the new session in charge (8.2's
exception). The lock is held throughout.

### 8.6 Migrations and versions

- Opening an older project: a raw pre-migration snapshot, then migrate in memory (as today), then the session's base
  snapshot is the migrated project, rendered. The files on disk are upgraded by the keeper's first save after an edit.
- Recovering a session written by an older build: replay at that session's schema over its base, then migrate the
  result, like opening an older project. A session's records and its base always share a schema.
- A journal from a newer build: refused, read-only, as in 4.5.

## 9. History (VC-01), branches (VC-02) and restore

- **Persisted history.** A snapshot's ref points at the executor's history export at that LSN: every actor's undo
  and redo stacks (each step: tx, label, combined patch, scopes), the open transactions, and the next `TxId` and op
  `seq` (so ids never repeat across sessions). Opening restores it, so `omuzaki run` in one process and `omuzaki undo` in
  the next work. Agent ids that aren't stable across sessions can't reach their old steps; the agent-session design decides agent identity.
  Steps of actors not seen for 30 days are dropped when a snapshot is taken.
- **Undo depth** is capped per actor (default 1,000 steps); a step that falls off stays in the archive.
- **The op log** of earlier sessions is the archive plus the live journal: the History panel and `history_list`
  page through it with an index by transaction (a task). The in-memory log keeps only the session's recent
  records.
- **Restore to a point** is journalled as the document changes it makes (undo entries with their patches, or
  whatever the restore design settles on); nothing about it is special to the journal. **Any point** in the archive can be rebuilt:
  the newest snapshot of that point's session at or before it, plus the archive's records up to it.
- **Named versions** are snapshots with a `name`, pinned: they survive history trimming and show in Backups too.
- **Branches (the branches design decides; constraints from this design):** one journal per working copy. A branch head is a pinned
  snapshot (`branch = "<name>"` in its ref). The simplest switch is one transaction whose patch is the tree diff
  between the heads (the `history.diff` diff), which makes it undoable, journalled and recoverable with no special case. If
  the branches design wants per-branch undo stacks, the switch also replaces the history, which needs record kind 10 (reserved) carrying
  the target's history export id; that is an additive format change. A merge is one transaction, as the branches design says.
- **Coalescing a drag into one record:** the journal still writes each applied delta (they are real patches,
  and an open drag is rolled back on recovery anyway). If coalescing rewrites the in-memory record, the executor adds a
  skippable entry so the archive's op log shows one record per drag.

## 10. Backups (VC-09): the format reused

- **A backup is a snapshot** (7.1) kept by retention: no second format, no second copy of anything.
- **Ops reach snapshots through a service** (`OpCtx::service`, as `docs/design/ops-api.md` describes), defined in
  `omuzaki-history-api` and implemented by `omuzaki-doc` (sketch):

  ```rust
  pub trait Snapshots: Service {
      fn list(&self) -> Result<Vec<SnapshotInfo>, SnapshotError>;      // ref facts, newest first
      fn take(&self, trigger: Trigger, name: Option<&str>) -> Result<SnapshotId, SnapshotError>;
      fn tree(&self, id: &SnapshotId) -> Result<Value, SnapshotError>; // the document as of that snapshot
      fn restore_outside_tree(&self, id: &SnapshotId) -> Result<(), SnapshotError>; // locks, takes
      fn save_files(&self, timeout: Duration) -> Result<(), SnapshotError>;         // project.save
      fn reload_patch(&self, paths: &[String]) -> Result<Patch, SnapshotError>; // project.reload
  }
  ```
  `backup.snapshot`, `backup.list`, `backup.restore` and `project.save` are ops over it in `omuzaki-ops` (`src/ops/`,
  beside `history.diff`), so MCP, the CLI and the UI get them from the registry like every other op, and
  `omuzaki-doc` registers no ops (it would otherwise need the file-access fence, and it is the store).
- **Retention:** buckets over refs (defaults: 24 hourly, 7 daily, 8 weekly, pins as in 7.2, configurable per
  project), then garbage collection (7.3). Never deletes the newest verified snapshot.
- **Verification:** *hash check*: the manifest, every pack slice and object, and every referenced asset in the
  asset store exist and hash right; *trial open*: load a `Project` from the manifest in memory (7.1), which runs the
  migrations as a dry run; no temporary folder is needed. Results go into the ref's `[verified]`. A failed snapshot
  never counts toward retention and raises a notification.
- **A second location** holds the same layout per project, `<location>/<project id>/{objects,refs}`.
  Copying is resumable by construction: copy reachable objects the target lacks (each verified after copying), then the
  ref, last. Referenced assets are copied too, so a backup survives the loss of this disk. History blobs and the
  archive hold prompts and chat text, so they are not copied unless the person opts in.
- **Restore:** dry-run returns the parameter diff and the thumbnail; apply takes a `pre-restore` snapshot,
  then runs one undoable transaction "Restore backup <time>" whose patch is the tree diff (so the journal, undo and
  recovery need nothing new), and writes the files outside the tree with the exact-match writer of 8.5. Undoing it
  reverts the document; the files outside the tree stay as restored (the executor has no hook to undo effects outside
  the document), and the `pre-restore` snapshot holds their earlier version.

## 11. The open questions this settles

| Question | Outcome |
|---|---|
| write-through saving | **Rejected** (section 2). Saving moves to the keeper; the CLI's `SavedProject` wrapper goes when the CLI opens projects through the journal. Folded into that work. The storage `PatchError` variant is no longer needed: the executor's document does no I/O. |
| interrupted layout-changing save | **Covered by recovery** (6, 8.5) plus a writer that makes the folder match a project exactly. Folded into that writer, P0. |
| replayable log, bounded memory | The journal is the replayable log (physical records with undo, redo, rollback and drop markers). What remains is the caps: undo depth and the in-memory log. |
| one writer, files changed behind our back | The lock is split out as a P0 item (recovery needs it). The remaining part keeps "changed on disk": the keeper's save refuses to overwrite a file whose hash changed, and the files are read back in through an ordinary op (`project.reload`), so the change is journalled and undoable. |
| persisted history | Snapshots carry the history export (9). What remains is the executor half: export, restore and replay. |

## 12. Test plan

Fixtures are synthetic projects (never the sample character or private material).

1. **Format (unit and property, `omuzaki-doc`).** Round-trip every kind. For every truncation point of a valid segment,
   the reader returns exactly the records wholly before the cut. Any single flipped bit yields a prefix and never a
   wrong record. Zero-filled tails, LSN gaps, oversized lengths. A newer format or an unknown required kind is refused.
   Golden v1 segments always decode.
2. **Simulated power loss (deterministic, in process, `omuzaki-test`).** The journal and refs reach files through a small
   file-system seam; the simulator keeps a *durable* image (what was flushed) and a *volatile* one. A crash keeps the
   durable image plus, from the unflushed writes, a seeded random subset of whole sectors and one torn sector. The
   snapshot store is the in-memory mock (durable at once) and the TOML files live in a real temporary folder, which
   only ever trails the durable journal. Crash
   points: any write, between write and flush, rotation, a snapshot between pack and ref, a save half done, and during
   recovery itself (then recover again). Oracle: the same seeded session run on a plain executor. The recovered document
   must equal the state after the last transaction whose commit record is in the surviving journal, which must be at
   or after the last one `sync` acknowledged; the undo stacks must match too. **Mutation checks** (each must fail the
   test): skip `sync_data`; skip the open-transaction rollback; skip the CRC check; replay from the wrong base.
3. **`kill -9` of the real binary.** A seeded 1,000-op session (`part.set_param`, palette and variant ops;
   transactions of 1 to 5 ops; undos and redos; a second actor interleaving in some seeds) drives `omuzaki` over MCP stdio
   in a child process. Kill points: after a seeded op index, and at named **failpoints** (a `failpoints` cargo feature;
   `OMUZAKI_FAILPOINT=<name>:<hit>` aborts the process there) inside the writer, the keeper's save, a snapshot and
   recovery. Relaunch with `--recover restore`; compare the document with the oracle state after the last transaction
   whose commit record survived, which must be at or after the last acknowledged one (with SIGKILL the page cache
   survives, so a commit written but not yet acknowledged may legitimately survive; the oracle reads the journal to
   know which). Then `undo` must undo that transaction. Power-loss variant: truncate the last segment at a seeded offset
   after the last acknowledged record and garble the rest. Determinism: the session, kill point and tear offsets come
   from the seed; the only free variable, how far the writer got after the last acknowledgement, is read back from the
   journal. 20 seeds nightly in at most 5 minutes; 3 per PR. Because SIGKILL keeps the page cache, a missing flush is
   caught by test 2, not this one.
4. **Windows-native**: tests 2 and 3 on a Windows host (`TerminateProcess`), plus a **power-cut test** (a VM
   hard reset under a commit load, then recovery checked against the last acknowledged commit) for the NTFS assumption
   of 5.2. Recorded as unverified until it runs.

## 13. Performance budgets

| What | Budget | Measured by |
|---|---|---|
| Hand-off on the hot path (`append`, plus the `admit` check) | ≤ 2 µs p99 and ≤ 0.5 µs median added to `Runner::run` for a 1 KiB patch; no I/O; no syscall unless the writer sleeps | `omuzaki-ops` `run` bench, with a no-op sink against none |
| Real-time threads | never call the executor or the journal | code review; the threads of §8 have no `Runner` |
| Apply to durable | p50 ≤ 2 ms, p99 ≤ 10 ms at 60 commits/s on a local SSD (ext4, NTFS); drvfs recorded, not gated | journal writer bench |
| Writer throughput | ≥ 20,000 records/s of 1 KiB with flushing | journal writer bench |
| Recovery | a 5,000-record tail over a 500-file project in ≤ 1 s | recovery bench |
| Save | one edited file of the 200-file benchmark project saved and saved back (two saves) under 2 ms of CPU work (render, hash, write) on a CI-class 4-vCPU machine; the device's `fsync` is outside the budget, measured and reported separately, and never traded for speed. Saves run on the keeper (6), never on the UI or real-time threads | `omuzaki-doc` `store` bench, `save` group: on tmpfs (where `fsync` costs nothing) for the CPU work, on the local SSD for the total |
| Open without recovery | within 5% of today's `open` bench; the base snapshot adds ≤ 20 ms on a local SSD when files changed while closed, nothing when they didn't | `omuzaki-doc` `open` bench |
| Snapshot (off-thread) | ≤ 50 ms for a 500-file project with 10 changed files on a local SSD | snapshot bench |
| Apple (estimate until measured on an M-series machine) | interactive apply to ordered p99 ≤ 10 ms with `F_BARRIERFSYNC`; an external acknowledgement waits one group-committed `F_FULLFSYNC` | journal writer bench on macOS |

## 14. Rejected alternatives

- **Write-through saving:** 2 to 25 ms per op per file on the calling thread, not atomic across files (2).
- **A journal written only at commit (whole transactions):** smaller, but a committed transaction planned on top of
  another actor's open changes would not replay, and the op log would lose rolled-back work, which history keeps.
- **Logical records (replay the ops):** replay would need every op's code at its old version, plus the services and
  files it read. Physical patches replay anywhere.
- **SQLite as the journal:** a mature WAL, but a native dependency and a second store format next to the TOML files and
  the content-addressed store, and its durability on drvfs is not better than ours.
- **One store object per file in a snapshot:** about five flushes per file (7.1).
- **Snapshots as copies of the project folder:** no deduplication, slow on drvfs.
- **Trusting the TOML files plus the journal tail as the recovery base:** after a crash in the middle of a save, the
  files are a mix that no LSN describes.

## 15. Maintainer decisions (2026-10-12)

1. **Headless recovery:** CLI and MCP restore automatically to the last committed transaction and roll back open ones,
   with a notice naming what was rolled back; the pre-recovery state is archived, never discarded silently (8.4).
2. **Archive retention:** 90 days by default, configurable; pinned and named versions never pruned; disk use shown in
   Settings (7.3).
3. **Large files:** snapshots include every project-owned file, deduplicated by hash; derived caches and renders are
   excluded by declaration; a file over 1 GB gets a warning and is never dropped silently (7.1).
4. **NTFS new files:** the next segment is created and flushed ahead of time, off the commit path, so a commit never
   creates a file (4.1); a power-cut test joins the Windows-native plan, unverified until it runs (12).
5. **macOS:** a commit an external caller waits on gets `F_FULLFSYNC`, group-committed so concurrent acknowledgements
   share one flush; interactive commits use `F_BARRIERFSYNC` where available, and only they carry the power-loss window
   of 5.3; snapshots and close use `F_FULLFSYNC`. Budgets are estimates until measured on an M-series machine (5.2, 13).

## 16. Work items

All belong to phase 1 (core + character), each with its contract and acceptance checks; those that need only this
design can start when it merges.

| Work | Pri | Needs |
|---|---|---|
| `omuzaki-history-api` + `omuzaki-history-mock`: entries, sink, history export, snapshot refs, `Snapshots` service | P0 | this design |
| `omuzaki-store-api` + `omuzaki-store` + mock: list and remove objects (garbage collection) | P0 | this design |
| `omuzaki-doc`: last-known texts and hashes, in-memory rendering, refuse to overwrite, `Project: Clone` | P0 | this design |
| `omuzaki-doc`: one writer per folder, `.omuzaki/lock` | P0 | this design |
| `omuzaki-ops`: hand entries to the sink, `admit`, marks, `flush_history`; bench | P0 | history API and mock |
| `omuzaki-ops` history `restore` and `replay` | P0 | ops history sink |
| undo depth and in-memory log caps | P1 | history restore and replay |
| `omuzaki-doc`: open a project from file bytes as well as a folder | P0 | doc texts and hashes |
| write the folder to match a project exactly | P0 | doc texts and hashes |
| `omuzaki-doc`: journal format and reader | P0 | history API and mock |
| `omuzaki-mcp`: acknowledge after `sync` | P0 | history API and mock |
| `omuzaki-doc`: journal writer (group commit, flushing, rotation, `sync`, status, failpoints) | P0 | journal format and reader |
| `omuzaki-doc`: snapshots (manifest, pack, refs, load, garbage collection) | P0 | history API and mock; store garbage collection; doc texts and hashes; open from bytes |
| `omuzaki-doc`: the keeper (shadow, saves, snapshot cadence, close, compaction) | P0 | journal writer; snapshots; folder writer |
| `omuzaki-doc`: open and recover | P0 | journal format and reader; keeper; folder lock; folder writer |
| `omuzaki-doc`: history list over the archive (VC-01) | P0 | journal format and reader; keeper |
| `omuzaki-doc`: the `Snapshots` service (VC-09) | P0 | keeper; folder writer |
| `omuzaki-test`: simulated power-loss harness | P0 | open and recover; ops history sink; history restore and replay |
| `omuzaki-cli` opens projects through the journal | P0 | ops history sink; history restore and replay; open and recover; MCP sync acknowledgement |
| `omuzaki-ui`: the relaunch prompt and the saving banner | P0 | open and recover; the UI shell |
| `project.save` op | P1 | Snapshots service |
| changed on disk, `project.reload` as an op | P1 | keeper; Snapshots service |
| `kill -9` acceptance test | P0 | CLI through the journal |
| retention and `backup.snapshot` | P0 | snapshots; keeper; Snapshots service |
| verification (trial open in memory) | P0 | retention; open from bytes |
| second location (exact layout) | P0 | verification |
| the person's own bucket | P1 | second location |
| `backup.list`, `backup.restore` in `omuzaki-ops` | P0 | verification; `history.diff`; Snapshots service |
| the one-screen restore | P0 | backup ops; the UI shell |
| `omuzaki-ui`: Settings → Storage (disk use, retention) | P1 | Snapshots service; retention; the UI shell |
