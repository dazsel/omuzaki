# omuzaki-test

Golden-image harness (spec v3 §34): compare a render with a reference image under a tolerance, version
the references **per branch** without putting images in git, accept intended changes with `--bless`, and report changed
images on the PR.

The renderer itself arrives later; this crate only needs two PNGs, so every render test can use it.

## The library

```rust
use omuzaki_test::compare::{compare, Tolerance};

let report = compare(&actual, &expected, Tolerance { per_channel: 2, max_bad_fraction: 0.001 })?;
report.stats.passed;        // within tolerance?
report.stats.bad_fraction;  // share of pixels differing by more than 2 in some channel
report.stats.max_delta;     // largest single-channel difference (0-255)
report.stats.psnr_db;       // PSNR over RGBA; None = identical
report.diff;                // RgbaImage: bad pixels red over a pale copy of the expected image
```

- **`per_channel`**: a pixel is *bad* when any of its premultiplied R, G, B or its A differs by more than this.
- **`max_bad_fraction`**: pass when at most this share of the pixels is bad. Use it for a few edge pixels that differ
  between GPU drivers; use `per_channel` for rounding noise everywhere.
- Colour is compared **premultiplied**: R, G, B are multiplied by alpha before the difference is taken, alpha is
  compared as it is. Transparent output is premultiplied (spec BR-01) and the colour hidden under alpha 0 is
  meaningless, so two fully transparent pixels always match, and a colour change counts in proportion to how visible the
  pixel is.
- Different sizes are an error from `compare()` (`CompareError::SizeMismatch`), never a pass. The CLI and `check` treat
  it as "images differ": exit 1, a `size mismatch` finding.
- An invalid tolerance (`max_bad_fraction` outside 0.0-1.0 or NaN) is always an error, even when blessing a golden that
  doesn't exist yet.
- Deterministic: integer maths for the diff image, no threads, so the same inputs give byte-identical output.

Tests that compare against the store use `Goldens`:

```rust
let goldens = Goldens::from_env()?;                       // OMUZAKI_GOLDENS, OMUZAKI_GOLDEN_MANIFESTS, OMUZAKI_GOLDEN_OUT
let outcome = goldens.check(&GoldenId::new("face", "game-neutral")?, &image,
                            Tolerance { per_channel: 2, max_bad_fraction: 0.001 }, Mode::from_flag_or_env(false))?;
assert!(outcome.is_ok(), "{outcome:?}");
```

The tolerance argument is what gets **recorded for a new case**. A case that already has a manifest entry is compared
under the tolerance in its entry, so loosening one is a reviewed edit of the manifest, never a quiet change in test code.

## How goldens are stored

A golden is two things, so that it can be versioned per branch without images in git (the same shape as the asset store,
`omuzaki-store-api`: content-addressed, named by BLAKE3 `ContentHash`):

| Part | Where | In git? |
|---|---|---|
| **Manifest**: case name -> image hash + tolerance | `tests/goldens/<suite>.toml` | yes, one per suite |
| **Image**: the reference PNG | `<OMUZAKI_GOLDENS>/objects/<hash>.png`, in practice `$OMUZAKI_ASSETS/goldens/objects` | no (decision 0003) |

```toml
# tests/goldens/render.toml
[front-view]
hash = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262"
per_channel = 2
max_bad_fraction = 0.001
```

- **Per branch.** The manifest is in the branch, so `--bless` on one branch never changes what another compares
  against. A bless never deletes or rewrites an object, so every branch's manifest finds its own images; only
  `omuzaki-test gc` removes objects, and only ones no branch names (see "Verifying and cleaning the store").
- **A bless is a manifest diff.** `--bless` stores the new image, then rewrites that one entry **in the working tree**.
  The PR diff shows exactly which goldens moved (one `hash = ` line each); commit the manifest with the code change.
- **Merge-friendly.** Cases are sorted, one table per case, so two branches adding different cases merge cleanly. Two
  branches that bless the *same* case differently conflict on one line, which is the right thing to resolve by hand
  (keep one hash).
- **Verified reads.** An image read back is hashed and refused if it doesn't match its name; a manifest that names an
  image the store doesn't hold is an error (`blessed on another machine?`), never a silent "new case".
- **Safe under `OMUZAKI_BLESS=1 cargo test`.** Tests run in parallel, so manifest updates take a lock
  (`tests/goldens/<suite>.toml.lock`, git-ignored) and images are written by rename.
- The manifest is generated: comments you add are not kept when it is rewritten. A tolerance is edited by hand.

## Where things live

| What | Where | Set by |
|---|---|---|
| Manifests | `tests/goldens/<suite>.toml` of the workspace around the working directory (each worktree has its own) | `OMUZAKI_GOLDEN_MANIFESTS` (or `--manifests`) |
| Images | `<OMUZAKI_GOLDENS>/objects/<hash>.png` | `OMUZAKI_GOLDENS` (or `--goldens`) |
| Evidence of changes | `<OMUZAKI_GOLDEN_OUT>/<suite>/<name>.{expected,actual,diff}.png` and `.json` | `OMUZAKI_GOLDEN_OUT` (or `--out`); default `<goldens>-out/<name of the workspace folder>`, so each checkout has its own |

`suite` and `name` are omuzaki-core slugs (`a-z`, `0-9`, `-`, `_`), so they are safe file names and TOML keys, and an
evidence `.json` whose names aren't slugs is refused when read back (the report never puts unvalidated text into the PR
comment). CI runs on the same machine, so it shares the image store. Never commit the images (decision 0003).

An empty `OMUZAKI_GOLDENS`, `OMUZAKI_GOLDEN_MANIFESTS` or `OMUZAKI_GOLDEN_OUT` counts as unset (it never means the current
directory).

**Evidence is per checkout.** A passing check deletes its own stale evidence, so a folder shared by several branches would
let branch B's pass erase branch A's failure and the report would say nothing changed. The default folder is therefore
`<goldens>-out/<workspace folder name>`; the project names each worktree after its branch (`192-golden-manifests`), so
that is one folder per branch. The report command resolves the same default from the same working directory. CI that wants
one folder per PR run sets `OMUZAKI_GOLDEN_OUT` to a fresh one such as `$OMUZAKI_ASSETS/goldens-out/pr-<number>`,
so the comment lists only that PR's changes. Re-running into the same folder stays accurate for the tests that ran.

## CLI

    omuzaki-test compare ACTUAL EXPECTED [--per-channel N] [--max-bad-fraction F] [--diff OUT.png]
    omuzaki-test check suite/name ACTUAL [--per-channel N] [--max-bad-fraction F] [--bless] [--goldens D] [--manifests D]
    omuzaki-test migrate [--per-channel N] [--max-bad-fraction F] [--goldens D] [--manifests D]
    omuzaki-test verify [--goldens D] [--manifests D]
    omuzaki-test gc [--apply] [--keep-manifests-of REV]... [--goldens D] [--manifests D]
    omuzaki-test report [--out DIR] [--artifact-url URL]      # PR-comment Markdown for the evidence folder

Exit codes: **0** match (or blessed), **1** images differ (including different sizes; for `verify`, a missing or damaged
image), **2** the harness hit a problem (bad file, no store, bad arguments or tolerance, `verify` with no manifest to
check, or `gc` stopping because it cannot be sure what is wanted), so CI can tell "the render changed" from "the harness broke".

### Blessing

`--bless` (or `OMUZAKI_BLESS=1`, for whole test suites) accepts the image as the new golden: it stores the image and
points the manifest entry at it, in the working tree. It only rewrites a golden that is **missing or differs beyond the
tolerance**: noise inside the tolerance is not a change and leaves the manifest byte-identical. The golden being replaced
is kept in the evidence folder as `<name>.expected.png` (and stays in the object store, so reverting the manifest is the
undo). Only `1` blesses; `OMUZAKI_BLESS=0` or `true` do not.

Blessing also **repairs** a case whose image is missing from the store or damaged (the error says "bless it again"):
the new image is written over the bad object and the entry rewritten. A plain check reports a lost or damaged image as an
error (exit 2) and never as a new case.

Tolerance flags only matter for a case with no manifest entry yet. Given for an existing case with a different tolerance,
`check` says so on stderr (`note: ... --per-channel 5 is ignored`) and compares under the manifest's.

Blessing only moves *your branch's* manifest, so it is safe from any branch; bless from the PR that intends the change and
say so in the PR. Commit `tests/goldens/*.toml` with it.

### Migrating old goldens

Goldens used to be `<goldens>/<suite>/<name>.png`, one folder shared by every branch. Run once, in the checkout whose
manifests should receive them:

    omuzaki-test migrate --goldens $OMUZAKI_ASSETS/goldens

Each old image is copied byte for byte into `objects/`, and an entry is added to `tests/goldens/<suite>.toml` (the
tolerance flags are what get recorded; old goldens had none). It is safe to repeat. A case the manifest already has with a
different image is reported as a conflict and left as the manifest has it (exit 1). `objects` is reserved for the object
store, so a legacy suite of that name is listed as skipped, not migrated. The old files stay put; delete them
once the manifests are committed.

## Verifying and cleaning the store

The store is append-only and outside git, which makes two things easy to miss: a manifest can name an image that was
blessed on another machine or lost from the store (it surfaces only when that case happens to run), and nothing says
which objects are still wanted.

**`omuzaki-test verify`** reads every manifest in the manifest folder and checks that each image it names exists and hashes
to its name. It lists each failing case by name (`MISSING: render/front-view: ...`, `CORRUPT: render/side-view: ...`)
and exits 1; exit 0 means every case has its image. A missing or empty manifest folder (a wrong `--manifests`) exits 2
and names the folder: checking nothing is not a pass. It reads each distinct image once, so it is cheap enough to run on
every PR. Repair a case by blessing it again (see above).

**`omuzaki-test gc`** deletes images that nothing wants. It is a **dry run by default**: it lists what it would delete with
the count and bytes, and changes nothing until you add `--apply`. An image is kept if the manifest in any of these names it:

- what **every ref** points at: local and remote-tracking branches (any PR branch is one), **tags** (an annotated tag counts
  as the commit it names), the **stash**, and any other ref git keeps;
- **every stash entry**, dropped-from-the-stack ones still in the stash log included, with the commits a stash is built
  from (so a manifest stashed with `-u` counts);
- the checked-out commit of every worktree (a detached HEAD included);
- the manifest **files** of every worktree and of the folder you pointed it at, so a bless you have not committed yet is
  safe;
- any revision given with `--keep-manifests-of REV` (a commit id nothing else names, say; repeatable). It only ever keeps
  more, it cannot narrow the list above.

Whatever the manifests say, an image **younger than 7 days is never deleted**, and the 7 days are not configurable. The
age is counted from the **newest** of the file's modification time (which a bless renews even when the image was already
stored), its change time (`ctime`: the kernel sets it whenever the file changes, a copy's creation included, and no tool
can carry it over) and its creation time where the file system has one. So an image that `rsync -a`, `cp -p` or a
restore from backup put in the store an hour ago is an hour old, however old its modification time says it is; the
listing says so (`0 days old; last stored 30 days ago, but the file itself changed since`). Any `chmod`, rename or
link made to an image renews its change time too, which only ever keeps it longer.

`gc` **warns** (on stderr, and goes on) when it can't trust the clocks, and keeps the image concerned:

- a clock reading of an unwanted image (`mtime`, `ctime` or creation time) is more than 5 minutes in the future, because
  a clock was set wrong, or the file came from a machine whose clock was. The warning names the file and the field; the
  image waits until that time has passed;
- the file system reports neither a change time nor a creation time for an unwanted image, so a copy could not be told
  from the original;
- the repository's **last `git fetch` was more than 3 days ago**, none is recorded (a fresh `git clone` writes none) or
  every record is dated in the future. A teammate's newest PR branch is invisible until fetched, and its images would
  look unwanted. The dry run still lists what it would delete; **`--apply` refuses** (exit 2, nothing deleted) until
  `git fetch --all` has been run. A repository with no remote has nothing to be behind. The record is the modification
  time of `FETCH_HEAD`, which git writes **per worktree** (`.git/FETCH_HEAD` for the main checkout,
  `.git/worktrees/<name>/FETCH_HEAD` for a linked one) while the branches a fetch updates are shared, so the newest
  record of any worktree counts (one dated in the future is ignored beside a past one). `FETCH_HEAD` is rewritten by any
  fetch, also one of a single branch, so this is a reminder to fetch and not proof that every branch is here: fetch with
  `git fetch --all`.

At most 10 warnings are printed, then a count of the rest.

**`gc` fails closed**: whenever it cannot be sure, it deletes nothing and exits 2 with a message that says how to fix it.
A deletion tool that guesses is worse than one that stops. It stops when:

- git fails for any ref, commit or manifest, prints something unexpected, or a manifest on any branch (or in any worktree,
  committed or not) cannot be parsed;
- the clone is **shallow**, or a remote's fetch refspec does not cover every branch (`git clone --single-branch` and
  `--depth` both do that), because branches it never fetched would look unwanted. Fix: `git fetch --unshallow`, then
  `git remote set-branches origin '*'` and `git fetch origin`. `gc` reads only what is in your clone, so it cannot know
  about a branch on the remote you have not fetched: run `git fetch --all` first so a teammate's newest PR branch counts;
- `--manifests` (or `OMUZAKI_GOLDEN_MANIFESTS`) is spelled in another letter case than the file system or any scanned commit
  uses (`Tests/Goldens` for `tests/goldens`). An NTFS mount is case-insensitive and opens both spellings, but git matches names
  exactly, so the wrong one would find the files on disk and none of the committed manifests. Pass the exact spelling;
- any folder or file it needs exists but cannot be read (no permission on a worktree or the folder around it, a manifest
  that cannot be inspected). Only a clean "no such file or folder" counts as missing, and a worktree whose folder is gone
  is read by its HEAD only. A **locked** worktree whose folder is gone stops it too: the drive may just be unplugged;
- no manifest exists anywhere (a wrong `--manifests` would otherwise make the whole store look like garbage), or the
  manifest folder is not inside a git checkout.

Right before deleting, `--apply` **reads everything again** (refs, stash, worktrees, manifests) and compares the set of
images they name with the one the plan was made from. If they differ (a fetch, a checkout, a bless, a stash or a deleted
branch happened in between) it deletes nothing and says so; run it again. Each image is also skipped if its file changed
since the scan (a bless that picks an old image again mid-run, a copy over it, a `chmod`: any of its three times moved),
so one a test is using keeps. Only regular `<hash>.png` files are ever touched; a symbolic link of that name is left
alone and so is what it points at. Run it from any worktree; the answer is the same.

    omuzaki-test gc                # list what would go
    omuzaki-test gc --apply        # delete it

**The test clock.** The kernel stamps every file with the real moment of its last change and nothing can set that back,
so a test can only make an image look old by moving `gc`'s clock forward. The test-suite sets
`OMUZAKI_GC_TEST_NOW=<whole seconds since 1970>` for that, and `gc` honours it **only for a store that holds a
`.omuzaki-test-clock` file in its root** (next to `objects/`), which the test fixtures create and nothing else does. On any
other store, a set variable makes `gc` stop with exit 2 and the message `OMUZAKI_GC_TEST_NOW is set, but <store> has no
.omuzaki-test-clock file ... unset OMUZAKI_GC_TEST_NOW`, on a dry run as well as with `--apply`, and nothing is listed or
deleted. Without the guard the variable would be a hole in the 7-day floor: set a year ahead, it makes an image written
seconds ago look old. Do not create the marker in a real store.

## PR comment

A CI step can build the report and keep one comment on the PR (found by
its hidden marker and author, updated in place on each push). A run that changed nothing posts nothing. The table lists
at most 100 images, failures first, so the comment stays under GitHub's 65,536-character limit; the folder has them all.

**Why the comment links a folder instead of showing the pictures.** Options considered for a repo whose images are not public:

1. *Attach images through `gh`*: `gh` has no attachment API; the web UI's uploader is undocumented and not for scripts.
2. *Commit the images to a branch and link them*: puts heavy assets in git history, which decision 0003 forbids, and
   grows without bound.
3. *Host them publicly*: leaks work in progress, and some test characters are local-only material.
4. **Name the folder (chosen).** The images already sit in the assets folder, next to the goldens, so the comment shows the Windows
   path (`$OMUZAKI_ASSETS/goldens-out/pr-12`) a maintainer can open in Explorer. In CI, the workflow can also run
   `actions/upload-artifact` on that folder; the script then adds a link to the run's artifacts page
   (`GITHUB_RUN_ID` is set), which is visible to repo members only.

The comment itself carries what is needed to decide: which images, the bad-pixel share, max delta and PSNR, and for
each image the golden's hash before and after (shortened to 12 characters; the manifest diff in the PR has them in full).
The goldens are `<hash>.png` in the object store folder the comment names.

## Not here yet

Headless wgpu rendering of the fixed scenes (the other half of the spec's golden-render harness) needs the renderer
crates first; they will call `Goldens::check` with the images they produce.
