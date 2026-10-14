# UP-0 spike: Velopack with a signed, identifier-free `UpdateSource`

Throwaway spike (design `docs/design/updates.md` section 1, ADR 0035). **Nothing here ships and nothing here is a workspace member**: `Cargo.toml` carries an empty `[workspace]` table, CI never builds it, and it pulls `velopack` 1.2.161 (MIT) only inside this folder. The written verdict is `docs/research/2026-10-velopack-spike.md`.

- `src/lib.rs`: `FeedDouble` (an in-memory static host that logs every request path) and `SignedSource` (a Velopack `UpdateSource`). The source drops `localVersion`, `id` and `staged_user_id`, verifies an ed25519 signature over the feed (domain-separated, verified before parsing), and checks each asset's SHA-256 against the *signed* feed before it writes any file.
- `tests/spike.rs`: five tests that drive the real `UpdateManager` over a fake install (a temp dir with a stub `Update`), covering verdict questions 1 and 5. Keys are generated from fixed test seeds; there is no server, account or real key.

## Run on Linux or macOS

```
cd spikes/update-velopack
cargo test -- --nocapture --test-threads=1
```

## Run on Windows (the UP-0b half)

Run in PowerShell from the repo root on a clean machine. The `velopack` crate builds the same on Windows; the extra steps check questions 1 to 4 against the real tools. Fill the Windows column of the verdict from the outputs.

```
# 1. The spike tests are Unix-only (the stub Update is a shell script), so this compiles the library and runs 0 tests; question 5 on Windows is covered by the Linux run
cd spikes\update-velopack
cargo test -- --nocapture --test-threads=1

# 2. Install the packer (question 4: what vpk needs). Needs the .NET 8 SDK or later.
dotnet --version
dotnet tool install -g vpk --version 1.2.161
vpk --help

# 3. Pack two versions of any small console exe (question 2: layout, previous version)
cargo new --bin ..\..\..\hello-velo
cd ..\..\..\hello-velo
cargo build --release
# Separate output folders: packing 1.1.0 into out-1.0.0 would replace the 1.0.0 Setup.exe.
vpk pack --packId OmuzakiSpike --packVersion 1.0.0 --packDir target\release --mainExe hello-velo.exe --outputDir out-1.0.0
vpk pack --packId OmuzakiSpike --packVersion 1.1.0 --packDir target\release --mainExe hello-velo.exe --outputDir out-1.1.0

# 4. Install 1.0.0 (not 1.1.0), check the installed version, then stage 1.1.0 without applying it (question 1)
.\out-1.0.0\OmuzakiSpike-win-Setup.exe
Get-Content $env:LOCALAPPDATA\OmuzakiSpike\current\sq.version | Select-String '<version>'   # must say 1.0.0
Copy-Item out-1.1.0\OmuzakiSpike-1.1.0-full.nupkg $env:LOCALAPPDATA\OmuzakiSpike\packages\
dir $env:LOCALAPPDATA\OmuzakiSpike               # note: current\, packages\, Update.exe

# 5. Apply from a separate process, then look at the layout (questions 1 and 2)
& $env:LOCALAPPDATA\OmuzakiSpike\Update.exe apply --norestart --package $env:LOCALAPPDATA\OmuzakiSpike\packages\OmuzakiSpike-1.1.0-full.nupkg
Get-Content $env:LOCALAPPDATA\OmuzakiSpike\current\sq.version | Select-String '<version>'   # must now say 1.1.0
dir $env:LOCALAPPDATA\OmuzakiSpike               # is 1.0.0 still anywhere on disk?
dir $env:LOCALAPPDATA\OmuzakiSpike\packages
```

Record for each step: the command, the exit code, and the directory listings. The licence of `vpk` and what it downloads are read from source in the verdict; on Windows add `vpk` run-time needs (internet use, helper binaries it unpacks) from the output of step 3.
