---
title: "The updater: delta updates, channels, rollback, models by hash (SE-08)"
summary: "delta updates, channels, rollback, models by hash (SE-08)"
area: design
---

# The updater: delta updates, channels, rollback, models by hash (SE-08)

**Status:** proposed (2026-10-13). It adds a new API crate (`omuzaki-update-api`); no `omuzaki-ops-api` change (section 12 reuses existing scopes). **Decision record:** ADR 0035. **Spec:** v3 §31 (SE-08, SE-04), §10 (`installer.size`), §34 (QG-06). **Builds on:** `docs/design/settings.md` (D5, D9, D10, ADR 0026), `docs/design/ai-api.md` (`Fetcher`, `FileSource`, ADR 0017), `docs/design/journal.md` (pre-migration snapshots), ADR 0016.

The page explains the decisions so an implementer or reviewer can disagree with a reason. The types are sketches; the tasks in section 12 finalise them in code.

## 0. The decisions in brief

1. **Our own thin updater, built from small audited parts:** a signed manifest, per-file binary deltas, and a staged swap into versioned folders. No updater framework. Velopack, with a custom signed `UpdateSource`, is the runner-up; a spike (UP-0) tests it before the delta work starts (section 1).
2. **We verify, not the transport.** Every manifest and every package is checked against an ed25519 key embedded in the app, with a monotonic `sequence` and an expiry. A hash that arrives in the same unsigned feed as the file proves nothing.
3. **The check is a plain GET of a static file.** The path holds the channel, OS and architecture. No query string, no identifier, no version in the request: the client compares versions itself.
4. **Install never hot-swaps and never runs while live.** A package is verified, unpacked beside the running version, and made current at the next start by a tiny launcher. Install and rollback are person-only.
5. **Rollback is a pointer move.** The previous version stays on disk; a new version that dies twice before the UI is ready is rolled back by the launcher on its own.
6. **Models travel separately:** a signed catalogue through `FileSource`, `min_app_version` gating, hash verified, opt-in. User-supplied models are never touched.
7. **Check-on-start has two designed values, default off** until the maintainers pick in UP2 (section 13).

## 1. The mechanism

Criteria, from the issue: delta updates (the spec); a beta channel to join and leave; rollback; no telemetry (the check may send version, OS, architecture and channel, never an identifier); signatures checked by us; Windows, macOS and Linux; licence (nothing non-commercial); the 150 MB installer budget; Rust integration off the frame thread; maintenance.

Sources were read on **2026-10-13** from the build box. `unverified` marks what a primary source could not confirm from here (documentation sites were unreachable; crate sources, README and LICENSE files were read).

| Option | Delta | Channels | Rollback | No identifier | We verify signatures | Platforms | Licence | Maintenance |
|---|---|---|---|---|---|---|---|---|
| **Velopack** 1.2.161 (crate; `velopack/velopack`) | yes: `DeltasToTarget`, `MaximumDeltasBeforeFallback` (crate `src/manager.rs`) | yes: `ExplicitChannel`, `AllowVersionDowngrade` (same file) | no rollback API found in the crate; the installer keeps older packages (docs unverified) | **no**: the feed request is `releases.<channel>.json?localVersion=&id=&stagingId=`, and `stagingId` is a per-install id created by the locator (`src/sources/http.rs`, `src/locator.rs`) | **no**: the feed carries `SHA256` and the client compares it (`src/manager.rs`); no signature check in the crate. Authenticode and notarisation are the only signatures | Windows, macOS, Linux (README) | MIT (LICENSE read) | active: crate 1.2.161 released 2026-09-29 |
| **Velopack with our own `UpdateSource`** (the public trait in `src/sources/mod.rs`: `get_release_feed(channel, app, staged_user_id)` and `download_release_entry`) | as above | as above | none in the crate or in the apply step (`src/bins/src/commands/apply.rs` at the pinned commit applies the latest full package it finds; read 2026-10-13); we would build the pointer and crash guard ourselves | **yes**: our source over `Fetcher` receives `staged_user_id` and need not send it | **yes**: our source verifies our signature on the feed and the asset hash before returning, so the unsigned-feed problem goes away | as above; Linux is AppImage-shaped (`locator.rs`: "path to the AppImage file on Linux"); other layouts unverified | MIT | as above |
| **cargo-packager** + `cargo-packager-updater` 0.2.3 | no: the updater downloads the whole installer (source and README: no delta, channel or rollback terms) | no (endpoint templating only) | no | yes if we avoid headers; the URL carries version, target and arch (README) | **yes**: minisign (`minisign-verify`) | packager: NSIS, MSI, DMG, `.app`, deb, AppImage (README) | Apache-2.0 OR MIT | **stale**: last release 2025-07-21 |
| **MSIX and App Installer** (Windows) | block-level differential updates (docs unverified) | no channel concept (docs unverified) | the OS keeps the previous package (docs unverified) | n/a | OS-checked package signature, a certificate we buy | Windows only | OS feature | OS |
| **Sparkle** (macOS) | yes, deltas (docs unverified) | yes, appcast channels (docs unverified) | no (docs unverified) | system profile is optional (docs unverified) | **yes**: EdDSA (WinSparkle README confirms the shared scheme) | macOS only; Objective-C framework | MIT plus bundled third-party notices (LICENSE read) | active (docs unverified) |
| **WinSparkle** | none mentioned in README (unverified) | no | no | n/a | **yes**: EdDSA, `win_sparkle_set_eddsa_public_key` (header read) | Windows only; C DLL | MIT (COPYING read) | active (copyright 2026) |
| **Ours**: signed manifest, per-file delta, staged swap | yes | yes | yes | yes, by construction | yes | all three | our code plus MIT, BSD and Apache parts (section 9) | ours |

**Choice: ours.** The deciding facts:
- **Correction from review (the first draft said Velopack needed a fork; that was wrong).** Velopack's stock `HttpSource` sends `localVersion`, `id` and a persistent `stagingId`, and its client compares a SHA256 from the same unsigned feed as the file. But `UpdateManager::new` accepts any `UpdateSource`, and a custom one over our `Fetcher` can omit the identifier and verify our signature and the asset hash before handing the file back. So privacy and signature checking are **not** reasons to reject Velopack; the row above shows it with an adapter.
- What the adapter does not give, and we would build ourselves in either world: **rollback and a crash guard** (nothing in the crate or in `apply.rs`), **holds** (we would call apply only at start), the **result-tree check** (`tree_sha256`) and **a pointer we control**. Applying is done by Velopack's separate `Update` executable, which owns the install layout and the packages folder; its internals beyond `apply.rs` are `unverified`. Our rollback pointer and launcher guard need that layout under our control, and we could not confirm from here that Velopack permits it.
- Packaging goes through Velopack's `vpk` tool (unverified: its runtime and licence were not readable from the build box), a new tool in the release pipeline.
- cargo-packager's updater has no delta (the spec requires it) and has not been released in 15 months.
- MSIX, Sparkle and WinSparkle are each one platform, so three mechanisms, three signing schemes and three UIs.
- What we need is small: the hard parts (signature, binary diff, hash, atomic rename) are solved by single-purpose crates, and our needs add the parts no framework has: models, `Fetcher`, settings, the live guard.

**Decision, and the spike that can overturn it.** Ours stays the recommendation because rollback, the launcher guard and holds must be ours either way, and they need the install layout. But the adapter route would save the delta, staging and most launcher work (UP-C, UP-D, part of UP-E), so it is checked **before UP-C starts**: task **UP-0**, a spike with a written verdict, answers on Windows and Linux (macOS where possible): (1) can apply be deferred to the next start by our launcher; (2) does Velopack keep the previous version, and can our pointer and guard sit on its layout; (3) what installer shapes does Linux get; (4) what `vpk` needs in CI and its licence; (5) does a custom `UpdateSource` over the fake server pass the no-identifier test. Five yes answers: adopt Velopack with our adapter and signed manifest, and shrink UP-C to UP-D's remainder. Any no: build ours as in section 14. UP-A and UP-B are needed in both worlds.

**Revisit if** section 14's task D or E costs more than three times its estimate, or if Windows Authenticode or macOS notarisation (UP2) forces an installer technology that insists on its own updater.

**Installers are a separate decision (UP2).** The updater replaces the app's files after install; creating the first installer is `cargo-packager` (Apache-2.0 OR MIT, supports the six targets above) or another tool. Neither choice changes this design.

## 2. Shape

```
 UI / CLI / MCP ── ops update.* ──► Updater (omuzaki-update-api trait)
                                       │ events, status
                omuzaki-update ──────────┼── Fetcher (the app's HTTPS client, ai-api.md §8)
   verify · delta · stage · rollback   └── Hold tokens from live subsystems (section 7)
                    │
        <install root>/versions/<v>/  +  state.toml  ◄── omuzaki-launcher (starts the right version, boot guard)
```

- **`omuzaki-update-api`** (new, with `omuzaki-update-mock`): traits and plain data only, no network, no disk. Depends on `omuzaki-core` for `Subscription`. **`omuzaki-update`** implements it. **`omuzaki-launcher`** is a separate tiny binary (no egui, no wgpu, a few hundred KB) that picks the version to run.
- **Off the frame thread:** `check` and `download` run on the update worker (a plain thread owned by `omuzaki-update`), never on the frame, audio or executor threads. Progress is a rate-limited event (at most 4 per second). Delta apply is CPU- and disk-bound, so it runs at low priority and in chunks with a cancel check between files.
- **`Fetcher`** is the one HTTPS client trait (ADR 0017; it stays in `omuzaki-ai-api`, and `omuzaki-http`'s `HttpFetcher` implements it). Range resume, size caps and "no network stack below the app" apply to updates exactly as to models. `omuzaki-update` takes an `Arc<dyn Fetcher>` in its constructor; the app builds the updater its **own `HttpFetcher` with `.user_agent("omuzaki-update/1")`** (the default `Omuzaki/<version>` would put the version in every request), and the model-catalogue fetch uses that same instance. `omuzaki-update-api` is plain data and traits and needs no `Fetcher`.

## 3. The contract

```rust
pub trait Updater: Send + Sync {
    fn status(&self) -> UpdateStatus;
    fn check(&self, channel: Channel) -> Result<Option<Release>, UpdateError>;
    fn download(&self, release: &Release, progress: &dyn Progress) -> Result<(), UpdateError>;
    fn stage(&self, release: &Release) -> Result<(), UpdateError>;   // applied at the next start, never hot-swapped
    fn rollback(&self) -> Result<(), UpdateError>;                   // takes effect at the next start
    fn hold(&self, why: HoldReason) -> Hold;                         // live subsystems take one (section 7)
    fn healthy(&self) -> Result<(), UpdateError>;                    // idempotent; writes the marker the launcher reads (section 6)
    fn subscribe(&self) -> Subscription<UpdateEvent>;
}
pub enum UpdateStatus {
    Unavailable { reason: Unavailable },                 // managed install, read-only root, dev build
    Idle { last_checked: Option<Timestamp> },
    Available { release: Release },
    Downloading { version: Version, percent: u8 },
    Staged { version: Version, deferred_by: Vec<HoldReason> },   // waits for the next start
    CheckFailed { at: Timestamp, reason: CheckFailure },         // Expired, Replay, BadSignature, Offline (explicit checks only)
    RolledBack { from: Version, reason: RollbackReason },        // shown once
}
pub struct Release { pub version: Version, pub channel: Channel, pub sequence: u64, pub package: PackageKind,
                     pub download_bytes: u64, pub installed_bytes: u64, pub notes_url: Option<Url> }
pub enum PackageKind { Delta { from: Version }, Full }
pub enum Channel { Stable, Beta }
```

Decisions where this differs from the original sketch:
- **`notes_id` is dropped from `Available`.** D10 ships release notes inside the binary and shows them after the update; a notes id for an app not yet installed would need a network fetch SE-04 rules out. `notes_url` is an optional page the app opens through the OS handler (`Url` checks syntax only).
- **`hold` is added.** A subsystem that must not be interrupted (section 7) takes a `Hold`; the updater never has to know about streams or renders, so `omuzaki-update-api` depends on no other subsystem (dependency inversion).
- **`healthy` is added.** The app calls it once, when the main window is up and `startup_plan` has resolved (D9). It is idempotent and only writes the marker file the launcher reads; it never blocks. It returns `Result<(), UpdateError>`: callers must handle a marker-write failure (log it, and retry later if the window is still up), because the launcher counts a trial whose marker is missing as failed.
- **`UpdateStatus::Unavailable`** covers installs we must not touch (a distro package, winget, a read-only root, a debug build). Section 5.
- **`stage` requires a downloaded and verified package** and is idempotent. The sketch's separate "hash and signature verified before staging" is enforced twice: at the end of `download`, and again by `stage` before it unpacks (the file could have changed on disk in between).

`UpdateError` variants (all values, no panics): `Offline`, `Http(status)`, `Expired { at }`, `Replay { have: u64, got: u64 }` (sequence went backwards), `BadSignature { key_id }`, `UnknownKey`, `HashMismatch { file }`, `TooLarge`, `DiskFull { need: u64 }`, `DeltaBase { missing: Version }`, `Busy(Vec<HoldReason>)`, `NothingToRollBack`, `Unavailable(Unavailable)`, `Io(..)`. `Progress` is `fn report(&self, done: u64, total: u64)` plus `fn cancelled(&self) -> bool`, the same shape as `FileSource::get`.

## 4. The wire: manifest, signatures, trust

**Layout on the host** (static files; any CDN or bucket; hosting is UP2):
```
<base>/v1/keys.json           (+ .sig by the root key)
<base>/v1/<channel>/<os>-<arch>/manifest.json     (+ .sig by the release key)
<base>/v1/<channel>/<os>-<arch>/<version>/full.tzu, <from>-to-<version>.dzu     (hashes in the manifest)
<base>/v1/models/catalogue.json (+ .sig by the catalogue key)
```
`channel` is `stable` or `beta`; `os` is `windows`, `macos`, `linux`; `arch` is `x86_64` or `aarch64`.

**What a check sends:** one GET of `manifest.json` and one of `manifest.json.sig` (or one request for both if the host supports it). Nothing else: no query string, no cookie, `User-Agent: omuzaki-update/1` (from the updater's own `HttpFetcher`; no app version in it), no id, no Authorization. The server learns IP, TLS fingerprint, channel, OS and architecture, which is within the issue's allowance, and *less* than it allows: the version is compared on the client. (The unavoidable IP address is said plainly in the setting's help text.)

**Manifest** (JSON, signed as the exact bytes served; verify before parsing, so there is no canonicalisation to get wrong):
```json
{ "format": 1, "channel": "stable", "epoch": 1, "sequence": 412, "issued": "2026-11-01T10:00:00Z", "expires": "2026-11-15T10:00:00Z",
  "latest": { "version": "1.4.0", "min_from": "1.0.0", "notes_url": "https://…", "yanked_below": "1.2.3",
    "full":   { "file": "1.4.0/full.tzu", "bytes": 98123456, "installed_bytes": 241000000, "sha256": "…", "tree_sha256": "…" },
    "deltas": [ { "from": "1.3.2", "file": "1.4.0/1.3.2-to-1.4.0.dzu", "bytes": 14200000, "sha256": "…" } ] } }
```
- `epoch` and `sequence` together are a per-channel counter that only rises, compared as the pair `(epoch, sequence)`. The client stores the highest accepted pair per channel in `state.toml` and refuses anything lower (`Replay`). The `epoch` is set by the **root**, in `keys.json` (below), never by the release key: a manifest whose `epoch` is not the current one in `keys.json` is refused. That bounds the damage of a stolen release key: it can sign `sequence = u64::MAX`, but only within the current epoch, and the root can end the epoch (see Rotation). This is the downgrade defence: an attacker who can replay an old, validly signed manifest cannot walk a client back to a vulnerable version. `version` alone is not enough, because a re-issued manifest for the same version (a yanked build) needs a new sequence.
- `expires` stops a freeze attack (serving a stale manifest forever). CI re-signs the unchanged manifest with a new `issued`, `expires` and `sequence` daily; expiry is 14 days. A manifest past `expires`, by the system clock, is **not** "up to date": the check ends in `UpdateStatus::CheckFailed { at, reason: Expired }`, shown in Help and the status area as "Update information is out of date (expired 15 Nov). The update server may be down or blocked, or this computer's clock may be wrong. This does not mean you are up to date." No update is offered from it. A wrong clock therefore shows a visible, explained state, and a freeze attack cannot pass for "nothing new". An automatic check treats `Offline` silently, but shows `Expired`, `Replay` and `BadSignature` (as a badge, never a modal).
- `min_from` forces a stepping stone (the client on an older version sees the stone release first); `yanked_below` marks versions with a known fault: a running version below it gets a stronger "Update recommended" status (never forced).
- `tree_sha256` is the hash over the sorted list of `(path, sha256)` of the installed tree: it is what `stage` verifies after unpacking, so a delta that produces any wrong byte is caught.

**Keys, and the plan to rotate them.** Three roles, ed25519 (`ed25519-dalek`, already in the tree at 2.2.0): **root** (offline, signs only `keys.json`), **release** (signs manifests; lives in CI secrets or an HSM, UP2), **catalogue** (signs the model catalogue; a separate key so a leak of one does not sign the other).
- The app embeds the root public keys (the current one and a spare, to allow a root change) and an initial `keys.json`. A fresh `keys.json` (key id, role, public key, `not_after`) signed by a root key is fetched **before** each manifest (a few hundred bytes) and replaces the stored set when its own `keys_sequence` is higher. It also carries the current `epoch`.
- **Rotation:** publish a `keys.json` with the new release key added, sign manifests with both for one release cycle, then drop the old one. **Compromise of the release key:** the root key publishes a `keys.json` that drops the key and **raises `epoch`**. A client that fetches it stops trusting the old key at once and, because the stored pair has an older epoch, **resets every channel's watermark**, so legitimate manifests from the new key (new epoch, `sequence` restarting at 1) are accepted even after a poisoned `sequence = u64::MAX`. A poisoned manifest of the old epoch is refused as `Replay`. Because `keys.json` is fetched first, a client never evaluates a manifest against a stale epoch. **Root compromise** needs a fresh installer carrying the spare root key; the plan and the custody of the offline key are an owner question (section 13).
- Signatures are domain-separated: the signed bytes are `b"omuzaki-update-v1\0" || role || b"\0" || file bytes`, so a manifest signature cannot be replayed as a catalogue signature.
- Why not minisign: the format is fine, but `minisign-verify` is verify-only, so signing would need another tool in CI; the key set and domain separation above need our own framing anyway.

**Code signing and notarisation are a separate outer layer (UP2).** They make the OS accept the installer and the executables (Windows SmartScreen, macOS Gatekeeper). They do not replace our check: a notarised binary from a compromised host is still a valid binary. The updater's rule is the reverse: bytes that fail our signature are never unpacked, whatever the OS would say. On macOS the unpacked bundle must be byte-identical to the signed and notarised one, so delta apply reproduces files exactly and `tree_sha256` covers `_CodeSignature` (verified by a test with a fake bundle; real notarisation is an UP2 task).

## 5. Install layout, staging and the launcher

```
<install root>/                 per user: %LOCALAPPDATA%\Omuzaki · ~/Library/Application Support/Omuzaki · ~/.local/share/omuzaki
  omuzaki-launcher(.exe)          the only file the OS shortcut points at; never replaced by an update (see below)
  state.toml                    written only by the launcher and the updater, atomically: current, previous, pending, attempts, bad[]
  versions/1.3.2/  1.4.0/      one complete app tree per version (macOS: a full signed Omuzaki.app per version)
  staging/1.4.0.partial/        being unpacked; deleted on start if found
  downloads/                    verified packages by hash (FileSource blobs), pruned after staging
```
- **Why folders and a pointer:** Windows cannot overwrite a running executable, macOS code-signed bundles must not be modified in place, and a pointer makes rollback and crash recovery the same small operation. `state.toml` is a few lines.
- **The launcher** is replaced only by a new *installer*, not by an update; it is deliberately tiny, rarely changes, and a one-line change to it is a release-blocking event. It reads `state.toml`, applies a pending switch (below), runs the boot guard, then starts `versions/<current>/omuzaki` and waits. Linux package installs, winget and the like get no launcher and no updater (`Unavailable { reason: Managed }`), as does a root the app cannot write.
- **Stage** (`Updater::stage`, runs on the worker):
  1. Check holds (section 7); with any hold, state is `Staged { deferred_by }`, and steps 2 to 5 still run because they do not touch the running version.
  2. Check free space for `installed_bytes` plus 10 percent (`DiskFull` otherwise).
  3. Unpack into `staging/<v>.partial/`: for a delta, each changed file is `bspatch(old file from versions/<current>/, patch)`, each new file is zstd-decoded, and each file's `sha256` is checked as it is written. Delta failure (wrong base hash, any mismatch) falls back once to the full package, then fails with `HashMismatch`.
  4. Recompute `tree_sha256`, compare with the manifest, `fsync` the tree, rename to `versions/<v>/`.
  5. Write `state.toml` with `pending = <v>` (atomic replace, `fsync`).
- **Apply at next start (the launcher):** if `pending` is set and no process holds `state.toml.lock`, set `previous = current`, `current = pending`, `pending = none`, `attempts = 0`, `trial = true`, write atomically, start. Nothing running is touched, so there is no hot swap and no half-applied state.
- **Kill-during-apply recovery** is the point of this shape. A kill before step 5 leaves a `.partial` folder, deleted at the next start; before the rename in step 4 the same; after step 5 but before the launcher's write, `pending` is still set and the launcher retries; during the launcher's atomic write, the old or new file is whole (write-then-rename, `fsync` both). No state needs a second program to repair. Tested in section 11.
- **Retention:** `current`, `previous` and at most one staged version (three folders). Cleanup deletes **only versions no longer referenced** by `current`, `previous` or `pending` in `state.toml`, and only after the new `current` has been `healthy` (below): the version two back is deleted, never the one `previous` points at, so manual rollback always has its target. A full version is about 250 MB installed (guess to be replaced by the first real build); the cost is shown in Settings > Storage.

## 6. Rollback

**Manual** (`update.rollback`, Help > Roll back to 1.3.2): person-only. Sets `pending = previous` and marks the version rolled back from as `bad[]` (the updater will not re-offer it until a *higher* version arrives, so rollback is not undone by the next check). Applies at the next start, with the same holds as install.

**Automatic: dies twice at start.** The launcher reuses the *shape* of the settings opening guard (D9: a marker durable before the risky step, cleared after, at most one attempt per start), but at a different level, because the D9 guard needs `session.toml` and the settings store, which a crashing new build may not even reach:
- **The launcher never kills a slow start.** The first start after an update can be slow (shader caches, models), so there is no timeout. `attempts` rises before each trial start and is cleared by `healthy()`; a start that ends before `healthy()` counts as one failure, including when the person closes a hung app.
- On a trial start the launcher increments `attempts` in `state.toml` (synchronous, `fsync`) *before* it starts the app.
- The app calls `Updater::healthy()` (via the launcher's pipe or a marker file) when the main window is up **and** `startup_plan` has resolved (D9: Welcome shown, or the project opened). That clears `trial` and `attempts` and lets cleanup delete the unreferenced older version (the one two back); `previous` itself stays.
- If the launcher finds `trial = true` and `attempts >= 2` (the app ended before `healthy()` on two trial starts: it crashed, exited non-zero, or the person closed a hung app), it sets `current = previous`, adds the failed version to `bad[]`, records `rolled_back_from`, and starts the old version. That version shows `RolledBack { from, reason }` once ("1.4.0 failed to start twice, so Omuzaki went back to 1.3.2"). **No more than one automatic rollback per update**, so there is no loop. If `previous` is missing or also fails twice, the launcher starts nothing silently: it shows its own plain message with the log folder, and the person can reinstall.
- The two guards compose: D9 covers "this build crashes opening *this project*" (it offers Welcome with a notice); the launcher guard covers "this build cannot reach the UI at all". A crash during project open after `healthy` is D9's, not the launcher's.

**What rollback does to the rest** (the updater touches only `versions/` and `state.toml`):
- **Settings (D5):** never migrated down. The older build sees keys stored at a version it does not know and keeps them (D5's rule: a key at a newer version is kept, and unknown keys are kept), uses defaults for those keys in memory, and merge-on-write leaves them byte-identical; when the person updates again, the new build reads them. As a cheap second line, the first start of a new version copies the settings files (a few KB: `settings.toml`, `machine.toml`, `session.toml`, profiles) into `backup/<previous version>/`; the last two are kept. Nothing reads them automatically; Settings > Storage can restore one.
- **Projects:** a project saved by the newer build with a newer schema is refused by the older one with the existing refusal (`StoreError::BadSchema`, the "schema newer than this build" card of D9), never opened lossy. The journal takes a raw pre-migration snapshot before any migration (journal.md), so the person can restore the project as it was before the new build migrated it. The rollback confirmation says so in one sentence when a recently opened project has a newer schema.
- **Models:** independent. The model cache is content-addressed and shared by all app versions; the registry keeps every installed manifest and has its own rollback (ML-01). After a rollback, a model that requires a newer app (`min_app_version`) is listed as disabled with the reason, not deleted.

## 7. Never while live

- A subsystem that must not be interrupted calls `Updater::hold(HoldReason::Streaming | Recording | Rendering | Capturing | ProjectSaving)` when it starts and drops the returned `Hold` when it ends (RAII; a leaked `Hold` dies with the process, which is also the next start). `omuzaki-update-api` knows nothing else about those subsystems.
- **While any hold exists:** `download` may continue (it is network and disk only, and low priority), `stage` may unpack (it touches nothing running), `update.install` still records the intent; but "Restart to update" is disabled with the reasons, and the launcher applies `pending` only at the **next start**, which by definition has no hold. The app never restarts itself: Restart is a person's button, refused (`Busy`) while a hold exists.
- No update dialog appears over a Perform screen: `Available` shows as a badge in Help and the status area, never a modal. Check-on-start never runs while a hold exists, and an explicit `check` is allowed (it is one small GET).
- Auto-rollback is the one launcher action that may happen without the person; it happens only at start, when nothing is live by construction.

## 8. Beta, and models

**Beta.** `update.channel` (`Machine`, agent `Never`, applies next start, ADR 0026) is `stable` (default) or `beta`. Beta versions are SemVer pre-releases of the next stable (`1.4.0-beta.3` sorts below `1.4.0`; `semver` 1.0 orders them). A client on `beta` takes the newest of the beta and stable manifests, by SemVer (so a stable `1.4.0` replaces `1.4.0-beta.3`, and a beta never skips back to an older stable). **Leaving beta does not downgrade:** setting `stable` makes the next check read the stable manifest only; if stable is lower than the installed beta, status is `Idle` and the beta keeps running until a stable version above it ships. The person can use Roll back if the previous version is the one they want. Beta release notes are the same embedded D10 notes (a beta version has its own entry; a stable build that follows shows the notes of `(last_seen, running]`, which already handles pre-releases because both sides are SemVer). The two channels have independent `sequence` counters and independent `state.toml` entries. Switching channel changes nothing until the next check.

**Models between releases.** New model manifests reach the app without an app update through a signed `catalogue.json` (catalogue key, section 4): `{ "format": 1, "epoch": 1, "sequence": 57, "issued": …, "expires": …, "entries": [ { model_id, slot, version, min_app_version, manifest_url, manifest_sha256, size } ] }`. **It follows the manifest's replay rules exactly:**
- `epoch` is the root's, from `keys.json`, and `sequence` rises per catalogue; the client persists the highest accepted `(epoch, sequence)` as the catalogue's own watermark in `state.toml`, separate from the channels'. A lower pair is refused (`Replay`), so an old signed catalogue, which could re-offer a withdrawn model version, stops being acceptable once a newer one was seen.
- `expires` is 14 days and the catalogue is re-signed daily, like a manifest. An expired catalogue installs nothing: the Models page says "model list out of date" (not "no new models") and the status area shows `CheckFailed { reason: Expired }`.
- Rotation and compromise recovery are the manifest's: a root-signed `keys.json` with a higher `epoch` drops the catalogue key and resets the watermark. A test mirrors the manifest's recovery test, with a catalogue.
- `sync_catalogue` takes `epoch` and `sequence` so the registry can record each import's **provenance** (the epoch that last confirmed it); the replay decision is `omuzaki-update`'s, made before the call.
- Fetched only by `update.check` or the Models page's own refresh, never in the background, with the same no-identifier rules and the updater's `HttpFetcher`.
- **The import path (a gap the first draft left open).** ML-01's registry knows only manifests it was handed: `register_model!` ones are `Shipped`, anything a person adds is `User`, and origin is never written in a manifest but decided by the registry from where the manifest came (ADR 0017, `ai-api.md` §5). A catalogue model is neither, so the design adds a third origin and one entry point. `Origin`, `ModelManifest::validate` and the `ModelRegistry` trait live in **`omuzaki-ai-api`** (the registry is an ops service, `dyn ModelRegistry`), so the change is an API change there, with the implementation in `omuzaki-ai` and the stand-in in `omuzaki-ai-mock`:
  - `Origin::Catalogue` (authenticated by us, delivered after the build) and `ModelRegistry::sync_catalogue(epoch: u32, sequence: u64, entries: &[CatalogueEntry]) -> Result<SyncReport, ImportError>` (a method of the trait, so `omuzaki-update` reaches it through `Arc<dyn ModelRegistry>`), where `CatalogueEntry { manifest: ModelManifest, manifest_sha256: Sha256 }`. The hash is the one `omuzaki-update` verified against the signed catalogue (the hash of the manifest file's bytes); the registry persists it with the import and compares it, because the existing model digest covers the model's files and cannot stand in for it. One call per verified catalogue, atomic: the registry first computes a plan (imports, no-ops, revocations, supersessions), and commits all of it or, on an error, none. Origin is assigned by this entry point alone; a manifest cannot claim it. Only `omuzaki-update` calls it, after verifying the catalogue signature, the entry's `manifest_sha256` and `min_app_version`; the registry trusts its in-process caller, as it trusts the app layer's `Fetcher`.
  - `validate(Origin::Catalogue)` applies the **shipped** rules (https URLs with pinned hashes, a licence record, a CPU path, an id that is not `user-*`), because we distribute these files and the licence policy covers them. The catalogue build in `cargo xtask release` runs the same shipped-model licence gate on every entry before it signs, and a register row exists for each entry.
  - The imported manifest is kept in the registry's own state file with the other installed manifests, so it survives restarts and rollback to a version the current app does not register. `install` then works on it exactly as on a shipped model: `FileSource` downloads, hash check, smoke test, only then default; the replaced version stays as the rollback target.
  - **An existing id may get a new version** (this is the between-release upgrade path). A catalogue entry may carry the id of a shipped or earlier catalogue model if its slot and interface version are the same as the existing id's and its `version` is strictly greater than every **non-revoked** version of that id the registry knows. It is a new installable version of that id, never a replacement: the version it supersedes stays installed as the rollback target (ADR 0017 §5). Only `user-*` ids are barred, and a `User` model can never be superseded.
  - **Refreshes are idempotent.** The daily re-sign re-offers entries already imported. An entry identical to an existing import (same id, version and `manifest_sha256`) is a no-op that returns the existing `ModelRef` and updates its provenance epoch to the current one; the same id and version with a different `manifest_sha256` **within the same epoch** is `ImportError::Conflict` and changes nothing (a version is never replaced inside one epoch). So a refresh, or a restart followed by a refresh, changes nothing but provenance.
  - **Collision recovery after a root epoch bump.** A stolen key could pre-import `silero-vad@1.1.0` with hash A; the legitimate catalogue then carries `1.1.0` with hash B. Within the poisoned epoch that is a `Conflict`, and the way out is the root raising the epoch. In a catalogue of a **higher epoch than the stored import's**, a different hash for an existing id and version is not a conflict: the older-epoch import is **superseded**. It is marked `Revoked` (quarantined, never selectable) and the new entry is imported in the same atomic plan, so the clean sync is never aborted and its other revocations still commit. A conflict between two entries of the same epoch, including inside one catalogue, still aborts the sync.
  - **A stolen catalogue key cannot poison the version order for good.** It could import `silero-vad` `4294967295.0.0` and, by the strictly-greater rule, block every legitimate version. The recovery is tied to the root's epoch: when `sync_catalogue` sees an `epoch` higher than the one stored, every import whose provenance epoch is older and which the new catalogue does **not** re-list is marked `Revoked`. The legitimate entries are re-listed in the first catalogue of the new epoch and are confirmed in place (no download, the files are by hash); the poisoned ones are not, so they are revoked. A `Revoked` version cannot be selected or installed, is ignored by the version rule, and if it was the selected version the slot falls back along the existing chain (previous non-revoked version, then the shipped default). Its files stay in `FileSource` until the usual cleanup.
  - **Test (UP-H1):** with the fake server, install a catalogue version whose id and version are in no `register_model!` call; assert `Origin::Catalogue`, a successful install, selection, rollback to the shipped version; and **upgrade a shipped id** (`silero-vad`) to a catalogue version no `register_model!` knows, with the shipped version kept as the rollback target; a restart that still lists it, repeated refresh and restart changing nothing but provenance; a conflicting manifest for an existing version refused; **poisoning recovery** (import `silero-vad` `4294967295.0.0`, bump the epoch, sync a catalogue that lists only legitimate versions, assert the poisoned one is `Revoked`, the selection fell back, a legitimate `1.1.0` installs, and the `Revoked` state survives a restart); **collision recovery** (poison `silero-vad@1.1.0` with hash A, bump the epoch, sync a catalogue with `1.1.0` and hash B plus other legitimate entries; assert A is `Revoked`, B is imported, the other entries committed, and all of it survives a restart; a same-epoch hash change is still refused); and refusals for a bad signature, a `user-*` id, a `min_app_version` above the app's, and a replayed catalogue.
- **`min_app_version` gating:** an entry the running app is too old for is shown "needs Omuzaki 1.5" and cannot be installed. An entry whose slot interface version the app does not register is likewise hidden, not an error.
- **Opt-in:** a new model version appears as "Available" on the Models page; the person installs it (an `update.models` setting could automate that later; not built).
- **User-supplied models (ML-13)** are not in the catalogue and never auto-updated, replaced or removed: catalogue code touches only `Origin::Catalogue` entries and `Shipped` ones it supersedes, and ignores every `User` model.

## 9. Dependencies and licences

All verified as of 2026-10-13 on crates.io. Rows go in `docs/licensing/register.md` by the task that adds each crate (section 14: UP-C records `zstd`, `zstd-sys` and the bundled library together).

| Crate | Version | Licence | Used for | State |
|---|---|---|---|---|
| `ed25519-dalek` | 2.2.0 (in `Cargo.lock`; 3.0.0 exists, 2026-07-06) | BSD-3-Clause | sign and verify | already in the tree; move to 3.x only with the tree |
| `sha2` | 0.11 | MIT OR Apache-2.0 | package and tree hashes | in the tree via other crates |
| `semver` | 1.0 | MIT OR Apache-2.0 | version order | planned by ADR 0026 |
| `zstd` | 0.14.2 (2026-10-10) | BSD-3-Clause (crate `Cargo.toml`) | package compression | new |
| `zstd-sys` | 2.1.1+zstd.1.5.7 | BSD-3-Clause (crate `Cargo.toml`); the bundled C library is dual BSD-3-Clause or GPL-2.0 (`COPYING`), used under BSD | the C library behind `zstd` | new, via `zstd` |
| `qbsdiff` | 1.4.4 (2025-08-24) | MIT | create and apply binary patches (bsdiff format, pure Rust, parallel) | new; the algorithm's origin (bsdiff) is BSD-2 |
| `serde_json` | 1.0 | MIT OR Apache-2.0 | manifest | in the tree |

Nothing needs network access except `Fetcher`. No new external service is introduced by the design; hosting the files is a maintainer choice (section 13).

## 10. Release pipeline

`cargo xtask release` (a future task; not in this fence) produces, per channel and target:
1. `full.tzu` (zstd-compressed tree) and a delta from each of the last **3** stable releases (and the last beta for the beta channel); a delta over 60 percent of the full package is dropped.
2. The manifest with hashes, `tree_sha256`, `sequence` incremented from the host's current one; signed with the release key from CI secrets (never printed, never on a developer machine).
3. A daily re-sign job (new `issued`, `expires`, `sequence`, same content).

**Gates, failing the release build:**
- **Installer size (QG-06):** each OS's installer ≤ 150,000,000 bytes, models excluded; the full update package is checked against the same figure (it holds the same files), and each delta must be smaller than its full package. A delta never counts toward the installer budget: it is not in the installer.
- **Release notes (D10):** `cargo xtask release-notes check` passes for the version being released; its English entry exists.
- **Update dry run:** the previous release's app, in a temp directory, takes the new release through `check`, `download`, `stage` and start against a local file server; `tree_sha256` equals the build's.
- **Key and expiry check:** the manifest verifies with the embedded keys and `expires` is at least 7 days ahead.

## 11. Tests (no real server, no real install)

- **Fake update server** (`omuzaki-update-mock::FakeServer`) implementing `Fetcher`: serves manifests, packages and catalogues from memory; can truncate, flip a byte, serve a stale or replayed manifest, expire, go offline, ignore `Range`, and records every `open(url, from)` call, so a test asserts the exact URLs, ranges and count. **A fake at the `Fetcher` trait cannot see HTTP headers**, so the header half of the **no-identifier test** is a separate loopback test using the real `HttpFetcher` (built as the app builds the updater's, `.user_agent("omuzaki-update/1")`) against a local test server on `127.0.0.1` that records the raw request; it asserts no query string, no cookie, no body, no `Authorization`, and `User-Agent` exactly `omuzaki-update/1`. It lives with UP-B and runs once `omuzaki-http` has landed.
- **Trust:** wrong key, wrong role (a catalogue signature on a manifest), unknown key id, `sequence` lower or equal, expired, truncated signature, rotated key set, a `keys.json` signed by a non-root key: each a distinct `UpdateError`. **Recovery after compromise:** accept a manifest with `sequence = u64::MAX`; publish a root-signed `keys.json` with a higher `epoch` and a new release key; assert the client accepts the new key's `(epoch+1, 1)` manifest and refuses the poisoned one.
- **Delta apply, property tests (`proptest`):** for random old and new trees (including empty files, renames as delete plus add, files that grow, shrink, or are unchanged), `apply(diff(old, new), old) == new` byte for byte and `tree_sha256` matches; a patch applied to the wrong base fails with `HashMismatch`, never produces output; a corrupted patch (any byte flipped) fails or is caught by the hash, never panics; memory stays bounded (a test with a 64 MiB file).
- **Kill-during-apply:** a `Disk` test double that fails or "kills" after the *n*th write, rename or `fsync` of staging and of the launcher's switch; for every *n*, restart, then assert one of exactly two worlds: the old version current, with the new version either absent or staged and valid; or the new version current. Never a half-written tree as current. Runs the real launcher logic as a library function over a temp directory.
- **Rollback:** manual (pointer move, `bad[]`, not re-offered); automatic (two failed trials roll back, once; no loop; missing `previous` gives the plain message); `healthy()` clears the trial; settings files unchanged by a rollback; a newer-schema project is refused.
- **Live holds:** a held `Updater` stages but reports `Staged { deferred_by }`, refuses restart with `Busy`, and releases on drop; a leaked hold does not survive a new `Updater`.
- **Channel:** beta to stable does not downgrade; stable replaces a lower beta; independent sequences.
- **Settings and ops:** `update.channel` is `Never` for agents (an agent `settings.set` returns `person_only`); `update.install` and `update.rollback` are person-only for every principal; the check-on-start key off performs no `Fetcher` call at all.

## 12. Ops and settings

| Op | Kind | Scope | Agent | Notes |
|---|---|---|---|---|
| `update.status` | query | `read` | yes | channel, state, last checked, holds |
| `update.check { channel? }` | query | `net` | confirm | the one GET of section 4; `net` is not in the agent defaults |
| `update.download { version }` | edit | `net` | confirm | background; progress events |
| `update.install { version }` | edit | `edit:settings` | **person only** | stages for the next start; never restarts |
| `update.rollback` | edit | `edit:settings` | **person only** | `pending = previous` |
| `update.cancel` | edit | `net` | confirm | cancels only a download that `net` started; a write under `read` would let any read-only grant cancel |

These reuse existing scopes (`Net`, `Read`, `EditSettings`), so **no `omuzaki-ops-api` change is needed**. The person-only rule is `OpError::person_only` as in ADR 0026; an agent in a session cannot approve it. Install and rollback replace code the user runs, so they are the one write no approval flow may delegate.

Keys (registered through `register_setting!`, `update.*` section):
| Key | Scope | Agent | Applies | Default |
|---|---|---|---|---|
| `update.channel` | Machine | Never | next start | `stable` (ADR 0026) |
| `update.check_on_start` | Machine | Never | next start | **`false`** until UP2 decides |

**`update.check_on_start`, both values designed.**
- **`false`:** the updater makes no request of any kind unless the person chooses Help > Check for updates (or a CLI/MCP `update.check` they confirmed). Status is `Idle { last_checked }`. No timer, no schedule.
- **`true`:** once per start, at least 24 hours after the last check (`last_checked` in `state.toml`), on the update worker, 15 seconds after `startup_plan` resolves and never inside the startup budget (`startup.live`), and not when a hold exists. It fetches the manifest only. A newer version appears as `Available` (a badge); **nothing downloads without the person** (auto-download is an UP2 question). A failed check is silent and tried at the next start.
- A line in Settings > Updates states what each check sends (section 4), so the choice is informed.

## 13. Open questions for the maintainers (UP2)

Each has a recommendation; none blocks the first four tasks. Questions 2 and 9 are answered, and the former question 10 is settled: `Fetcher` stays in `omuzaki-ai-api` and `omuzaki-update` takes an `Arc<dyn Fetcher>`.

| # | Question | Options | Recommended |
|---|---|---|---|
| 1 | Default of `update.check_on_start` | off; on (daily at most); ask once at first run | **Ask once at first run, the preselected answer off.** It keeps "no telemetry by default" and makes updates discoverable. It adds a step to the first-run state machine. |
| 2 | Download automatically after a positive check | never; only on `true`; ask | **Decided: never in v1.** The person clicks Download; staging and applying are already deferred |
| 3 | Where manifests and packages are hosted | GitHub Releases plus a static site; a bucket and CDN; both | **A static bucket or site behind a custom domain** (so the host can change without a client release). A new external service, so the maintainers decide; cost is storage plus egress for deltas |
| 4 | Windows code signing | Azure Trusted Signing; OV or EV certificate; none at first | **OV or Trusted Signing**: the project buys; unsigned installers trigger SmartScreen |
| 5 | macOS signing and notarisation | Apple Developer ID plus notarisation; none | **Developer ID** (yearly fee, a project account); macOS cannot reasonably run unsigned |
| 6 | Installer technology | cargo-packager (NSIS, DMG, AppImage); native tools; MSIX for Windows | **cargo-packager**: one tool, permissive licence. Per-user install on Windows (no admin) so the updater can write |
| 7 | Linux | AppImage self-updating; deb and rpm only; both | **AppImage self-updates; distro packages are `Managed`** and defer to the package manager |
| 8 | Custody of the root and release keys | hardware key held by a maintainer; HSM service; CI secret | **Root on an offline hardware key held by a maintainer; release key in CI secrets** with a revoke plan (section 4) |
| 9 | Versions kept | current and previous; plus one older | **Decided: current and previous.** Disk cost is the deciding fact, to be measured on the first real build |

## 14. The task cut

Every task is blocked by this design first.

| Task | Does (fence) | Blocked by |
|---|---|---|
| UP-0 | Spike with a written verdict: Velopack with a custom `UpdateSource` against the five questions of section 1; runs before UP-C and decides whether UP-C, UP-D and part of UP-E are built (the Windows part needs a Windows machine) | UP-A |
| UP-A | `omuzaki-update-api` and `omuzaki-update-mock` (`MockUpdater`, `FakeServer` skeleton, `FakeDisk`): the types of section 3, errors, `Hold`, events; crate-check test | this design |
| UP-B | `omuzaki-update` trust: manifest and `keys.json` types, signature framing, `sequence`, expiry, key rotation, the check path over `Fetcher`, the no-identifier tests (fake for URLs, loopback `HttpFetcher` for headers) | UP-A (loopback test) |
| UP-C | `omuzaki-update-delta`: package format (`.tzu`, `.dzu`), `diff` (used by xtask) and `apply`, zstd and qbsdiff, property tests; licence rows (`zstd`, `zstd-sys`, `qbsdiff`) | UP-A, UP-0 verdict |
| UP-D | `omuzaki-update` staging: layout, `state.toml`, unpack and verify, `stage`, retention, kill-during-apply tests | UP-B, UP-C |
| UP-E | `omuzaki-launcher`: pointer switch, trial and `attempts`, `healthy()` channel, auto-rollback, manual rollback, plain failure message; per-OS process start | UP-D |
| UP-F | Ops `update.*`, the two settings keys, the `Hold` wiring helper (`omuzaki-ops` or `omuzaki-update-ops`), l10n text | UP-A (settings api) |
| UP-G | `omuzaki-ui`: Help > Check for updates, badge, Settings > Updates (channel, check-on-start, "what is sent"), Roll back, `RolledBack` notice; reuses the Release notes view | UP-F, UI shell |
| UP-H1 | `omuzaki-ai-api` (API change: `Origin::Catalogue`, `CatalogueEntry`, `ModelRegistry::sync_catalogue`, `Revoked` state, `validate(Catalogue)`), `omuzaki-ai-mock` and `omuzaki-ai` (implementation, state-file persistence); tests: install an unregistered version, upgrade a shipped id, rollback, restart | the ML-01 registry |
| UP-H2 | `omuzaki-update`: `catalogue.json` fetch and verification (epoch, sequence watermark, expiry), catalogue key, `min_app_version` gating, the call into `sync_catalogue`, Models page "Available" rows | UP-B, UP-H1 |
| UP-I | `cargo xtask release`: build full and delta packages, sign, manifest, daily re-sign, the four gates of section 10 (installer size, notes) | UP-B, UP-C |
| UP-J | The live-hold callers: streaming, recording, render jobs, capture take a `Hold` (one task per subsystem) | UP-A, the subsystem's crate |
| UP2 | Maintainer decisions of section 13, then hosting, signing certificates and installer tool | UP-I for the first real release |

Sequence: A, then B and UP-0 in parallel, then C (if UP-0 says build), D, E; F and J alongside D; G, H1 then H2, I last. A, B and C are testable without a real install; E's per-OS process start and I's signing need real machines or CI later.

## 15. Where this page may need splitting

At about 280 lines it is within the size target. If review asks for it, the natural split is sections 4 to 6 (trust, layout, rollback) into `updates-trust.md` and leave the mechanism, contract and cut here.
