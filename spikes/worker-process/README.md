# Spike: the renderer in a killable worker process

Can the renderer live in a process that is killed with `kill -9` while a second process keeps the output fed with the last
frame? Results and what they mean for the supervisor design: `docs/research/2026-10-worker-process.md`. Task: issue #1022.

Its own workspace, like `spikes/broadcast-out`; nothing here touches the main workspace. The test picture, frame tag,
statistics and Spout bridge come from `spikes/broadcast-out/spout-out` by path dependency.

```
src/supervise.rs   spawn, seeded kill, reap, respawn; the timeline (platform-neutral)
src/analysis.rs    the numbers the page reports, from the timeline and the presenter's log (pure, unit-tested)
src/cpu_host.rs    Linux host: shared-memory last-frame slot, steady 30 fps presenter
src/cpu_worker.rs  Linux worker: wgpu (lavapipe here) -> shared memory -> (slot, index) on the pipe
src/win/           Windows DX12 lane: shared texture + shared fence + Spout. Compile-checked only, never run.
tools/check-windows.sh   compile-checks the Windows lane from Linux
results/           the JSON reports behind the page's Linux tables
```

## Run (Linux)

```sh
cargo build --release
./target/release/wp-spike host --json run.json     # 30 s undisturbed, then 200 seeded kills; about 3 minutes
./target/release/wp-spike host --baseline-s 0 --worker-env MESA_SHADER_CACHE_DISABLE=true --json run.json
```

Needs a Vulkan driver (`mesa-vulkan-drivers` for lavapipe on a VM without a GPU). The ring file lives in `/dev/shm`
and is removed on exit. Same seed, same kill schedule.

## Check (Windows lane, from Linux)

```sh
rustup target add x86_64-pc-windows-msvc
spikes/worker-process/tools/check-windows.sh
```
