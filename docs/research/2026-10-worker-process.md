---
title: "The renderer in a killable worker process (spike #1022): Linux CPU lane measured, Windows lane pending"
summary: "The renderer in a killable worker process (spike #1022): Linux CPU lane measured, Windows lane pending"
area: research
---

# The renderer in a killable worker process (spike #1022)

**Issue:** #1022 (BR-23 acceptance is `kill -9` of each worker; a thread cannot be killed). **Feeds:** the supervisor design (#1023).
**Date:** 2026-10-15. **Code:** `spikes/worker-process/` (own workspace, never shipped; reuses `spikes/broadcast-out`).

**Status.** Half done on purpose. The **Linux CPU lane is fully measured** (below). The **Windows DX12 lane is written and
compile-checked, never run**: every Windows number is *pending an owner run*. Nothing on this page was measured on a GPU,
on Windows, on macOS or on a reference machine.

**Every number below** comes from one cloud VM: *Intel Xeon @ 2.80 GHz, 4 logical cores, 16 GB, Linux; wgpu 30 on Vulkan
with Mesa llvmpipe (lavapipe) 25.2.8, LLVM 20.1.2, **no GPU***. Frames are 1920x1080 BGRA8 (8.29 MB), the output runs at 30 fps.
The raw reports are in `spikes/worker-process/results/`.

## Answers

| Question (from #1022) | Answer |
|---|---|
| Does a separate output-owner process hold the last frame for under 2 s while the renderer is killed and restarted? | **Yes on this lane, with a wide margin: longest hold 0.74 s over 600 kills (0.37 s in the main run).** The margin is mostly the lavapipe worker's tiny cold start (about 0.13 s). A real DX12 renderer's cold start is **unmeasured**, and it is the term that decides this on Windows. |
| Did the output ever go without presenting? | Held frames count as presented, so the presenter never stops. Three runs, about 6 minutes of presenting: **longest interval between presents 78.6 ms (2.4 periods); one interval above two periods (66.7 ms) in the three runs, and it ended at the moment of a kill, so it began before that kill.** No two presents were closer than 31.2 ms. See "Present gaps". |
| Kill to first new frame | p50 141 ms, p95 212 ms, max 355 ms (200 kills, warm Mesa cache). |
| Hand-off cost at 1080p | CPU lane: worker copy-in 3.0 ms, host read 3.2 ms per frame (p50). The GPU-lane figure against the 1.5 ms budget is **pending an owner run**. |
| Leaks | Host open descriptors 5 before and 5 after 200 kills; host RSS did not grow (29.0 MB peak). GPU memory and Windows handles: pending. |
| In-process device re-creation | **Not measured** (not in the Linux split). |

## Prior art, in one paragraph

Browsers isolate the GPU in its own process and let the browser process supervise it. Chromium's own design note on GPU
process fallback says the browser restarts a crashed GPU process, counts crashes (the usual limit is three in a short window,
each elapsed forgive-interval forgives one), and once the limit is reached the next start uses a more conservative mode:
Vulkan, then GL, then SwiftShader (software), then the display compositor, on the argument that software avoids third-party
drivers ([Chromium: GPU Process Fallback](https://chromium.googlesource.com/chromium/src/+/main/content/browser/gpu/fallback.md)).
OBS Studio does not isolate its renderer: libobs keeps graphics in-process and, on Windows, recovers from device removal by
rebuilding the Direct3D device and reloading assets from copies it keeps in RAM, with "a minor hiccup in playback"
([OBS commit "libobs-d3d11: Rebuild device and assets if device removed/reset"](https://git.tjdev.de/mirror/obs-studio/blame/commit/a504691af83dd8bbb755b5ae8dea4a30c6fe7361/libobs-d3d11/d3d11-rebuild.cpp)),
and a later commit lets external code (Windows Graphics Capture) join that handling. That covers a lost device, not a crashed
process: an access violation in OBS or a plugin ends OBS. The relevant lesson is Chromium's: a supervisor that counts crashes
and degrades after a limit, and a long-lived process that does not render. Only search-result summaries of these pages were
read (see open questions).

## Method

Two processes, `wp-spike host` and `wp-spike worker` (the host starts the worker from the same executable).

- **Host** owns the output. It keeps the newest complete frame in its own memory (the *last-frame slot*) and presents at a
  steady 30 fps from a plain `sleep` loop (each deadline is `max(previous deadline, now) + period`, the worker loop's rule, so
  no present follows the previous one by less than a full period; the whole periods a tick was late are counted): each tick shows the newest complete frame, or the previous one again when none arrived.
  Presenting here records a timestamp and the frame's age; the real output call (Spout, NDI) is the other spike's subject.
- **Worker** renders the broadcast-out test picture (`scene.wgsl`, via `SceneRenderer`) with wgpu on lavapipe, reads it
  back, writes it into one of 3 slots in a POSIX shared-memory file (`/dev/shm`, positioned reads and writes rather than
  `mmap`, so there is no `unsafe` and no extra crate), and posts `(slot, frame index)` as a 32-byte message on its stdout pipe.
  It caps itself at 30 fps. It posts `Ready` once the device and pipeline exist.
  **Slot ownership:** the worker may write a slot only after the host has read it and sent its number back (one byte on the
  worker's stdin), so a slow host stalls the worker instead of letting it overwrite a slot being read.
- **Integrity.** The host reads the announced slot and decodes the frame tag in the pixels (header, footer and frame number
  must agree with the message). Frames failing that are refused. **0 refused in every run**, including frames being written when
  a worker died (those were never posted).
- **Kills.** The host sends SIGKILL (`Child::kill`) at seeded random moments: a SplitMix64 sequence from seed 1022 (the
  other runs say theirs), uniform 20 to 1500 ms after the worker was spawned, then reaps, respawns at once and repeats. 200 kills per
  run. Kills before a worker's first frame are *startup kills* (15 of 200 in the main run). A final worker is left alone
  until its first frame so the 200th kill has something to measure to. A worker that dies **by itself** (not by our
  kill) fails the run at once with the time and exit status, so it can never be counted as an injected kill; none did. The run ends with no worker left (checked).
- **Main run:** 30 s of one undisturbed worker (per-frame costs), then 30 cold starts (a worker left alone until its first frame,
  then stopped), then 200 kills. **Start-up timings come only from the workers nothing interrupted** (baseline, cold-start and
  probe workers, n = 32); workers the kill schedule started are censored for start-up (15 of 200 never posted a frame) and are
  not in the cold-start tables. Two more runs: Mesa's shader cache
  disabled (`MESA_SHADER_CACHE_DISABLE=true`, "cold"), and a startup-heavy run (kills 20 to 250 ms after spawn, seed 77).
- **Definitions.** *Kill to first new frame*: the kill call to the first frame, usable by the output, from a later worker.
  *Hold*: the gap between the last frame before the kill and that first new frame, i.e. how long the output showed one picture.
  *Frame age*: at each present, now minus when the shown frame became usable.

## Linux results

### Hold and restart (200 kills per run)

| ms | warm cache (main) | Mesa cache off | startup-heavy kills |
|---|---|---|---|
| kill to first new frame, p50 / p95 / max | 141 / 212 / 355 | 260 / 474 / 736 | 161 / 438 / 720 |
| of which kills before the worker's first frame (n) | 15 | 35 | 93 |
| hold, p50 / p95 / max | 155 / 285 / **368** | 283 / 578 / **740** | 262 / 579 / **734** |
| frame age at present, p95 / max | 123 / 342 | 262 / 714 | 370 / 719 |
| host respawn latency (kill to next process creation), p50 / p95 / max | 13.1 / 20.2 / 43.4 | 11.9 / 18.1 / 35.2 | 10.2 / 17.7 / 57.7 |

Against 2.0 s, the worst hold of any run is 0.74 s. Earlier runs of the same configurations, before the fixes of rounds 1 and 2,
had a longest hold of 392 ms (main) and 1.05 s then 0.54 s (startup-heavy), so the tail of the runs that kill during start-up
varies by several hundred ms between runs.

### Worker cold start (process creation to a usable first frame), uncensored

Workers left alone until their first frame: the baseline worker, 30 cold starts and the probe worker (n = 32; n = 31 without a
baseline). Workers started by the kill schedule are not in these tables because a kill before the first frame hides exactly
the slow starts. The "very first worker" row is reported only when worker 1 was one of those uninterrupted workers.

| ms | warm cache | Mesa cache off |
|---|---|---|
| spawn to `Ready` (device and pipeline exist), p50 / p95 / max | 78 / 104 / 137 | 101 / 132 / 136 |
| spawn to first frame the host can use, p50 / p95 / max | 125 / 153 / 268 | 255 / 305 / 338 |
| the very first worker of the run | 268 | 338 |
| inside the worker: instance + adapter + device, p50 / max | 68 / 127 | 92 / 124 |
| inside the worker: pipeline build, p50 / max | 6.5 / 11.4 | 6.4 / 10.3 |

Shader caching matters even for software Vulkan: turning it off added about 130 ms to the first frame. Whether wgpu's own pipeline cache
(on or off) changes anything was **not tested**.

### Present gaps (held frames count as presented)

| main run, kill phase (4,459 presents) | |
|---|---|
| presents that re-showed the previous frame | 768 (17 percent) |
| interval between presents, min / p50 / p95 / p99 / max | 31.2 / 33.5 / 34.2 / 37.2 / **78.6** ms |
| intervals above 66.7 ms (two periods): "real gaps" | 1 (78.6 ms, ending at the moment of a kill) |
| whole periods a tick was late | 1 |
| same, 30 s undisturbed baseline (892 presents) | min 33.4, max 41.8 ms, 0 gaps |

Cache-off run: min 31.9, max 53.9 ms, 0 gaps. Startup-heavy run: min 33.4, max 40.8 ms, 0 gaps. In the three runs the output went
without a present for more than two periods once, for 78.6 ms; that interval ended at a kill, so it started before it. The 4-core
VM runs lavapipe on all cores next to the host, so scheduler contention is the likely cause, but **that was not tested**. The
median interval is 33.5 ms, not 33.3: the deadline rule adds the wake-up overshoot to each period, so the output ran at about
29.7 fps (mean interval 33.65 ms). Before round 2 the presenter caught up after a late tick, and the reports then showed
intervals as short as 1.6 to 2.2 ms; none remain.

### What the host costs

| 1080p BGRA, 30 fps | per frame, p50 / p95 / p99 / max | |
|---|---|---|
| worker draw + readback (lavapipe) | 18.4 / 23.2 / 30.6 / 110.4 ms | not the host's cost |
| worker copy into the slot | 3.0 / 3.8 / 5.3 / 6.8 ms | pwrite into tmpfs |
| host read of the slot + tag check | 3.2 / 4.2 / 6.4 / 17.3 ms | the host's hand-off cost |

| host process | undisturbed 30 s | kill phase, 150 s |
|---|---|---|
| CPU, share of one core | 9.9 % | 8.5 % |
| resident memory (peak) | 28.7 MB | 29.0 MB (21 MB at the end) |
| open descriptors, start / end | 5 / 5 | 5 / 5 |

The host's CPU is almost all the 8.29 MB read per frame (3.2 ms times 30). That is the cost of a CPU-lane hand-off, not of the GPU
lane; it is far above the 1.5 ms GPU budget and is not comparable to it. The host does no real output call here.

## The Windows lane (pending an owner run)

Written, not run: `spikes/worker-process/src/win/`. Per frame the worker draws the scene, GPU-copies it into one of 3 shared
D3D12 textures (committed resources, `ALLOW_SIMULTANEOUS_ACCESS`, NT shared handle published under a name derived from the
host's process id), signals a shared D3D12 fence with `index + 1`, and posts `(slot, index)`; it rewrites a slot only after the host has finished
the GPU copy out of it and sent the slot number back. The host opens the objects by
name on its own device, makes its queue wait for the fence on the GPU, copies the slot into a texture it owns (the *hold
texture*), and every tick sends that texture to Spout through `spikes/broadcast-out`'s unmodified `SpoutBridge` (wgpu D3D12
to D3D11On12 to Spout). A fence is used instead of a keyed mutex because a keyed mutex needs a D3D11-created texture. The
hold texture is what makes the output outlive the worker: killing the worker releases only the worker's references.

**Compile check, run on this VM:** `rustup target add x86_64-pc-windows-msvc` then
`spikes/worker-process/tools/check-windows.sh` (it runs `cargo check --target x86_64-pc-windows-msvc --locked`; `TARGET=x86_64-pc-windows-gnu`
checks the other target; `CARGO_CMD=clippy ... --all-targets -- -D warnings` also passes). `spout-out`'s build script compiles its C++ shim
even for `cargo check`, so the script gives it empty stand-in sources and a stand-in compiler; it proves the Rust compiles against the
Windows APIs, not that the shim compiles and not that anything runs. **Build note for #1925:** the shim's build script passes GCC
flags (`-mssse3`, `-include`), so the real binary is a `x86_64-pc-windows-gnu` build as in `spikes/broadcast-out`; the
workflow also needs `SPOUT2_DIR` (from `spikes/broadcast-out/tools/fetch-spout2.sh`). `spikes/worker-process/Cargo.lock` is
committed for `cargo build --release --locked`.

**The command an owner runs on Windows** (a machine with a DX12 GPU; OBS with a Spout2 source named
`omuzaki-worker-process-spike` is optional, to watch the output):

```
wp-spike.exe adapter
wp-spike.exe host --kills 200 --seed 1022 --baseline-s 30 --json worker-process-dx12.json
```

`adapter` prints one line: GPU name, device type, driver. `host` prints nothing while it runs (a few minutes), writes the JSON
report, and exits 0. The report has the same `analysis` block as the Linux files in `results/` (kill to first new frame, hold,
present intervals, cold start), plus `send_errors` (Spout sends that failed), `skipped_ticks`, and the run's `size`, `fps` and `backend`. It does **not** yet contain: handle counts before
and after, GPU memory (`QueryVideoMemoryInfo`), GPU copy time from timestamps, driver shader-cache cold versus warm, or the pipeline
cache comparison; those are in #1022's list and still to be written. Paste the JSON and the adapter line into the issue.

## What it means for the supervisor design (#1023)

1. **A separate output-owner process is enough to keep the output alive through `kill -9` of the renderer on this lane.** The
   output kept presenting through all 600 kills (one interval above two periods, 78.6 ms, which ended at a kill), and the
   picture was held for at most 0.74 s, 0.37 s with a warm cache. The restart machinery costs about 13 ms (reap and respawn) plus
   about 3 ms of host read per frame; nearly all the hold is the worker's own cold start.
2. **The 2 s budget is therefore a budget for the real renderer's cold start**: roughly 1.9 s minus the 13 ms respawn and the
   time to render the first frame. Lavapipe's device creation is 68 ms; D3D12 device creation, shader compile and model/asset
   re-upload on a real GPU are **not measured** and may be much larger. The Windows run answers this.
3. **Cold caches matter**: with Mesa's cache off the first frame took about 100 ms longer (255 ms against 125 ms at p50), and the very first
   worker of a run is slower than the rest (268 ms and 338 ms). A supervisor should keep the driver caches warm and not wipe them on restart.
4. **Do not render in the host.** The host stayed at about 9 percent of one core and 29 MB while a worker burned about 1.6 cores
   rendering, and its tick held through the kills (the single interval above two periods was 78.6 ms).
5. **Recommended process model (for #1023, from this data plus reasoning, not a measurement):** a small, long-lived *output host*
   that owns the outputs, the last-frame slot and the supervisor, and never loads the renderer, driver, tracker or encoder
   libraries; the renderer in a worker the host can kill and restart. Processes should be given to the parts that load native
   code that can crash and have a cold start the host can hide behind a held frame: the renderer, and the tracker (camera and
   ONNX runtime), as workers; the audio engine and encoder need their own hold strategy (audio has no "last frame", and an encoder restart
   resets the stream session), which this spike did not test. The hand-off carries frames (a shared texture on Windows, shared
   memory on the CPU lane), and a kill is observed as pipe EOF.

## Not verified

- Anything on Windows, macOS, a real GPU, DX12, tier `R`, or the 1.5 ms GPU hand-off budget.
- The renderer's cold start with a real device, real pipelines and real assets (this scene has one pipeline and no assets).
- The in-process alternative (forced device loss and re-creation) and the list of failures it does not cover.
- wgpu's pipeline cache on versus off; the driver shader cache beyond Mesa's.
- GPU memory and Windows handle counts across the kills; descriptor leaks were checked on Linux only (the host's, not the worker's).
- A loaded machine: the VM was otherwise idle apart from lavapipe. No stress run, and no run with the host's CPU limited.
- The cause of the one interval above 66.7 ms (scheduler contention is a guess).
- A frame torn by a kill was never seen (0 refused), but the test cannot prove it cannot happen: it relies on the worker posting
  only after the write finishes, on the slot hand-back (a slot is rewritten only after the host has read it), and on the host's
  tag check, which looks at the header, footer and frame number, not every pixel.
- Nothing else than SIGKILL (a hang, an out-of-memory kill, an access violation in a driver) was tried; a hung worker that is
  not dead needs a watchdog this spike does not have.
- Seeds: one seed per run; the three runs are not independent samples of one distribution.

## Open questions

- The Chromium and OBS pages above were found through search; direct fetches of `chromium.googlesource.com` and
  `docs.obsproject.com` were refused by this environment's network policy, so only the search summaries were read. The OBS
  `gs_device_loss` callback signatures and OBS's current (non-D3D11) behaviour are not confirmed; someone with access should read
  `libobs/graphics/graphics.h` and Chromium's `docs/gpu` before the supervisor design cites them.
- Should the hold be longer than the last frame: a "reconnecting" card after N seconds, and who draws it, since the renderer is dead?
- Who decides a worker is hung rather than dead, and with what timeout (the 2 s budget leaves little room for detection)?
- Does the Windows host need its own wgpu device at all, or only a D3D12/D3D11On12 device for the hold texture? (The spike used wgpu to reuse
  `SpoutBridge` unchanged.)
- Is a crash counter with degradation (Chromium's model: restart, then a lower-risk renderer after repeated crashes) wanted for
  the renderer, and what is the lower-risk mode?
- Audio and the encoder (see "Recommended process model"): do they restart in a worker too, or stay in the host?
