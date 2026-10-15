---
title: "0035: Updates are our own signed manifest, per-file deltas and a staged swap into versioned folders (2026-10-13)"
summary: "Updates are our own signed manifest, per-file deltas and a staged swap into versioned folders"
area: decisions
---

# 0035: Updates are our own signed manifest, per-file deltas and a staged swap into versioned folders (2026-10-13)

**Status:** proposed. **Context:** SE-08, SE-04, QG-06; spec v3 §10, §31, §34; ADR 0017 (`Fetcher`, `FileSource`, model rollback), ADR 0021 and `docs/design/journal.md` (pre-migration snapshots), ADR 0026 (`update.channel`, D5, D9, D10). Design: `docs/design/updates.md`.

## Context

The spec wants delta updates, models updated separately and verified by hash, rollback, and a beta channel people can join and leave, with no telemetry by default. Settings fixed the surroundings and left the updater out. The Rust stack research never checked an updater, and the desktop app is egui, so the Tauri updater does not come with it.

Read on 2026-10-13: Velopack 1.2.161 has deltas and channels. Its stock `HttpSource` sends a persistent per-install `stagingId` and trusts a SHA-256 from the same unsigned feed as the file, but its public `UpdateSource` trait lets a custom source over our `Fetcher` omit the identifier and verify our signature (the first draft wrongly said this needed a fork). It has no rollback or crash guard, applies through a separate `Update` executable that owns the install layout, and packages with its `vpk` tool (unverified). `cargo-packager-updater` 0.2.3 checks minisign signatures but downloads whole installers and was last released 2025-07-21. Sparkle, WinSparkle and MSIX each cover one platform; their documentation was unreachable from the build box and is marked `unverified` in the design.

## Decision

1. **Build a thin updater from audited parts** (`ed25519-dalek`, `qbsdiff`, `zstd`, `sha2`, `semver`) in new crates `omuzaki-update-api` (+ mock), `omuzaki-update`, `omuzaki-update-delta` and a tiny `omuzaki-launcher` binary. Velopack with a custom signed `UpdateSource` is the runner-up, tested by a spike (UP-0) before the delta work starts; five yes answers adopt it and shrink the delta, staging and launcher tasks (design §1).
2. **We verify.** Manifests, key sets and the model catalogue are ed25519-signed by separate keys (root offline, release, catalogue); the app embeds the root keys. Manifests carry a root-controlled `epoch`, a monotonic `sequence` and an expiry; a lower `(epoch, sequence)` is refused (downgrade protection), and the root can raise the epoch to recover from a stolen release key. An expired manifest is a visible `CheckFailed`, never "up to date".
3. **A check is one GET of a static file** whose path holds channel, OS and architecture. No query, no identifier, no version: the client compares versions itself.
4. **Versioned folders and a pointer.** Packages unpack beside the running version and are made current by the launcher at the next start. Never hot-swapped; install and rollback are person-only; nothing applies while a stream, recording, render or capture holds the updater.
5. **Rollback** keeps the current and the previous version. A new version that does not reach a ready UI in two starts is rolled back once by the launcher (the shape of the D9 opening guard, one level lower). Settings are never migrated down (D5), a newer project schema is refused, models roll back independently (ML-01).
6. **Models** arrive through a signed catalogue, `min_app_version`-gated, hash-verified through `FileSource`, opt-in, imported through a new `Origin::Catalogue` entry point on the `ModelRegistry` trait (a `omuzaki-ai-api` change: `sync_catalogue`, idempotent, with provenance epochs so a root epoch bump revokes imports a stolen catalogue key made), with epoch, sequence and expiry like manifests. User-supplied models are never touched.
7. **Beta** is a SemVer pre-release channel; leaving it never downgrades.
8. **`update.check_on_start`** is a `Machine` setting, agent `Never`, **default off** until the maintainers decide (UP2); both values are designed. Ops reuse the existing scopes (`net`, `read`, `edit:settings`); no `omuzaki-ops-api` change.

## Consequences

- New crates and a task cut (`docs/design/updates.md` §14). New dependencies: `zstd` 0.14 (BSD-3-Clause) and `qbsdiff` 1.4 (MIT); `ed25519-dalek` is already in the tree. Register rows go in the tasks that add them.
- The maintainers decide in UP2: the default of `update.check_on_start`, hosting, Windows and macOS code signing, the installer tool, key custody, Linux packaging (`design/updates.md` §13).
- We own the updater's bugs and security; the mitigation is that it is small, pure-functional at its core, and tested with a fake server, property tests and kill-at-every-step tests.
- Code signing and notarisation stay an outer layer and never replace our check.
- **Rejected for now:** stock Velopack (identifier in the check, unsigned feed), and Velopack with our adapter pending the spike (no rollback or guard, owns the install layout); `cargo-packager-updater` (no delta, stale); MSIX, Sparkle and WinSparkle (one platform each, three mechanisms); replacing files in place (impossible for a running executable on Windows, breaks macOS signatures); a version in the check request (not needed, the client can compare); release notes in the manifest (D10: they ship in the binary).

## Amendment (2026-10-15): contract changes from the review of #1512 (issue #1589)

The review of the first `omuzaki-update-api` pull request (#1512) found three weak points in the contract; they were fixed in the follow-up for issue #1589:

- `Updater::healthy` returns `Result<(), UpdateError>`. A marker that cannot be written must not look like success, because the launcher counts a trial start whose marker is missing as failed and could roll back a healthy version.
- `Url` has a private field and is validated: it is built only through `Url::new` (or deserialization, which uses it), so a value is always an https URL with a host and a port in 0..=65535.
- The API snapshot (`tests/api_surface.txt`) also lists the methods of each `pub trait`, so a renamed or removed trait method shows up as a diff.

Nothing on `main` depended on the old shapes (`healthy` returning nothing, a public `Url` field), so no other crate changed.
