---
title: "0026: Settings are self-registered typed keys in the user's own files, read and written through ops (2026-10-12)"
summary: "Settings are self-registered typed keys in the user's own files, read and written through ops"
area: decisions
---

# 0026: Settings are self-registered typed keys in the user's own files, read and written through ops (2026-10-12)

**Status:** accepted (2026-10-12). It adds a new API crate (`omuzaki-settings-api`) and a `omuzaki-ops-api` scope. **Context:** SE-10, SE-08, CN-05, VC-08; spec v3 §4.1, §6, §7, §31, §39 rule 2; ADR 0013 (file roots), ADR 0021 (recovery), ADR 0023 (locale follows the reader). Design: `docs/design/settings.md`.

## Context

Every phase 2 feature (devices, outputs, quality tier, input capture, mascot, voice commands) needs settings, and today each would invent its own: `omuzaki-ui::ds::Settings` is a struct nobody saves, the audio design has a `ProfileStore` stand-in, ADR 0013 says file roots "persist with the project". SE-10 adds profiles, session restore and release notes on top. Ops are pure functions of the project document and return a patch, and the journal, undo and branches all follow that document; a person's taste must not enter it.

## Decision

1. **Four kinds of persisted thing:** settings (a person's choices), project state, secrets (OS keychain only), and app state (recent projects, window geometry). Only the first is a setting.
2. **A setting is a typed key that registers itself** (`register_setting!`): key, value type with a derived JSON Schema, default, scope `User | Machine`, agent rule `Free | Confirm | Never`, profile membership, live or next-start, version. All are required; none has a default. Any key touching privacy, network, file roots, credentials, updates or capture is `Confirm` or `Never`, never `Free`.
3. **`User` settings live in the config dir (they may roam); `Machine` settings and session state in the local data dir (they never do).**
4. **Read and written through ops over a `Settings` service** (`Undo::Explicit`, empty patch), not through the document. A new scope `edit:settings` is in the agent defaults; each key's own agent rule is the real gate.
5. **TOML edited with `toml_edit`:** atomic replace, merge-on-write per key, unknown keys kept, per-key versions and migrations, a damaged value falls back to its default without being deleted.
6. **Profiles are overlays of `User` `Member` keys in their own files.** Edits while a profile is active go into it; switching is one atomic snapshot swap and one event. **Profile "share" is cut:** a profile is a small file that can be copied by hand and is validated on load.
7. **Session restore remembers what was open, never what was running.** Crash recovery wins over the opt-out; a project that cannot be reopened is explained, not dropped; a guard prevents a crash loop on open.
8. **Release notes are reviewed user text per version, embedded in the app, drafted from `.changes` fragments, shown once after an update, always under Help.** English is required for a release; a missing translation falls back to English per item and does not block.
9. **`io.fs_roots` is a `Machine` setting.** This supersedes the sentence in ADR 0013 §3 that saved roots "persist with the project"; the rest of 0013 stands.

## Consequences

- New crates `omuzaki-settings-api`, `omuzaki-settings-mock`, `omuzaki-settings`, `omuzaki-release-notes`; `Scope::EditSettings` in `omuzaki-ops-api`; `write_atomic` moves to `omuzaki-core`. The task cut is `docs/design/settings.md` §6.
- New dependencies: `directories` 6.0 (MIT OR Apache-2.0; its `dirs-sys` MIT OR Apache-2.0 and `option-ext` MPL-2.0, allowed unmodified), `include_dir` 0.7 (MIT), `semver` 1.0 (MIT OR Apache-2.0); `toml_edit` is already in the tree. Register rows go in the tasks that add them.
- A settings change is not an undo step; each row has reset, and the last 200 changes are kept with their actor.
- The audio `ProfileStore` stand-in is replaced by the `audio.bindings` key when the audio engine lands.
- **Rejected:** a second document mounted in the executor (its patches would enter the project journal and branches); a settings struct per feature (no agent peer, no profiles, no migration); a central settings struct (a merge hotspot, against §39 rule 2); layered-config crates (they do not give overlays with write-to-active, per-key migration or unknown-key preservation); profiles in `settings.toml` (one bad profile threatens the base); release notes fetched from the network (offline failure, SE-04); release notes in the Fluent catalogs (they arrive after the strings freeze and have their own review).
- **Amended (2026-10-14):** the crash-report key `privacy.crash_reports` has the values `Off`, `SaveOnly` and `AskToSend`, default `SaveOnly`: the default saves locally and sends nothing. The earlier "off by default" row (`Off, Ask, Always`) is replaced; scope, agent rule (`Never`), profile (`Excluded`) and apply (`Live`) are unchanged. See `docs/design/privacy.md` §2 and `docs/design/settings.md` §6.
