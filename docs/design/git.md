# Git integration: access, clean commits, binaries, the commit view and ops (VC-04)

**Status:** proposed (2026-10-13).
**Feature:** VC-04. **Decision record:** ADR 0045. **Spec:** v3 §7 (project layout), §29
(VC-04, VC-09), §37, §40.
**Builds on:** `docs/design/branches.md` section 10 (Omuzaki branches are not git branches, `.omuzaki/` is never committed,
a commit can be imported as a point, `kind = "import"`); ADR 0021 and `docs/design/journal.md` (the keeper, the
`project.reload` op, snapshots, the second location); ADR 0003 (heavy assets stay out of git); ADR 0020 and
`docs/design/agent-sessions.md` (scopes, person-only ops); `crates/omuzaki-ops-api/src/scope.rs`.
**Worked example:** section 13 (three commits, the CRLF case, a missing blob).

This page explains the decisions so an implementer or a reviewer can disagree with a reason. Git is optional: a
project without a repository loses nothing, and no other feature depends on git.

## 0. The decisions in brief

1. **Omuzaki runs the person's own `git` program as a separate process.** Not `gix`, not libgit2. Their credentials,
   ssh agent, hooks, config and LFS just work, the licence stays `external`, and the web build has no git at all (2).
2. **"Commits cleanly" is a short list of guarantees the writer already nearly makes** (3): stable bytes on save, a
   save never rewrites an unchanged file, one entity per file, canonical key order, a file's own line endings kept,
   rename-friendly ids. Omuzaki adds `.omuzaki/.gitignore` (`*`); it never edits the person's `.gitignore`, `.gitattributes`
   or git config. A `.gitattributes` is **offered**, never written unasked (3.3).
3. **Binaries never travel in git.** `assets.lock` carries hashes; a clone gets the bytes from the VC-09 second
   location, and a missing blob shows a placeholder that names its hash (4). LFS is not built (open question 1).
4. **The commit view** lists the folder's commits, imports a commit as a point on demand to get its changed units
   (`history.diff`) and a thumbnail (`render.preview`), cached by commit id under `.omuzaki/`, computed off the UI
   thread (5).
5. **Ops:** `git.status`, `git.log`, `git.show`, `git.import_commit` for anyone with `read`; `git.commit`, `git.push`,
   `git.pull` for a person only. Pull is fast-forward only; anything else becomes an import plus `branch.merge` (6).
6. **Files changed by git while Omuzaki runs** go through `project.reload` as one undoable transaction, never over the
   keeper's own writes; conflict markers are never parsed or overwritten (7).
7. **Tests** run one conformance suite against `FakeGit` and against real git over a local bare repository (8).

## 1. What has to be true

- A person who never types `git` sees nothing about it. A person who does sees `git status` show exactly their edits
  and `git diff` read as a diff of TOML, not of noise.
- Saving, opening and closing a project never dirties a clean checkout (idempotent saves).
- Omuzaki never reads a credential, never stores one, never prompts for one itself, and never runs a git command the
  person did not cause (no background fetch).
- Every git call is cancellable, off the UI thread and bounded in time. A slow remote never freezes the studio.
- Agents can read git history; only a person changes the repository or talks to a remote.

## 2. Access: the person's own git, as a process

| | System `git` (process) | `gix` | libgit2 via `git2` |
|---|---|---|---|
| Licence (read 2026-10-13) | GPL-2.0-only (git's `COPYING`); used as a separate program: register verdict `external` | `Apache-2.0 OR MIT` (gitoxide README) | `git2` is `MIT OR Apache-2.0` (git2-rs README), the bundled libgit2 is GPL-2.0 **with a linking exception** (libgit2 `COPYING`) |
| Credentials, ssh, hooks | the person's own, unchanged | credentials via helper support is partial, **push and commit hooks are unchecked** on gitoxide's own roadmap (README, 2026-10-13) | Omuzaki would implement credential callbacks (it would see secrets); no hooks |
| Config, `.gitattributes`, `autocrlf`, LFS filters | exactly as the person's git does | not all of git's filter drivers; no LFS (`gix-lfs` is a name placeholder) | filters partial; no LFS |
| Build / ship cost | none (needs git on the machine) | pure Rust, large dependency tree | C library, a `cc` build, and a cross-build step for Windows |
| Maturity | decades | "usable", possibly incomplete (README) | mature, but a second implementation of git's behaviour |

**Recommendation: system git.** The only costs are that git must be installed and that each call is a process spawn
(a few ms to tens of ms, measured by the conformance task, never on the hot path). The person who commits already has
git; the person who does not has no use for the feature. Rejected:

- `gix`: push and hooks are not done, and "what git did" would differ subtly from the person's own git on `status`,
  filters and rebases. Revisit when its roadmap shows push, hooks and filters done (the trait makes it a drop-in).
- `git2`: the linking exception makes it shippable, but it moves credentials into Omuzaki and gives an app that behaves
  differently from the person's git. Not worth a C dependency for features git already provides.

**Detection.** `omuzaki-git` looks for `git` on `PATH` (the person's own, and the usual install folders on Windows) and
runs `git --version`. The floor is git 2.30 (an engineering floor: it has `--porcelain=v2`, `-z` everywhere and
`--end-of-options`; the conformance run checks it). A missing or older git gives `GitError::Missing { found }` and the
panel says "Install git to use this" once, with the version found. Nothing else in Omuzaki looks for git.

**The web build does nothing.** `omuzaki-git` is native-only (it spawns a process). On the web, ops answer
`GitError::Unsupported`, the commit view is not offered, and the project folder is whatever the browser's storage is.

**How it is called (the rules for `omuzaki-git`):**

- argv array, never a shell; `-C <dir>`; user input only after `--end-of-options` or `--`; revisions that start with
  `-` are refused; every caller path is passed as `:(literal)<path>` (so a name with `*` or `[` is a name, not a glob).
- machine formats only: `status --porcelain=v2 -z --branch`, `log -z --format=<fixed>`, `ls-tree -r -z`,
  `cat-file --batch`. `LC_ALL=C`, `GIT_OPTIONAL_LOCKS=0` for reads (a status never takes the index lock, so it cannot
  collide with the person's own `git`).
- `GIT_TERMINAL_PROMPT=0`, `GCM_INTERACTIVE=never` and ssh `BatchMode=yes` for push and pull: if auth needs a prompt it
  fails with `GitError::AuthRequired`, and the panel says "run `git push` once in a terminal to sign in". The
  person's helper, agent and keychain are inherited and never touched.
- timeouts: reads 30 s, push and pull 10 min; every method takes a `&CancelToken` (section 9), polled while the child runs; on cancel or timeout the **whole process tree** is ended, not just git (hooks, ssh and credential helpers are its
  children): git starts in its own process group on Unix (`setsid`, then `killpg` with SIGTERM, a 2 s grace, SIGKILL) and
  in a Job Object on Windows (`TerminateJobObject`), and the call returns `Cancelled` only after the group has been
  reaped.
- stderr is kept (last 4 KiB) after removing `user:password@` from any URL; it is shown in the panel's detail, logged
  with that redaction, and never put in an op result an agent sees (an agent sees the error kind).

## 3. "Commits cleanly": the exact guarantees

### 3.1 Bytes

- **G1 Deterministic.** The same document saves to the same bytes: keys in the schema's order (maps sorted by key),
  shortest round-trip floats, one trailing newline, UTF-8 without a BOM, no timestamps or machine paths inside project
  files. (Spec §7: one entity per file, stable key ordering.)
- **G2 Idempotent saves.** The keeper writes a file only if its bytes would differ from what is on disk; it never
  touches a file's mtime otherwise. Open then close leaves `git status` clean. A test asserts this on every fixture.
- **G3 One entity per file.** A character, a scene, a camera, a light rig, a show: each its own file, named by a
  stable **id**, never by the display name. Renaming a character changes one field, not a path, so git sees an edit and
  not a delete plus an add. Ids are never reused.
- **G4 Line endings are the file's own.** The writer detects a file's line ending from its existing bytes (CRLF if the
  first line break is `\r\n`, else LF) and writes the same; a new file gets LF. The reader accepts both. So a Windows
  checkout with `core.autocrlf=true` (CRLF in the working tree, LF in the repository) stays CRLF in the folder and git
  normalises on `add`: no whole-file diffs, no "everything changed" after a save. Mixed endings in one file are
  rewritten to the first line's ending (reported once as a warning in the reload banner, because it is a change).
  Omuzaki never converts a file's endings on its own. The CRLF case is in the worked example.
- **G5 Parsed, not byte, comparison.** Anything that compares trees (`history.diff`, `branch.merge`, import)
  compares parsed `Value` trees, so LF against CRLF is never a difference. (A snapshot hashes raw bytes, so an
  imported LF commit and a CRLF folder have different snapshot ids with identical content; nothing depends on those
  matching.)
- **G6 Rename detection.** Git's own (similarity 50%) works because G3 keeps entity files small and their contents
  stable when a name changes. Omuzaki never moves a file just to reorder it. Directory layout changes only in a schema
  migration, which is its own commit by design: Omuzaki tells the person "project updated to schema N, commit this on its
  own" in the reload banner.

### 3.2 What is ignored, and what is committed

- **`.omuzaki/.gitignore` containing `*`** (the small task already filed) is the only ignore rule Omuzaki writes. It lives
  inside the folder it hides, so the person's own `.gitignore` is never edited and `.omuzaki/` can never be added by
  `git add -A`. It is created with `.omuzaki/` and recreated on open if missing.
- **`merges.toml` is committed** (ADR 0025, branches section 10). It is one entity with stable keys. On another
  machine its point ids are unknown and skipped, which costs extra conflicts and never data. Ignoring it would make
  every clone re-offer every merge, which is worse. A project that dislikes it can add it to its own `.gitignore`.
- **`assets.lock` is committed** (text, hashes and provenance, sorted by hash so two people adding assets merge
  cleanly).
- **Takes (`performances/*/takes/*.track`) are binary and are not committed by default:** Omuzaki writes
  `performances/<id>/takes/.gitignore` with `*.track`, for the same reason as `.omuzaki/` (inside its own folder, never
  the person's file). They travel with the VC-09 second location like asset bytes (open question 2).
- **Everything else in the folder** (the TOML tree, `shows/`, `variants/`) is the person's to commit.

### 3.3 The offered `.gitattributes`

If the folder has a repository and no `.gitattributes` mentions its files, the commit view offers once: "Add a
`.gitattributes` so line endings stay put?" It is a normal project file, shown in full before it is written, written
only on a click (a person-only op, `git.offer_attributes`, a small task):

```
*.toml  text
assets.lock  text
*.track  binary
```

`text` alone (no `eol=`) lets git normalise to LF in the repository and leaves the working copy to the person's
`autocrlf`/platform default, which G4 then respects. `eol=lf` is **not** offered: it would rewrite every file in a
Windows working tree on the next checkout and look to Omuzaki like a mass external change. Declining is remembered in
`.omuzaki/` (not committed) and the offer never repeats.

## 4. Binaries: how a clone gets the files `assets.lock` names

Options considered:

| Option | Verdict |
|---|---|
| Tracked files in git | **Rejected.** ADR 0003: heavy assets never in git; a 2 GB repository with unprunable history. |
| git-lfs | **Not built in v1.** MIT (git-lfs `LICENSE.md`, read 2026-10-13), and it would work with system git, but it needs a host that supports it, a second set of bytes beside the store, and `.gitattributes` patterns for a folder Omuzaki does not own. Open question 1. |
| The VC-09 second location (a folder, a drive or the person's own bucket) | **Recommended.** The store already holds the bytes content-addressed and deduplicated. `assets.lock` says which hashes; the location says where. A clone points at the same location once (project setting `locations`, never committed because it may hold a bucket name). |

**On clone/pull,** for each hash in `assets.lock` that the local store lacks, `store.fetch(hash)` asks the project's
locations in order; with none configured the asset is `Missing` and stays so. No git operation moves a blob.

**A missing blob** (the store lacks a hash, at HEAD or at an old commit being previewed) is never an error:

- the unit lists it as `missing asset blake3:9f2c…41ab (hero-face.png)` with the provenance row from the lock (name,
  licence, source), so the person knows what to fetch;
- a render preview substitutes the same placeholder the studio uses for a deleted asset (a flat checker material or an
  empty frame labelled with the short hash) and marks the thumbnail `incomplete`, with the count; an incomplete
  thumbnail is cached under a key that includes the set of missing hashes, so it is recomputed when blobs arrive;
- nothing blocks: `history.diff` compares hashes, not bytes.

## 5. The commit view

**Panel (visual, built last):** the current folder's commits, newest first (`git.log` with the folder as pathspec, so
a monorepo shows only this project's commits), the current Omuzaki branch's name above it (branches section 10), per
commit: a thumbnail, the subject, author, time and its changed units. Selecting a commit shows the unit list and the
compare view (branches section 6.4) between its parent and itself; "Import as point" and "Merge into current" are
buttons.

**Data flow (all off the UI thread, lazy, per visible row):**

```
git.log (ids)  ->  git.import_commit(c)  ->  history.diff(parent point, c point)  ->  render.preview(c point)
   cheap             makes a point            changed units (cached)                   thumbnail (cached)
```

- **Import.** `git.import_commit {rev}` calls `Git::tree_at(rev)`, builds a snapshot from the text files (files over
  8 MiB or not valid UTF-8 TOML are skipped and reported), and records a point with `kind = "import"` on branch
  `git/<short ref>` (created on first use; it has no lane and is never the folder branch). It is idempotent: the point
  is keyed by the commit id, so a second import returns the first. Points are immutable and garbage collected when no
  branch reaches them (branches section 4.6), so browsing history costs store space only until GC.
- **Changed units** per commit: `history.diff {from: StateRef(point of first parent), to: StateRef(point of commit)}`
  (the branches design), the same diff as every other compare. A root commit diffs against the empty tree; a merge commit diffs
  against its first parent. The result (the unit names and counts, not values) is cached.
- **The lock file is compared separately.** `history.diff` covers the document tree, which excludes `assets.lock`
  (branches 4.5), so it cannot report lock rows. The import keeps each commit's `assets.lock` bytes as a file of the
  point's snapshot (snapshots hold every project-owned file, journal section 7), and the commit view computes an
  **`assets.lock` unit** itself: parse the parent's and the commit's lock (the same parser the project loader uses),
  compare rows by hash, and report added, removed and provenance-changed rows. Provenance for a hash at that commit
  (name, licence, source, shown in the missing-blob placeholder) is read from **that commit's** lock, so history shows
  what was true then. Both are kept in the units cache; a lock that fails to parse is reported as one `lock unreadable`
  unit, never an error.
- **Thumbnails.** `render.preview {state: StateRef(point), camera: focus character, size: 256 x 256}` at the
  branch's matched camera recipe. Cached at `.omuzaki/git/<project id>/<commit id>/{units.json,thumb-256.png}` (never
  committed: `.omuzaki/.gitignore`), keyed by commit id plus the preview recipe version and the missing-hash set (4).
  Computed on demand for visible rows only, newest first, one at a time on the render pool at low priority,
  cancelled when the row scrolls away; a cached row costs one file read.
- **Budget (a target, to be measured by the thumbnails task; no measurement exists yet):** per thumbnail, p95 on the
  reference machine with warm shaders: import 150 ms (a typical project: tens of TOML files under 1 MiB), build 150 ms,
  render and encode 100 ms: **400 ms in all**; on a software adapter (CI lavapipe) 2 s. A 50-row first page is
  therefore about 20 s of background work that never blocks input, and a revisit is instant from cache.
- **Invalidation.** The cache is content-addressed by commit id, so history rewrites (rebase, amend) simply orphan
  entries; `.omuzaki/git/` is trimmed by LRU at 256 MiB.

## 6. Operations, scopes and agents

| Op | Does | Scopes | Agents |
|---|---|---|---|
| `git.status` | branch, ahead/behind, changed project files (read-only) | `read` | yes |
| `git.log {range, limit}` | commits touching the folder | `read` | yes |
| `git.show {rev}` | one commit: the changed files and units | `read` | yes |
| `git.import_commit {rev}` | makes a point (5) | `read` | yes (see below) |
| `git.commit {message}` | stage the folder's project files and commit | `fs` | **no** (`OpError::person_only`) |
| `git.push {remote}` | send commits to a remote | `net`, `publish` | **no** |
| `git.pull {remote}` | `fetch` then `merge --ff-only` | `net`, `fs` | **no** |

- **Why agents read only.** A commit is an act in the person's name, with their git identity (git's own `user.name`;
  Omuzaki never sets it), and a push puts the project on someone else's machine. Both are `person_only`, not
  `needs_person`: no approval card exists for them, because the approval would be an agent asking a person to press the
  button the person already has. An agent that wants work saved uses branches (`branch_checkpoint`) and tells the
  person.
- **`git.import_commit` is the one write an agent may make.** It adds an immutable point and changes nothing in the
  folder, the document or git; it is how an agent compares or merges a colleague's commit (it then needs
  `edit:*` for the merge itself). It counts against the session's step and rate limits, and an unreferenced point is
  garbage collected.
- **`git.commit`.** First `sync` the keeper so the folder equals the document. Then `git add -A -- . ':(exclude,literal).omuzaki'` and
  `git commit -F - -- . ':(exclude,literal).omuzaki'` with the message on stdin: only the folder's files, never other staged work in the
  repository, and **never anything under `.omuzaki/`, even if it is already tracked**: `.omuzaki/.gitignore` stops only
  untracked files, so a `.omuzaki/` file that someone once added with `git add -f` stays indexed and would otherwise ride
  along in a folder-wide commit. Caller-supplied `paths` under `.omuzaki/` are refused (`BadPath`). `git.status` lists
  such files as `private_tracked` and the panel says "`.omuzaki/...` is tracked by git; run `git rm -r --cached .omuzaki`"
  (Omuzaki never edits the index itself). The person's hooks run (a hook failure is `HookRejected` with its output); `.omuzaki/` cannot be staged.
  Nothing to commit is `NothingToCommit`, not an empty commit. The op takes an optional list of paths so the panel can
  commit a subset; paths must be inside the folder.
- **`git.pull` is fast-forward only.** It never merges, so it never writes conflict markers into project files. If the
  histories diverged: `GitError::NotFastForward { ahead, behind }`; the panel offers "Import their commit" (5) and
  "Merge into current" (`branch.merge` with `base` set to `Git::merge_base`, branches section 10). Omuzaki's merge is
  by unit and cannot leave a half-merged file; plain git would.
- **The licence gate and push.** `publish` is required, so a team's VC-11 `Review` policy applies to a push like any
  other publish. The gate itself runs **advisory, not blocking**: no asset bytes leave in a push (only
  `assets.lock` hashes and provenance), and the gate protects *shipping* of `no`/`unknown` material (spec §7). The
  confirmation lists every `no` and `unknown` row of `assets.lock` ("these names and licences will be in the repository")
  so the person decides; a team may make that block through VC-11. A push never uploads blobs, so the bytes-level gate
  has nothing to check (open question 3).
- **Audit.** Each op is an `OpRecord` with the actor, so VC-12's audit trail sees commits and pushes. Seam: who may
  push (a role) belongs to the roles and audit design; here only the scopes above.

## 7. Interplay with the journal, the keeper and `git checkout`

- **Closed while git changed files** (a pull, a checkout, a stash): the next open treats the new files as the folder
  branch's next state (ADR 0021, branches section 10); nothing special.
- **Open, and a file changed behind the keeper's back**: the keeper's save refuses to overwrite a file whose
  bytes are not the ones it last wrote, and the change is read back with `project.reload`, one journalled, undoable
  transaction labelled "Files changed outside Omuzaki (N)". Undo restores the document and rewrites the files to what it
  had, which is what the person asked for by pressing undo.
- **A `git checkout` or `pull` in flight.** Files change one at a time. Omuzaki watches the folder, debounces for 750 ms
  and also waits while the repository's index lock exists. The path is **resolved through git once per open**
  (`git rev-parse --git-path index.lock`, run from the folder), never assumed to be `.git/index.lock`: in a linked worktree
  `.git` is a file and the lock lives in the worktree's own metadata folder, and a project nested in a larger repository
  has its repository above it. Each wake is then a `stat` of that path, not a git call, so it reloads once from a settled tree and
  never from half a checkout.
- **Conflict markers** (the person ran `git merge` themselves): a file that fails to parse is not reloaded and is
  never overwritten. The document keeps its last good state; the banner names the files ("`characters/a/face.toml`
  has git conflict markers; fix it in git and Omuzaki will read it"); the keeper skips that path until it parses.
- **A different branch checked out** (git, not Omuzaki): the folder branch's files now differ; it is just a large
  external change, one reload transaction. Switching Omuzaki branches (branches 5.5) never calls git.
- **Journal cursor.** `git.commit` records nothing in the journal; a commit is not a Omuzaki point. (A person wanting a
  Omuzaki point uses a checkpoint. Auto-checkpointing on commit is rejected: two ledgers of "saved states" confuse more
  than they help.)

## 8. Tests

- **`FakeGit`** (`omuzaki-git-mock`): an in-memory repository (commits, a worktree map, remotes as other `FakeGit`s),
  scriptable failures (`Missing`, `AuthRequired`, `NotFastForward`, `HookRejected`, slow calls for cancellation) and
  a recorder of calls. It has no code from `omuzaki-git` (the mock is the independent oracle, `crates/README.md`).
- **Conformance** (`omuzaki-git/tests/conformance.rs`): one suite of cases, written against the `Git` trait, run twice:
  against `FakeGit` and against real git over a local bare repository (`git init --bare` in a temp dir; the remote is
  a `file://` path; no network). Environment isolated: `GIT_CONFIG_GLOBAL`/`GIT_CONFIG_SYSTEM` pointed at an empty file,
  author and committer set by `-c`, `GIT_TERMINAL_PROMPT=0`. Cases: status on clean/modified/untracked/conflicted and with an indexed `.omuzaki/` file;
  log ranges and limit; `tree_at` with a binary and an oversized file; commit (subset, nothing to commit, hook
  rejection with a temp hook); push, pull fast-forward and diverged; revisions starting with `-`; a path with spaces
  and non-ASCII; cancellation and timeout of **every** method (a slow fake; for real git, a hook that spawns a long-running
  grandchild) return `Cancelled`/`Timeout` and leave no process of the tree running (checked by pid);
  the index-lock path in **both layouts** (a linked `git worktree add` checkout and a project in a subfolder of a larger
  repository); a commit with an already-indexed `.omuzaki/` file changed (it must not be in the commit);
  an `assets.lock` that changes between commits; stderr redaction of `https://user:pass@host`. Synthetic projects only
  (`omuzaki-test` fixtures); no real person's material.
- **Clean-commit test** (already filed): save, open and close a fixture with `autocrlf` on and off; `git status` stays
  clean; CRLF stays CRLF; `.omuzaki/` is unaddable.
- **Pull/checkout test** (already filed): an external change under a running keeper becomes one reload.
- Missing git fails the suite unless `OMUZAKI_ALLOW_NO_GIT=1`, so CI cannot pass silently without exercising git.

## 9. API surface (names final)

```rust
// omuzaki-git-api (API crate)
pub trait Git: Send + Sync {
    fn status(&self, dir: &Path, cancel: &CancelToken) -> Result<GitStatus, GitError>;
    fn log(&self, dir: &Path, range: LogRange, limit: usize, cancel: &CancelToken) -> Result<Vec<Commit>, GitError>;
    fn show(&self, dir: &Path, rev: &str, cancel: &CancelToken) -> Result<CommitDetail, GitError>;
    fn tree_at(&self, dir: &Path, rev: &str, cancel: &CancelToken) -> Result<CommitTree, GitError>;   // files to import as a point
    fn merge_base(&self, dir: &Path, a: &str, b: &str, cancel: &CancelToken) -> Result<Option<CommitId>, GitError>;
    fn commit(&self, dir: &Path, message: &str, paths: &[PathBuf], cancel: &CancelToken) -> Result<CommitId, GitError>; // a person only
    fn push(&self, dir: &Path, remote: &str, cancel: &CancelToken) -> Result<(), GitError>;                            // a person only
    fn pull(&self, dir: &Path, remote: &str, cancel: &CancelToken) -> Result<PullOutcome, GitError>;                   // a person only
}
/// Cloneable flag the host sets to stop a call; `omuzaki-git` polls it while the child runs and kills the child.
pub struct CancelToken { /* Arc<AtomicBool>: new(), cancel(), is_cancelled() */ }
pub struct Commit { pub id: CommitId, pub parents: Vec<CommitId>, pub author: String, pub at: Seconds, pub subject: String }
pub enum LogRange { Head, Rev(String), Between { from: String, to: String } }
pub struct CommitTree { pub id: CommitId, pub entries: Vec<TreeEntry> }
pub enum TreeEntry { File { path: String, bytes: Vec<u8> }, Skipped { path: String, size: u64, why: SkipReason } }
pub enum GitError { Missing { found: Option<String> }, Unsupported, NotARepo, BadRev, NothingToCommit, BadPath { path: String },
    NotFastForward { ahead: u32, behind: u32 }, AuthRequired, HookRejected { output: String }, Timeout, Cancelled,
    Failed { code: Option<i32>, stderr: String } }
```

`GitStatus` lists the folder's changed files (path, kind), `private_tracked` (tracked paths under `.omuzaki/`), the current git branch, ahead/behind and whether a merge
is in progress. Every method takes the folder, not a path inside it; paths in results are relative to it and use `/`.
`CommitId` is the full object id as a string (SHA-1 or SHA-256 repositories both work). Calls are blocking; the host
runs them on a worker thread and passes a `CancelToken` it sets when the op is cancelled or the panel row scrolls
away (`OpCtx` carries none today; the token is an explicit argument, so the trait needs no change to `omuzaki-ops-api`).

## 10. Rejected alternatives

- **Mapping Omuzaki branches to git branches** (branches section 13): two histories to keep in step.
- **Committing automatically** on checkpoint, close or schedule: a commit is the person's act.
- **Bundling git** (portable git, or `gix` for everything): the person's credentials and hooks would not apply.
- **Reading credentials to push for the person:** never.
- **A background fetch** to show "behind by 3": it spawns a process and touches the network unasked; refresh is a
  button.
- **LFS or tracked blobs by default:** section 4.
- **Writing `eol=lf` attributes:** section 3.3.

## 11. Seams with the sibling Phase 5 designs (named, not decided here)

- **Identity and trust:** commit authorship is git's `user.name`/`user.email`, not a Omuzaki identity. If
  the identity design wants signed commits or a link between the two, it adds an input to `git.commit`, not a new path.
- **Roles and audit:** which role may `git.push`/`git.pull`; the audit entry shape.
- **Comments:** if comment files live in the project folder they are committed like any entity (G1, G3); if they
  live in `.omuzaki/` they are not. Either works here; the comments design decides.
- **Shared library sources:** a source that is a git repository can reuse the `Git` trait read-only
  (`tree_at`). That is the library design's call; this design only promises the trait is not tied to the open project.

## 12. Tasks

`omuzaki-git-api` and `omuzaki-git-mock`; `omuzaki-git`; `git.status`/`git.log`/`git.show`; `git.import_commit`
(after the branch-listing task); commit thumbnails; `git.commit`/`git.push`/`git.pull`; the commit view panel; the offered
`.gitattributes`. Already filed: the `.omuzaki/.gitignore` and clean-commit test; the git pull and checkout test.

## 13. Worked example

A synthetic `demo-show.omuzaki` with three commits (made-up ids and times; the thumbnail numbers are the section 5
targets, nothing was rendered). `git.log`, newest first:

```json
[{"id":"c3d9e1f4…","parents":["b27a05c8…"],"author":"Ada Test","at":1760367600,"subject":"Warmer key light"},
 {"id":"b27a05c8…","parents":["a1f00e92…"],"author":"Ada Test","at":1760281200,"subject":"Ada: new face, hero texture"},
 {"id":"a1f00e92…","parents":[],"author":"Ada Test","at":1760194800,"subject":"Start the show"}]
```

| Commit | Changed units (`history.diff` against the first parent, plus the lock diff) | Thumbnail (`.omuzaki/git/<project id>/<commit>/thumb-256.png`) |
|---|---|---|
| `a1f00e92` | root, against the empty tree: character `ada` (3 files added), scene `intro` (2 added); `assets.lock`: 2 rows added | complete, target 400 ms |
| `b27a05c8` | character `ada`: face (2 params), look (1 param); `assets.lock`: `hero-face.png` replaced | **incomplete**, 1 missing asset |
| `c3d9e1f4` | scene `intro`: lighting (`key.color`, `key.intensity`) | complete |

Cached `units.json` for `b27a05c8` (names and counts):
`{"against":"a1f00e92…","units":[{"unit":"character:ada","params_changed":3},{"unit":"assets.lock","rows_changed":1}],"missing":["blake3:9f2c…41ab"],"recipe":1}`

**CRLF.** A Windows clone with `core.autocrlf=true` has `characters/ada/face.toml` as CRLF on disk and LF in the
repository. Omuzaki changes `mouth.width` and saves: only that line differs and every line still ends `0d 0a`;
`git diff --stat` shows `face.toml | 2 +-`. A save with no change writes nothing (G2), so `git status` stays clean. A
new file is LF. Importing `b27a05c8` gives LF bytes for the same file, and `history.diff` reports the 2 real
parameters, not line endings.

**Missing blob.** On a fresh clone with no second location, HEAD's `assets.lock` names `blake3:9f2c…41ab` and the store
lacks it. The row for `b27a05c8` reads `missing asset blake3:9f2c…41ab hero-face.png (CC0-1.0, made in-house)` (from
that commit's own lock); its thumbnail uses the checker placeholder with a "1 missing" badge and nothing errors. Once
the project points at the backup folder the hash is fetched, the cache key (which includes the missing set) changes and
the thumbnail is recomputed once.
