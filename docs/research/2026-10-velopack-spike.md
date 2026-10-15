---
title: "Velopack spike (UP-0), Linux half: verdict"
summary: "Velopack spike (UP-0), Linux half: verdict"
area: research
---

# Velopack spike (UP-0), Linux half: verdict

**Design:** `docs/design/updates.md` section 1, ADR 0035. **Date:** 2026-10-13. **Code:** `spikes/update-velopack/` (standalone, not in the workspace, never shipped).

**Read and run:** Velopack source at commit `92d6a1c` (2026-09-29, MIT `LICENSE`), the `velopack` crate **1.2.161** from crates.io, and the real `Update` binary built from that commit (`cargo build --release -p velopack_bins --bin update`, 4m45s). Linux (cloud VM, x86_64) only. **macOS: `unverified`** (no macOS machine; `apply_osx_impl.rs` was not read). **Windows: filled by a maintainer's half** (commands in the spike README); where the source already says something, it is marked "read, not run".

## Verdict table

| # | Question | Linux | macOS | Windows |
|---|---|---|---|---|
| 1 | Apply deferred to next start by our launcher? | **yes**, with a layout caveat | unverified | read, not run: yes (separate `Update.exe apply`); a maintainer to run |
| 2 | Keeps the previous version; our pointer and boot guard on its layout? | **no** | unverified | read, not run: no (old `current` is renamed to a temp dir and deleted); a maintainer to run |
| 3 | Linux installer shapes | **AppImage only** (inside a `-full.nupkg`) | n/a | n/a |
| 4 | `vpk` in CI, licence | **.NET tool + `mksquashfs`; MIT** (not run: no .NET here) | unverified | a maintainer to run |
| 5 | Custom `UpdateSource` passes the no-identifier test | **yes**, with two caveats | unverified | the Linux run covers the source; a maintainer repeats |

## 1. Deferred apply: yes

`UpdateManager::download_updates` only writes `<packages>/<name>.nupkg` (via a `.partial` file, `manager.rs:405-480`). Applying is a separate step: `unsafe_apply_updates` spawns the native `Update` executable with `apply --package <file> ...` (`manager.rs:615-683`). `UpdateManager` never applies by itself. **`VelopackApp::run()` does, by default:** it applies the newest local package on startup and exits (`app.rs:51-54`, `:164-176`, `auto_apply: true`). A launcher must call `set_auto_apply_on_startup(false)` and apply on its own schedule.

Evidence (`cargo test q1_`): after download the `Update` stub had not run and the installed file was byte-identical; `get_update_pending_restart()` returned the package; a fifth test shows `VelopackApp::run()` with auto-apply off leaves the pending package and the installed file alone (`q1_startup_with_auto_apply_disabled_...`); calling apply produced `apply --package .../omuzakitest-1.1.0-full.nupkg --silent --norestart --root .../omuzakitest.AppImage --packageDir .../packages`.

**Caveat:** on Linux the locator finds `Update` inside the mounted AppImage (`<mount>/usr/bin/UpdateNix`, `locator.rs:463-535`). A launcher outside the AppImage would have to extract that binary itself or run the old AppImage with an apply flag. This couples our launcher to Velopack's layout.

## 2. Previous version: no

Real run, `Update apply` from the built binary on a fake install (AppImage `v1.0.0`, package `1.1.0`): exit 0, log `AppImage moved successfully`, and the AppImage then reads `v1.1.0`. After it, the tree held only the new AppImage and `packages/omuzakitest-1.1.0-full.nupkg`; **no copy of 1.0.0 remained** (`apply_linux_impl.rs`: extract to temp, `mv -f` over the AppImage). `download_updates` also deletes older `*.nupkg` files (`manager.rs:424-440`), so no old package is left either. There is no rollback API in the crate. Our `state.toml` pointer with `versions/<v>/` folders (design section 5) does not sit on a layout that is one replaced file; we would copy the AppImage aside before every apply, which is our own layout in all but name. Windows (read): `apply_windows_impl.rs:160-229` renames `current` to a temp dir, swaps in the new one, then removes the temp dir; same conclusion pending the maintainers' run.

## 3. Linux installer shapes: AppImage only

`LinuxPackCommandRunner` produces one `<id>.AppImage` (squashfs + the type2 runtime) wrapped in the `-full.nupkg`; the options file has no deb, rpm, flatpak or snap term. A `.desktop` entry is written inside the AppDir. Self-update needs `$APPIMAGE` to point at a writable file; a distro-package install gets no updater, which matches design section 5 (`Managed`). `UpdateNix` may fall back to `pkexec` when the move fails (elevation dialog, `apply_linux_impl.rs`).

## 4. `vpk` in CI and licence

- A .NET global tool (`PackAsTool`, targets net8.0, net9.0, net10.0, `Velopack.Vpk.csproj`), so CI needs the .NET SDK and `dotnet tool install -g vpk`. Linux packing also needs `mksquashfs` on PATH (`AppImageTool.cs`: asserts it, hint `apt install squashfs-tools`). Not run here (no .NET); the NuGet dependencies' licences (Spectre.Console, Serilog, System.CommandLine and others) were **not read**: `unverified`.
- Licence: Velopack is **MIT** (`LICENSE`, "Copyright 2021 Caelan Sayler, 2024 Velopack Ltd"). `vendor/THIRD_PARTY_NOTICES.md` lists what `vpk` bundles: zstd 1.5.5 (dual GPL-2.0 / BSD-3, the BSD-3 option is usable), AppImage type2-runtime (MIT), backhand-based `mksquashfs.exe` (MIT or Apache-2.0), WiX fork (MS-RL, MSI only), `signtool.exe` (Microsoft SDK terms, Windows signing only). `mksquashfs` on Linux is the system's squashfs-tools (GPL-2.0), run as a separate program and never linked or shipped by us. None is non-commercial.
- Crate side (the part that would enter the workspace): `cargo metadata` on the spike lists 131 packages, all MIT, Apache-2.0, BSD, ISC, Zlib or Unicode variants; the exceptions are `webpki-roots` (CDLA-Permissive-2.0, a permissive data licence, needs a policy check) and the spike itself. If Velopack is adopted the lead files the register rows.

## 5. Custom `UpdateSource` over a fake server: yes

`SignedSource` (`src/lib.rs`) is driven by the real `UpdateManager` over an in-memory feed double. Evidence (`cargo test -- --nocapture --test-threads=1`, 5 passed):

- Recorded requests, in full: `GET /v1/linux/feed.json`, `GET /v1/linux/feed.json.sig`, `GET /v1/linux/omuzakitest-1.1.0-full.nupkg`. No query string, no `localVersion`, `id` or `stagingId`, no version; the test also asserts the locally stored staging id never appears in any request.
- A byte flipped in the asset after signing: `download_updates` fails with `Checksum did not match ... expected 8ff9bd79..., actual 33f96c01...`; no `.nupkg` is left in the packages folder and `get_update_pending_restart()` is `None`. The source compares against the hash from the *verified* feed, not the `SHA256` on the asset it is handed.
- A feed byte flipped, or a feed signed by another key: `feed signature does not verify`; nothing is parsed.

**Caveats.** (a) Implementing `UpdateSource` needs the crate feature **`public-utils`**: without it `bundle::Manifest`, which is in the trait's signature, is private and the code does not compile (found by the first build). (b) The locator still creates `packages/.betaId` on disk and passes it to every source; it is never sent by us, but the file exists unless we also patch or avoid `get_staged_user_id`.

## Provisional recommendation

Design rule (section 1): "five yes answers: adopt; any no: build ours". Linux has one clear **no** (question 2), a launcher coupling in question 1, and an extra .NET toolchain in question 4. Question 5 shows Velopack *can* be made private and signed, which was never the objection. **Provisional: keep our own updater** (design section 14 stands). The Windows half can overturn this only by showing Windows keeps the previous version in a layout our pointer can use, which the source suggests it does not.
