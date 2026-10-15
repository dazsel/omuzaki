---
title: "The live-show supervisor: workers, heartbeats, hold-last-frame, restart policy"
summary: "workers, heartbeats, hold-last-frame, the restart policy and the contracts for the watchdog, the runtime, the remote encoder and the acceptance run"
area: design
---

# The live-show supervisor

**Status:** proposed (change #2068). No code: the contracts here are built by the supervisor runtime change (change map
row 16 on #1885: #1024, #1025, #1224) and proven by the acceptance run (#1456).
**Feature:** BR-23 (the live-show watchdog). **Issue:** #1023.
**Spec:** v3 §25 (BR-23), §8 (runtime model, threads), §10 (`startup.live`), §22 (PF-23 source ownership, PF-26 health
meter).
**Builds on:** the worker-process spike (#1022, merged as #1995: `spikes/worker-process/`,
`docs/research/2026-10-worker-process.md`) and its known limits recorded on #1995; `docs/design/record.md` (D2, D6,
D11, §5: the encoder is a supervised worker, the ring and the files outlive it); `docs/design/audio-api.md` (the
engine and its rings); `omuzaki-render-api` ("device loss is terminal"); `omuzaki-anim-api` (`LostPolicy`).
**Prior art:** Chromium's GPU-process restart and its crash budget (BSD-3-Clause): restart a crashed GPU process,
count crashes in a window, forgive one per elapsed interval, and degrade after the limit. Erlang/OTP supervisors
(Apache-2.0): a restart intensity of `MaxR` restarts in `MaxT` seconds, after which the supervisor gives up and
escalates. This page takes Chromium's long-lived non-rendering parent and OTP's intensity rule, and adds what neither
has: an output that keeps presenting while the child is gone.

The page settles the eleven items of #1023 with a reason each, then gives the types and the order of work. Numbers
marked *spike* come from the Linux CPU lane of #1995 (software Vulkan on lavapipe, no GPU, 1080p BGRA at 30 fps).
Numbers marked *budget* are targets this design sets. Nothing on this page was measured on Windows or on a GPU; §13
lists the owner run that measures it.

## 1. What this covers

| #1023 item | Where |
|---|---|
| 1. Which workers are processes, threads or tasks; the output host | §2 |
| 2. Editor and live | §3 |
| 3. The channels: control, frames, tracker frames, audio rings, packets | §4 |
| 4. Heartbeats and hang detection per kind | §5 |
| 5. The restart policy and budget, what "failed" means | §6 |
| 6. Hold-last-frame, and after 2 s | §7 |
| 7. State a restarted worker needs; the 2 s budget split | §8 |
| 8. The tracker hand-off to the life layer | §9 |
| 9. Encoder restarts | §10 |
| 10. Visibility and records: status, ops, events, crash dumps | §11 |
| 11. Tests: fakes, the policy table, the kill test, the acceptance harness | §14 |
| Output pacing: the deadline advances after the present completes (#1995's known limit) | §12 |
| The Windows half of #1022 | §13 |
| The contracts for #1024, #1025, #1224 and #1456; the re-cut and the tasks to file | §15, §16 |
| Open questions | §17 |

## 2. The process model

**Decision.** While the show is live there are three kinds of place a supervised thing runs:

```
 studio process (UI, document, ops, automation, connectors)          person-facing; may crash, the show stays up
   │  adapter tasks (Twitch, YouTube, plugins), supervised in-process (Monitor + TaskLauncher)
   │  local socket: commands, status, events (§4.1)
   ▼
 output host process  ── owns: Spout / NDI / vcam senders, RTMP session and muxers, take writers, replay ring,
   │                      the last frame, the presenter tick, the Supervisor; no renderer, no driver, no ONNX,
   │                      no encoder library (libavformat for stream copy only, record.md D11)
   │  stdio pipes per worker: control and heartbeats (§4.2); shared memory: data (§4.3)
   ├── stage worker (kind Renderer): input → track → retarget → life → anim → IK → physics → face → render
   ├── tracker worker (kind Tracker), one per camera tracker: camera capture + ONNX
   ├── audio worker (kind Audio): devices, the 48 kHz graph, effects, voice-to-face cues
   └── encoder worker (kind Encoder), one per distinct encode (record.md D2): FFmpeg encoders, PNG encoding
```

| Worker | Where | Why |
|---|---|---|
| Stage (`Renderer`) | **process** | BR-23 names it and the acceptance is `kill -9`, which only a process survives. It loads the GPU driver, the most crash-prone native code we run; a driver fault or an access violation ends the process whatever Rust does. The spike shows a separate output owner hides its restart: longest hold 0.74 s over 600 kills, kill to first new frame 141 / 212 / 355 ms (p50 / p95 / max). |
| Tracker | **process** | Camera drivers and the ONNX runtime are native and load models of their own; BR-23 names it. Its cold start (camera open, model load) is hidden by the life layer (§9), not by a held frame. |
| Audio engine | **process** | BR-23 names it; device drivers (WASAPI, ASIO, Core Audio) and effect or voice-conversion models (ML-14) are native. Audio has no last frame: a restart is a gap of silence (§7). |
| Encoder | **process** | record.md D11 already puts encoders in a worker and the ring and files in the host. FFmpeg's encoders and hardware encoder drivers are native. |
| Platform adapters | **async task** in the studio | Pure Rust over the network, no native code, so the failure modes are a panic or a hang, which a task boundary catches; `kill -9` cannot target a task and BR-23's acceptance does not need it to. They feed automation and ops, which live in the studio, and they read the secret store there. |
| Output host | **process, never restarted** | It owns everything a viewer can see disconnect: the senders, the RTMP session, the last frame, the files. It does no GPU work beyond copies and loads no renderer, driver, tracker or encoder library, so the code that can crash is elsewhere. The spike's host stayed at about 9 % of one core and 29 MB through 600 kills. |

- **Its name** is the *output host*, binary `omuzaki-output-host` (`omuzaki-host` is taken by #444). Workers are one
  binary, `omuzaki-worker <kind>`, so an install ships one more executable, not four; each kind loads only its own
  native libraries at run time.
- **The supervisor lives in the output host,** next to the outputs it protects: a worker death is observed there
  first (pipe end, §5), and the hold (§7) needs no hop. The studio runs a second `Monitor` for its adapter tasks; it
  merges the host's status into one health view (§11).
- **The studio is not a supervised worker.** If it crashes during a show, the host keeps streaming: the tracker still
  feeds the stage through the host, so the avatar keeps performing on the last scene. Reattaching a restarted studio
  to a running host is a follow-up task (§16, T12). Until it lands, the host keeps the show up after the studio is
  gone, and a newly started studio that finds the host's socket asks the person, then tells the host to stop, which
  finalises every file.
- **Not supervised here:** speech hosts, the AI performer and LLM clients run where their designs put them; BR-23 does
  not list them, and a failure there does not touch the outputs.
- **Children die with the host.** On Linux each worker sets the parent-death signal to SIGKILL at start (rustix's safe
  `set_parent_process_death_signal`); on Windows the host puts every worker in a job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`; on macOS the worker exits when its control pipe closes (§4.2). A host crash
  therefore never leaves orphan workers holding the camera or the GPU.

## 3. Editor and live

**Decision.** The stage runs in the worker whenever the outputs are on, in the editor too. With the outputs off (plain
editing, no show), it runs in the studio process as today.

- **Why not a worker always:** editing needs gizmos, picking, partial face repaint and an immediate response to every
  op, and the document lives in the studio. A worker would put a snapshot round trip under every edit for no
  protection anyone needs: with the outputs off, a renderer crash costs the person a restart, not a viewer's stream.
- **Why not in-process while live:** that is exactly the case BR-23 protects. Two architectures for the live case
  (an in-process one for "small" shows) would double the test matrix of the path that must not fail.
- **What it costs:** one renderer host in the studio and one in the worker. Both drive the same stage and
  `omuzaki-render` code through the same `Renderer` trait; only the frame sink differs (the viewport, or the frame
  slots of §4.3). Going live spawns the workers (counted in `startup.live`, §8).
- **The viewport while live** shows the program frame from the host's *preview slot* (a `FrameSlots` with one
  newest-wins slot that the host fills on each new frame, one copy). Editing while live still works: ops change the
  document in the studio, and the studio tells the stage worker the new revision (`HostMsg::Scene`, §4.2), which
  reloads what changed. The cost of a large edit while live (a mesh rebuild in the worker) is measured by the stage
  worker task (T2), not assumed.

## 4. The channels

### 4.1 Studio and output host: a local socket

A Unix domain socket in the user's runtime folder (mode 0600) on Linux and macOS, a named pipe with
`PIPE_REJECT_REMOTE_CLIENTS` and the user's default DACL on Windows. Length-prefixed messages (u32 little-endian
length, at most 1 MiB, then a `postcard` body). It carries `StudioMsg` (go live, stop, scene revision, a person's
restart, output configuration) and `ShowMsg` (status snapshots once a second, `HealthEvent`s as they happen, output
status). Nothing on it is on a real-time path. The crate for the socket (`interprocess` is the candidate) gets its
licence row in the task that adds it.

### 4.2 Host and worker: the child's stdio pipes

**Decision.** Each worker's control channel is its own stdin (host to worker) and stdout (worker to host): anonymous
pipes the OS creates with the process, with no names, permissions or cleanup, on every OS. Messages are framed as in
4.1 (at most 64 KiB). **The pipe's end is the crash signal:** the host's reader thread sees end-of-file the moment the
worker dies, whatever killed it; the spike measured kill to the next process creation at 13.1 / 20.2 / 43.4 ms
(p50 / p95 / max), reap and respawn included.

- **The worker moves its stdout aside before loading anything native.** ONNX Runtime, FFmpeg and drivers may print to
  stdout, which would corrupt the protocol. The first thing `omuzaki-worker` does is duplicate fd 1 to a private
  descriptor for the protocol and point fd 1 at stderr (rustix's `dup` and `dup2_stdout`; `SetStdHandle` on
  Windows). The host logs each worker's stderr, line by line, at debug level.
- **Messages** (`omuzaki-stream-api::worker`, §15.2): the worker says `Hello` (kind, protocol version, pid) and then
  `Beat`, `Ready`, `Frame`, `Gone`; the host sends `Start`, `SlotFree`, `Keyframe`, `Scene`, `SourceDown`,
  `SourceUp`, `Stop`. A version mismatch on `Hello` is a launch error, never a restart loop.
- **Why not one byte per beat** (#1025's first outline): the stage worker already posts frame messages on the same
  pipe, and a worker needs to tell the host *what* it did, not only that it is alive. A beat is a small message; the
  fake worker of §14 speaks the same protocol.

### 4.3 Data: shared memory, one owner per slot

The data channels live in a new lib crate, **`omuzaki-shm`**, the only crate in this design with `unsafe` (allowed
locally with a why-comment, as `omuzaki-link-probe` does): mapping a shared region and viewing its header as atomics.
The object is created with rustix's `shm` on Linux and macOS (mode 0600, a random 128-bit name, unlinked when the host
ends) and with `CreateFileMappingW` in the `Local\` namespace on Windows; `memmap2` (MIT or Apache-2.0) maps it. Every
type below is safe to use, has a producer end and a consumer end, and carries a **producer epoch** in its header: a
respawned producer bumps it on attach, so the consumer drops whatever the dead one left half written.

| Channel | Type | Producer → consumer | Semantics |
|---|---|---|---|
| Frames (CPU lane) | `FrameSlots` | stage → host; host → encoder (record.md D2) | 3 slots (16 for a recording's encode queue, 4 for the replay buffer, record.md D2). A slot belongs to the producer until it is announced (`Frame { slot, index }`) and to the consumer until it sends `SlotFree { slot }` back: the spike's hand-back, which keeps a slow consumer from reading a slot being rewritten. On a producer's death the consumer reclaims every slot it was not holding; a slot never announced is never read. |
| Frames (GPU lane, Windows) | `SharedTextures` | stage → host | The spike's design: 3 committed D3D12 textures with NT shared handles, a shared fence signalled with `index + 1`, the same hand-back after the host's GPU copy out of the slot completes. The host copies into its own *hold texture* (§7). macOS (IOSurface) and Linux (dma-buf) GPU routes are not designed yet; they use the CPU lane. |
| Tracker frames, voice cues, live inputs | `Latest` | tracker → stage; audio → stage; studio → host → stage | Newest wins, never queued (spec §8): a seqlock over one record of at most N bytes (a `postcard` `TrackFrame`, at most 4 KiB). The reader retries on a torn read and gets `(seq, bytes)`; a stale `seq` is how the stage sees a silent source. |
| Audio | `SampleRing` | audio → host (program, buses, taps of record.md D7) | A single-producer, single-consumer ring of interleaved `f32` frames, head and tail as atomics in the header, sized for 2 s (audio-api's tap length). `omuzaki-audio-api`'s rings are `Arc`-backed heap rings and cannot sit in shared memory; `SampleRing` is the same shape (`push` and `pop` wait-free, no allocation) over the mapped region, and the audio engine writes its host-bound taps into it instead of a heap ring. The engine's internal rings are unchanged. |
| Packets | `PacketChannel` | encoder → host | A byte ring of records: a fixed header (stream, `pts`, `dts`, `duration`, keyframe flag, data length, side-data count), then per side datum its kind, id and length, then the bytes. Sized for 1 s at the encode's maximum rate (record.md §3: 64 MiB for ProRes, 8 MiB for VP9 and H.264). A full channel makes the encoder wait; that backs up the frame queue, whose full state is record.md D2's `StreamError::Full`. |

## 5. Heartbeats and hang detection

**Decision.** Each worker advances one **progress counter** and sends it in `Beat` (at most every 50 ms, and at least
once per main-loop iteration). The host's reader thread stores it in that worker's `HeartbeatCell`; the `Monitor`
compares it each tick (§15.1). Crash detection needs no deadline (the pipe ends); a deadline is only for hangs.

- **The counter counts main-loop iterations, each bounded by a timeout,** not inputs, so an idle worker is not a hung
  one: a tracker with its camera muted or unplugged still loops every 100 ms, and an audio engine with its device
  gone keeps processing blocks on its own timer. That is a requirement on the tracker and audio worker tasks (T4, T6).
  The kind's *work* counter (frames, packets, callbacks) is sent alongside for the health meter.
- **The stage counts frames posted:** it renders at the output cadence whatever its inputs do.

| Kind | Progress | `start_grace` | `hung_after` | Why |
|---|---|---|---|---|
| Renderer (stage) | frames posted | 5.0 s | 0.5 s | 15 frames at 30 fps. The spike's slowest single frame was 110 ms; a shader-compile hitch can be a few hundred ms. Lower would kill healthy workers; higher eats the hold (§8). Grace covers a cold driver cache on the first start of a session (the spike's very first worker: 268 ms warm, 338 ms with the cache off, on lavapipe). |
| Tracker | loop iterations (≤ 100 ms each) | 10.0 s | 1.0 s | Model load and camera open are slow and unmeasured; the life layer covers the gap, so a long grace costs nothing visible. |
| Audio | blocks processed | 3.0 s | 0.25 s | A 250 ms stall is already an audible fault; the engine's period is about 10 ms. |
| Encoder | packets out | 5.0 s | `max(1 s, 4 × frame period × queue depth)`: 2.13 s for a recording at 30 fps (depth 16) | record.md D11's rule, unchanged. |
| Adapter (task) | loop iterations (≤ 5 s each) | 30 s | 30 s | Network reconnects are slow and invisible to viewers; events queue meanwhile. |

**A kill that does not finish.** `Kill` on a hung worker usually ends it at once, but a process stuck in a kernel
driver call (a GPU hang mid-reset) can stay unkillable for seconds. The runtime waits `exit_wait` (1.0 s, *budget*)
for the exit; if none comes it reports `Observation::Abandoned`, which the `Monitor` treats like an exit for spawning,
and logs it. The old process holds no slot that matters (the host reclaims them, §4.3), and the job object or
parent-death signal ends it when the host ends. The thread launcher reports a hung thread the same way. Whether a
real GPU hang ever needs this is an owner-run question (§13).

## 6. The restart policy and budget

**Decision.** The rules of #1024 are confirmed: immediate first restart; exponential backoff after it; a restart
intensity in OTP's form (`max_restarts` within `within` seconds); a stability period that forgives the count
(Chromium's forgive interval, but whole rather than one at a time, because a show that ran 30 s healthy is a new
situation); and `Failed` when the intensity is exceeded.

| Kind | `backoff_first` | `backoff_max` | `max_restarts` / `within` | `stable_after` | `slow_after` |
|---|---|---|---|---|---|
| Renderer, tracker, encoder | 0.25 s | 4.0 s | 5 / 60 s | 30 s | 2.0 s |
| Audio | 0.1 s | 2.0 s | 5 / 60 s | 30 s | 0.5 s |
| Adapter | 1.0 s | 60 s | 10 / 600 s | 300 s | 30 s |

Backoff for attempt `n` inside the window: 0 for `n = 1`, then `min(backoff_first × 2^(n − 2), backoff_max)`. For
the renderer that is spawns at +0, +0.25, +0.5, +1.0, +2.0 s, and a sixth crash in the window makes it `Failed`.

**What `Failed` means for the stream.** Nothing disconnects. The outputs keep presenting what §7 says (the held frame,
then the slate if one is set); the audio stays silent if the audio engine failed; a failed encoder ends its take
cleanly (record.md D6, with a new `StopCause::EncoderFailed`, added by T7) while the RTMP session holds its connection for the
host's reconnect window; a failed adapter stops its events and says so. The person is told at once (§11).

**Who restarts a failed worker:** a person, through `supervisor.restart` from the health meter or the Live UI, which
calls `Monitor::request_restart` (a fresh window). An agent may restart a worker that is *not* failed (a test, a
stuck adapter); for a `Failed` worker the op refuses an agent caller with the ops layer's person-only permission,
because a crash loop the budget stopped is a decision for the person on the show.

**Not decided: degrading after the budget** (Chromium falls back to a safer GPU mode). A "safe renderer" (no MSAA,
no post, the lowest face atlas) is plausible, but its value depends on what actually crashes a DX12 renderer; it waits
for the owner run's data (§17).

## 7. Hold-last-frame

**Decision.**
- **Where it lives:** in the output host, which owns the last complete frame: the *hold texture* on the GPU lane (the
  spike's design: the host copies each new frame out of the shared slot into a texture it owns, so the worker's death
  releases only the worker's references) and a *hold buffer* on the CPU lane (one frame-sized buffer, allocated at go
  live).
- **What is re-presented:** at every tick of the output cadence (§12), each output gets the newest complete frame, or
  the held one again when none arrived. Held frames carry the tick's index and time (§12), so receivers and encoders
  see an unbroken cadence and the recording repeats the frame (record.md D2's "repeat the previous frame").
- **Which outputs hold:** every output fed by the renderer: Spout, NDI, the virtual camera, the preview slot, the
  recorder and the replay buffer's frame feed, and the RTMP video feed. **Not held:** audio (it has no last frame: the
  host writes silence for the gap and marks it, §10); tracker data (the life layer takes over, §9); packets (the
  encoder's frame queue waits, record.md D6); adapter events (they queue in the studio).
- **After 2 s:** `HoldPolicy` decides, per show, from the moment the held frame became the newest one:

  | Hold so far | What happens |
  |---|---|
  | 0 to 2.0 s (`tell_after`) | the held frame, nothing else; this is the BR-23 promise |
  | from 2.0 s | still the held frame; the health meter turns red and the person gets a notice ("the renderer is restarting: viewers see a still frame") |
  | from 5.0 s (`slate_after`), only if the person set a slate | a cut to the slate: an image the person chose (a "be right back" card with alpha), uploaded to the host at go live; the host copies it like a frame. A cut, not a fade, because a fade is GPU work the host does not do. |
  | the renderer recovers | the next new frame replaces the hold or the slate at once; the notice clears |

  With no slate set, the hold continues indefinitely: a frozen avatar is better than a blank or transparent output
  that an OBS scene would show as the avatar vanishing. Who draws a richer "reconnecting" card is not this design's
  question: the slate is the person's own image.

## 8. A restarted stage worker, and the 2 s budget

**What it needs:** the renderer starts from nothing and re-uploads from CPU copies, as render-api says device loss is
terminal. It reads the latest scene snapshot (`HostMsg::Scene`: the document revision the studio last wrote to the
session's live folder, and the content store, both read-only), builds its device and pipelines with the pipeline cache
and the driver's shader cache kept warm (never cleared on restart: the spike's cache-off run added about 130 ms to the
first frame on lavapipe alone), re-uploads meshes and textures, reads the newest tracker and live-input records
(`Latest`) and posts its first frame. The simulation restarts from rest; physics state (spring bones, cloth) is not
carried over, so hair settles within its own settle time after a restart, which is shown, not hidden.

**The split, for a crash** (the acceptance case: from the kill to the first new frame on the outputs):

| Step | *spike* (lavapipe), p50 / max | *budget* | Notes |
|---|---|---|---|
| frame shown before the kill, at most one period old | 33 ms | 33 ms | the hold starts at the last new frame, not at the kill |
| detect, reap, spawn | 13.1 / 43.4 ms | 50 ms | pipe end; the supervisor wakes on the event, not on its tick (§15.3) |
| process start to device | 68 / 127 ms (instance, adapter, device) | 400 ms | DX12 unmeasured: owner run |
| pipelines | 6.5 / 11.4 ms (one pipeline) | 300 ms | the real renderer has many; warm pipeline cache |
| scene snapshot and re-upload | not in the spike (no assets) | 500 ms | measured by the stage worker task (T2) with the kit character |
| first frame drawn and posted | about 47 ms (spawn to first frame 125 ms minus spawn to `Ready` 78 ms) | 100 ms | |
| the host picks it up | at most one period | 33 ms | next tick |
| **total** | **hold 155 / 368 ms; 0.74 s worst of 600 kills** | **1.42 s** | 0.58 s of margin under 2.0 s |

- **A hang adds `hung_after`.** A hung renderer's last frame is already 0.5 s old when the `Monitor` decides, so the
  hold for a hang is bounded by `hung_after` + the crash path (+ `exit_wait` if the kill does not finish): 1.9 s on
  the budget, at the edge. BR-23's acceptance is `kill -9` (a crash); a hang's hold is reported with `slow` set when
  it passes 2.0 s, never hidden. If the owner run shows DX12's cold start eats the margin, the fallback is a **warm
  standby** stage worker (device, pipelines and assets loaded, not rendering), promoted on a crash: it removes device,
  pipelines and upload from the path at the cost of a second copy of the GPU resources. It is not built unless the
  numbers ask for it (§17).
- **`startup.live`** (process start to a performing avatar, at most 2 s) is the same path cold, plus the host's own
  start and the workers spawned in parallel: the tracker's cold start does not gate the first frame, because the life
  layer performs until the tracker is up.

## 9. The tracker and the life layer

**Decision.** "Hands the avatar to the life layer within one frame" means: the first stage frame that starts after
the supervisor marks the tracker `Restarting` already has the life layer taking over (the tracker layer's weight
falling, the life layer driving), and the tracker layer's weight reaches 0 within `LostPolicy::fade_s` (0.5 s by
default) plus one frame. An instant snap would break PF-23's "seamless hand-off; ease to rest".

- **The restart bypasses the grace.** `LostPolicy::grace_s` (0.25 s) exists so a blink of the tracker is not a lost
  face. A dead tracker is not a blink: on `Restarted` for a tracker, the host sends `SourceDown { source }` to the
  stage worker, which tells the retarget to skip the grace and start the fade at the next `apply`.
- **That needs one new input on `Retarget`** (`omuzaki-anim-api`, the Character body area's contract):
  `fn source_lost(&mut self)`, "the source is gone: skip the grace and start the fade at the next `apply`"; the next
  frame with a found face cancels it as today. Without it the stage can only feed `Lost` frames and the hand-off
  starts after the grace, 0.25 s late. It is proposed here and needs that lead's approval as its own small contract
  change before T5 (§16).
- **When the tracker is back** (`Recovered`), the host sends `SourceUp`; the tracker's first found frame fades its
  layer back in as after any lost face.
- **The bound #1456 checks:** life layer driving within one stage frame (33 ms at 30 fps) of `Restarting`, and the
  tracker owning the face channels again within `fade_s` + one frame of its first found frame after `Healthy`.

## 10. Encoders and audio across a restart

**Encoders** (record.md D6 and D11, confirmed): the ring, take writers, muxers and the RTMP session are in the host
and outlive the encoder worker. On the encoder's `Spawned` the host creates a fresh `RemoteEncoder` (§15.4) with a
forced keyframe and `pts` continuing from the frame index (the presenter's tick index, §12, which never resets); the
take continues in the same file when the new `extradata` is byte-identical, else it rolls to `_part2`; frames wait in
the queue meanwhile, and `encoder_restarts` counts the restart. The RTMP session sends nothing during the gap (no
packets exist) and its timestamps jump by the gap, which servers accept; it is not disconnected.

**Audio:** the host's audio aligner sees the audio worker's `SampleRing` run dry and writes silence for the gap, which
it records as a gap in the take's metadata (start, length, cause) so an editor can find it; it never splices the two
sides together. The take does **not** roll to a new part for an audio gap: the WAV sidecars and the program track stay
aligned because the silence fills the time. (#1456's draft said "continues in a new part"; this page corrects it,
§16.)

## 11. Visibility and records

- **The restart log:** every restart appends a `RestartRecord` (when, which worker, why, the attempt, the downtime
  once recovered); the `Monitor` keeps the newest 256 (#1024).
- **The health meter (PF-26)** gets one row per worker: its name, `Health`, restarts in the current window, the last
  reason and downtime, and a red state for `Failed` or a hold past `tell_after`. The rows read `WorkerStatus` (§15.1),
  which is why the observable types sit in `omuzaki-stream-api`: the UI reads them without depending on the
  implementation. The rows themselves are the UI area's task (T9).
- **Ops:** `supervisor.status` (a query: every `WorkerStatus` and the log's newest records) and `supervisor.restart
  { worker }` (a person only for a `Failed` worker, §6). They are registered in `omuzaki-stream` and reach the host
  over the local socket.
- **Events:** each `HealthEvent` is published as an op-log event, so the Theatre and automation (a "renderer
  restarted" trigger) see it.
- **Crash dumps:** off by default. When the person opts in, a worker's crash writes a minidump with stacks and
  registers only, never heap or mapped memory, so no frame, camera image or audio is in it; it is sent to the
  project's crash reporting only with that consent. The reporter (Embark's `crash-handler` and `minidumper`, MIT or
  Apache-2.0, are the candidates) and its privacy review are their own task (T11), checked against
  `docs/design/privacy.md`.

## 12. Output pacing

The host's presenter is the output cadence: Spout, NDI, the virtual camera and the encoders' frame feed all tick from
it. #1995 left two known limits in the spike's loop; this design fixes both in the contract.

- **The deadline advances after the present completes.** The spike computed the next deadline at wake-up, before the
  output work, so a present that ran long (Spout's send, a GPU wait) could be followed by another less than a period
  later, and the time spent sending was not counted as missed (Codex's finding on #1995, `cpu_host.rs:114`). Here
  `next_deadline` takes `done`, the time *after* every output's `submit` returned.
- **The cadence is a grid, not a chain.** The spike's `max(deadline, now) + period` added each wake-up's overshoot to
  the period, so its output ran at about 29.7 fps (median interval 33.5 ms). Here deadlines stay on
  `origin + k × period`: a tick that finished before its next grid point keeps it; a tick that overran skips to the
  first grid point after `done` and counts the points it skipped as missed ticks (shown in `OutputStatus.dropped`).
  Each present carries its grid index `k` and grid time, never the wall time of the call, so receivers and encoders
  see an exact cadence and `pts` follows `k`.
- **The frame period is validated** (the second limit): it comes from the output's rational rate `(num, den)`
  (`OutputConfig.fps`), never from a float, and must be at least 1 ms and at most 1 s; anything else is
  `StreamError::Config` at `start`, so no later arithmetic can overflow or divide by zero.

```rust
/// One output frame period, from a rational rate; at least 1 ms, at most 1 s.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct FramePeriod { nanos: u64 }
impl FramePeriod {
    /// # Errors
    /// `StreamError::Config` if `num` or `den` is 0 or the period is outside `MIN_PERIOD..=MAX_PERIOD`.
    pub fn from_rate(num: u32, den: u32) -> Result<Self, StreamError>;
    pub fn nanos(self) -> u64;
}
pub const MIN_PERIOD: Duration = Duration::from_millis(1);
pub const MAX_PERIOD: Duration = Duration::from_secs(1);

/// The tick after the one due at grid point `index` (time `deadline`), given that its output work finished at `done`
/// (host monotonic nanoseconds). Pure: no clock read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NextTick { pub index: u64, pub deadline: u64, pub missed: u64 }
pub fn next_tick(index: u64, deadline: u64, period: FramePeriod, done: u64) -> NextTick;
// done < deadline + period            -> { index + 1, deadline + period, missed 0 }
// otherwise, m = (done - deadline) / period -> { index + m + 1, deadline + (m + 1) * period, missed m }
```

Tests (in the runtime change): an on-time tick; a tick that overran by 1, 2 and 3.5 periods (`missed` 1, 2, 3, the
next deadline strictly after `done`); a tick finishing 1 ns before the next grid point (kept, missed 0); a tick
finishing exactly on it (`missed` 1, because that point's work could not start on time); 10,000 ticks of random work
times never produce two deadlines less than a period apart or off the grid; `from_rate` rejects `(0, 1)`, `(1, 0)`,
`(2000, 1)` and `(1, 2)` and accepts `(30000, 1001)`.

## 13. The Windows half (owner run)

The Windows DX12 lane of #1022 is written and compile-checked (`spikes/worker-process/src/win/`), never run. Its
numbers come from an owner run of the CI-built binary (#1885's owner-run list, through `spike-build.yml`), and the
budget of §8 is confirmed or changed by them. Listed, not guessed:

1. `wp-spike.exe adapter`, then `wp-spike.exe host --kills 200 --seed 1022 --baseline-s 30 --json
   worker-process-dx12.json`: kill to first new frame and hold (p50, p95, max) against 2.0 s; present intervals;
   `send_errors` and `skipped_ticks`; with Spout received by OBS.
2. The same with the driver's shader cache emptied (cold) and warm, and with wgpu's pipeline cache on and off.
3. Handle counts and GPU memory (`QueryVideoMemoryInfo`) before and after the 200 kills.
4. The GPU copy time of the 1080p hand-off from timestamps, against the 1.5 ms GPU budget, and the host's CPU share.
5. Tagged-frame continuity on the Spout receiver (the maintainer question on #1995): every received frame is whole.
6. The in-process alternative: a forced device loss (`ID3D12Device::RemoveDevice`) and the time to re-create the
   device, re-upload and rebuild pipelines, cold and warm.
7. For this design, beyond the spike: `taskkill /F` of a worker inside a job object leaves no orphan and closes the
   pipe; a `Local\` file mapping is reachable from the worker and not from another user's session; and how long a
   `TerminateProcess` of a worker in a GPU hang (a TDR) takes to complete (`exit_wait`, §5).

Items 3, 4, 6 and 7 are not yet in the spike's binary; adding them is a task on the spike (T13).

## 14. Tests and the `kill -9` plan

- **The policy table** (#1024): pure, deterministic, `now` as a parameter; the cases of #1024's acceptance, renamed for
  `Down` and `Up` (§16), plus: `Abandoned` acts as `Exited` for spawning; a worker's `slow_after` sets `slow`.
- **Fake workers** (#1025): the `fake-worker` binary speaks the §4.2 protocol: `--beat-ms N` (default 33),
  `--startup-ms N`, `--crash-after-ms N` (exit code 3), `--hang-after-ms N` (stops beating and stays alive),
  `--role encoder` (#1224). `exit_wait` cannot be tested on a real child (SIGKILL ends even a stopped process), so it
  is tested with a fake `WorkerHandle` whose kill takes effect only after a set delay. A fake output host thread
  presents a frame counter at 30 Hz through the real `next_tick` and the real hold.
- **The real-process kill test** (#1025, Linux CI) and **the acceptance run** (#1456, the local lane on real workers):

| Worker | Fake (CI, #1025 and #1224) | Real (#1456, Windows and Linux) |
|---|---|---|
| Stage (`Renderer`) | `kill -9` its pid: `Healthy` within 2.0 s; the fake host's longest gap between presents ≤ 50 ms; one `Crashed` record, attempt 1; other pids unchanged | 10 kills at random frames: no receiver disconnected (Spout reader attached, vcam open, RTMP connected); hold < 2.0 s at p100; no dropped frame outside the hold; JSON line per kill |
| Tracker | as above, with `Down` and `Up` observed in order | life layer driving within one stage frame of `Restarting`; tracker owning the face again within `fade_s` + one frame of its first found frame after `Healthy` (the stage's channel-owner log) |
| Audio | as above | the stream's audio gap ≤ the restart time + one audio period, silent and marked as a gap in the take; no new part |
| Encoder | `kill -9` during a recording: the take does not end, `status.bytes` does not drop, the first packet after is a keyframe, `encoder_restarts == 1`; `parts == 1` with identical `extradata`, 2 with changed | the recording continues (`_part2` only if `extradata` changed); the RTMP session keeps its connection; the replay ring is intact and still saves |
| Adapter | a panic and a hang in a supervised task: restarted; the hung one aborted | not a process: covered by the fake case; a real adapter's disconnect is the connector host's test |
| Every kind | `--hang-after-ms 1000`: killed and replaced within `hung_after` + 0.5 s of the last beat, `Hung`; `--crash-after-ms 50` always: `Failed` after `max_restarts`, pid gone, one `GaveUp`, `restart(id)` revives it; `stop` leaves no child | the restart log has one record per kill with the right reason; the health meter showed each |

Each CI test finishes in under 10 s, with margins of at least twice the asserted bound, and passes 20 runs in a row
(the count is in the PR). The kill tests are `#[cfg(unix)]`; the Windows cross-build compiles.

## 15. The contracts

The enum rules of `CLAUDE.md` apply to every type in an `*-api` crate below: every struct-like variant is
`#[non_exhaustive]` and has a constructor (`Health::restarting(attempt)`, `RestartReason::crashed(code)`,
`HealthEvent::recovered(worker, downtime, slow)` and so on), and the enums themselves are `#[non_exhaustive]`. Docs on
every public item live in the code; these are shapes. `Seconds` and `Clock` are `omuzaki-core`'s.

### 15.1 What everyone reads: `omuzaki-stream-api::supervisor` (new module)

```rust
/// A supervised worker: its index in the supervisor's spec list.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct WorkerId(pub u16);

#[non_exhaustive]
pub enum WorkerKind { Renderer, Tracker, Audio, Encoder, Adapter }   // Renderer is the stage worker (§2)

#[non_exhaustive]
pub enum Health {
    Starting,
    Healthy,
    Restarting { attempt: u32 },
    Backoff { attempt: u32, until: Seconds },
    Failed { restarts: u32 },
}

#[non_exhaustive]
pub enum RestartReason {
    Crashed { exit_code: Option<i32> },   // None when killed by a signal
    Hung { silent_for: Seconds },
    Requested,                            // a person's or an agent's `supervisor.restart`
}

#[non_exhaustive]
pub enum HealthEvent {
    Restarted { worker: WorkerId, reason: RestartReason, attempt: u32 },
    Recovered { worker: WorkerId, downtime: Seconds, slow: bool },
    GaveUp { worker: WorkerId, restarts: u32 },
    Abandoned { worker: WorkerId },       // a kill that did not finish within exit_wait (§5)
}

pub struct RestartRecord {
    pub at: Seconds, pub worker: WorkerId, pub reason: RestartReason, pub attempt: u32,
    pub downtime: Option<Seconds>,        // filled on recovery
}

/// One health-meter row (§11).
pub struct WorkerStatus {
    pub id: WorkerId, pub name: String, pub kind: WorkerKind, pub health: Health,
    pub restarts_in_window: u32, pub last: Option<RestartRecord>, pub pid: Option<u32>,
    pub work: u64,                         // the kind's work counter: frames, packets, blocks (§5)
}
```

All of these derive `Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema` (the status crosses the local socket
and the ops layer).

### 15.2 The worker protocol: `omuzaki-stream-api::worker` (new module)

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

#[non_exhaustive]
pub enum WorkerMsg {                      // worker → host, on the worker's private stdout (§4.2)
    Hello { kind: WorkerKind, version: u32, pid: u32 },
    Beat { progress: u64, work: u64 },
    Ready { device_ms: f32, pipelines_ms: f32, upload_ms: f32 },   // the §8 split, measured on every start
    Frame { slot: u32, index: u64 },      // a frame complete in slot (FrameSlots or SharedTextures)
    Gone { reason: String },              // a clean, explained exit (a config error): the Monitor still counts it
}

#[non_exhaustive]
pub enum HostMsg {                        // host → worker, on the worker's stdin
    Start { channels: ChannelNames },     // the shared objects to attach, with this producer's epoch
    SlotFree { slot: u32 },
    Keyframe,
    Scene { rev: u64 },                   // the stage reloads the snapshot of that revision (§3, §8)
    SourceDown { source: WorkerId },      // §9
    SourceUp { source: WorkerId },
    Stop,                                 // finish and exit 0
}

/// The names of the shared objects a worker attaches to; opaque strings made by the host.
pub struct ChannelNames { pub entries: Vec<(ChannelRole, String)>, pub epoch: u32 }
#[non_exhaustive]
pub enum ChannelRole { Frames, Preview, Tracker, VoiceCues, LiveInputs, Audio, Packets }

/// Length-prefixed postcard framing over any byte stream.
pub fn write_msg<W: Write, M: Serialize>(w: &mut W, msg: &M) -> Result<(), StreamError>;
pub fn read_msg<R: Read, M: DeserializeOwned>(r: &mut R, buf: &mut Vec<u8>) -> Result<Option<M>, StreamError>; // None at end of stream
```

### 15.3 The channels: `omuzaki-shm` (new lib crate)

```rust
pub struct ShmError { /* what, os error */ }
/// Frame slots with ownership hand-back (§4.3).
pub struct FrameSlots { /* header + n slots of `slot_bytes` */ }
impl FrameSlots {
    pub fn create(slots: u32, slot_bytes: usize) -> Result<(Self, String), ShmError>;  // the host; returns the name
    pub fn open(name: &str) -> Result<Self, ShmError>;                                  // the producer
    pub fn write(&self, slot: u32, bytes: &[u8]) -> Result<(), ShmError>;              // producer, owned slots only
    pub fn read_into(&self, slot: u32, out: &mut [u8]) -> Result<(), ShmError>;       // consumer, announced slots only
}
/// Newest-wins record (seqlock).
pub struct Latest { /* header + one record of at most max_bytes */ }
impl Latest {
    pub fn create(max_bytes: usize) -> Result<(Self, String), ShmError>;
    pub fn open(name: &str) -> Result<Self, ShmError>;
    pub fn publish(&self, bytes: &[u8]) -> Result<u64, ShmError>;        // returns the new seq
    pub fn read(&self, out: &mut [u8]) -> Option<(u64, usize)>;          // (seq, len); retries a torn read
}
/// SPSC ring of interleaved f32 frames.
pub struct SampleRing { /* header (head, tail, epoch, channels) + samples */ }
impl SampleRing {
    pub fn create(channels: u16, frames: usize) -> Result<(Self, String), ShmError>;
    pub fn open(name: &str) -> Result<Self, ShmError>;
    pub fn push(&self, interleaved: &[f32]) -> usize;                    // frames written; wait-free, no allocation
    pub fn pop(&self, out: &mut [f32]) -> usize;                         // frames read
    pub fn epoch(&self) -> u32;
}
/// Byte ring of packet records (§4.3).
pub struct PacketChannel { /* header + bytes */ }
impl PacketChannel {
    pub fn create(capacity: usize) -> Result<(Self, String), ShmError>;
    pub fn open(name: &str) -> Result<Self, ShmError>;
    pub fn send(&self, packet: &Packet) -> Result<bool, ShmError>;       // false when full: the caller waits
    pub fn recv(&self, out: &mut Vec<Packet>) -> Result<usize, ShmError>;
}
```

`omuzaki-shm` depends on `omuzaki-stream-api` only for `Packet`; it is an implementation crate in the Broadcast and
live area. Its tests: every type round-trips across two real processes; a producer killed mid-write never yields a
torn record to the consumer (`Latest` retries, `FrameSlots` never announces, rings bump the epoch); loom or Miri on the
ring indices.

### 15.4 The policy (#1024) and the runtime (#1025): `omuzaki-stream::watchdog`

```rust
pub struct RestartPolicy {
    pub backoff_first: Seconds, pub backoff_max: Seconds, pub max_restarts: u32, pub within: Seconds,
    pub stable_after: Seconds,
}
pub struct WorkerSpec {
    pub name: String, pub kind: WorkerKind, pub start_grace: Seconds, pub hung_after: Seconds,
    pub slow_after: Seconds, pub restart: RestartPolicy,
}
impl WorkerSpec { pub fn defaults_for(kind: WorkerKind, name: impl Into<String>) -> Self; }   // the §5 and §6 tables

/// Worker side: wait-free `beat`; monitor side: `read`.
#[derive(Clone)] pub struct HeartbeatCell(Arc<AtomicU64>);
impl HeartbeatCell { pub fn new() -> Self; pub fn beat(&self); pub fn set(&self, progress: u64); pub fn read(&self) -> u64; }

pub enum Observation { Spawned, Beat(u64), Exited { code: Option<i32> }, Abandoned }
pub enum Action {
    Spawn(WorkerId), Kill(WorkerId),
    Down(WorkerId),     // the worker stopped serving: hold, hand off, mark a gap (§7, §9, §10)
    Up(WorkerId),       // it serves again
    Event(HealthEvent),
}
pub struct RestartLog { /* the newest RESTART_LOG_LEN records */ }
pub const RESTART_LOG_LEN: usize = 256;
impl RestartLog { pub fn iter(&self) -> impl Iterator<Item = &RestartRecord>; pub fn len(&self) -> usize; }

pub struct Monitor { /* one slot per spec */ }
impl Monitor {
    pub fn new(specs: Vec<WorkerSpec>, now: Seconds) -> Self;
    pub fn observe(&mut self, now: Seconds, id: WorkerId, obs: Observation, out: &mut Vec<Action>); // `out` cleared first
    pub fn tick(&mut self, now: Seconds, out: &mut Vec<Action>);                                     // deadlines, due spawns
    pub fn request_restart(&mut self, now: Seconds, id: WorkerId, out: &mut Vec<Action>);             // also revives Failed
    pub fn health(&self, id: WorkerId) -> Health;
    pub fn status(&self, id: WorkerId) -> WorkerStatus;
    pub fn log(&self) -> &RestartLog;
}

pub struct ExitInfo { pub code: Option<i32> }
pub trait WorkerLauncher: Send + Sync {
    fn spawn(&self, id: WorkerId, channels: &ChannelNames) -> Result<Box<dyn WorkerHandle>, LaunchError>;
}
pub trait WorkerHandle: Send {
    fn heartbeat(&self) -> &HeartbeatCell;            // set by the handle's reader from `Beat`
    fn try_wait(&mut self) -> Option<ExitInfo>;       // Some once the worker has ended (crash, kill or exit)
    fn kill(&mut self);                               // SIGKILL / TerminateProcess; never waits
    fn send(&mut self, msg: &HostMsg) -> Result<(), LaunchError>;
    fn pid(&self) -> Option<u32>;
}
pub struct WorkerCommand { pub program: PathBuf, pub args: Vec<OsString>, pub env: Vec<(OsString, OsString)> }
pub struct ProcessLauncher { /* a WorkerCommand; env applied on every OS (#1995's Windows finding) */ }
pub struct ThreadLauncher { /* a closure per worker; a panic is a crash; a hung thread is abandoned and counted */ }
pub struct TaskLauncher { /* an async fn per adapter on the studio's runtime; a panic is a crash; kill aborts */ }
#[non_exhaustive] pub enum LaunchError { Spawn { what: String }, Protocol { what: String }, Closed }

/// What the rest of the show does while a worker is down (§7, §9, §10). Called on the supervisor thread: never blocks.
pub trait Handoff: Send + Sync {
    fn down(&self, id: WorkerId, kind: WorkerKind);
    fn up(&self, id: WorkerId, kind: WorkerKind);
}

pub struct Supervisor { /* its own thread: wakes on a child event or every `tick`, observes, ticks, acts */ }
impl Supervisor {
    pub fn start(workers: Vec<(WorkerSpec, Box<dyn WorkerLauncher>)>, handoff: Arc<dyn Handoff>,
                 clock: Arc<dyn Clock>, cfg: SupervisorConfig) -> Result<Supervisor, LaunchError>;
    pub fn status(&self) -> Vec<WorkerStatus>;
    pub fn log(&self) -> Vec<RestartRecord>;
    pub fn events(&self) -> std::sync::mpsc::Receiver<HealthEvent>;
    pub fn send(&self, id: WorkerId, msg: HostMsg);  // queued to that worker; dropped while it is down
    pub fn restart(&self, id: WorkerId);             // a person's restart (request_restart)
    pub fn stop(self);                               // Stop, then kill after `stop_wait`; waits for every child
}
pub struct SupervisorConfig { pub tick: Duration, pub exit_wait: Duration, pub stop_wait: Duration }
// defaults: tick 50 ms, exit_wait 1 s, stop_wait 2 s
```

**The rules** are #1024's, exact, with these changes: `Down` and `Up` replace `HoldOutputs` and `ReleaseOutputs` and
are emitted for every kind (the `Handoff` decides what they mean; `WorkerSpec.hold_outputs` is gone); `Recovered`'s
`slow` is `downtime > slow_after` instead of a fixed 2.0 s; `Abandoned` counts as `Exited` for "a spawn only after the
old instance ended", and emits one `HealthEvent::Abandoned`. `observe` and `tick` allocate nothing once `out` has
capacity; `beat` is one relaxed atomic add.

**The runtime** maps actions onto launchers and the `Handoff`; one reader thread per child parses `WorkerMsg` into
the handle's `HeartbeatCell` and forwards `Frame` and `Ready` to the host's frame intake. The supervisor thread waits
on a channel the readers signal at end of stream, with `tick` as the timeout, so a crash is observed at once (§8's
50 ms detect budget), not at the next tick. `stop` leaves no zombie: every child is waited.

### 15.5 The remote encoder (#1224): `omuzaki-stream::encode_remote`

```rust
pub struct RemoteEncoder { /* producer end of a FrameSlots queue; consumer end of a PacketChannel; the worker's id */ }
impl RemoteEncoder {
    pub fn new(sup: &Supervisor, worker: WorkerId, frames: FrameSlots, packets: PacketChannel, spec: StreamSpec) -> Self;
}
impl Encoder for RemoteEncoder {
    // send: copy into a free slot and announce it; StreamError::Full when none is free (record.md D2).
    // receive: drain the PacketChannel, side data included, metered by data.len() plus every side datum.
    // request_keyframe: HostMsg::Keyframe. spec: the worker's StreamSpec with its extradata.
    // A dead or hung worker surfaces as StreamError::Encode; the host makes a fresh RemoteEncoder on Spawned (§10).
}
```

### 15.6 The acceptance run (#1456): `tools/chaos`

No library contract. The kill script reads `supervisor.status` and the events over the local socket and writes one
JSON line per kill:

```json
{"worker":"stage","kind":"Renderer","kill_at_s":123.456,"signal":"SIGKILL","receivers":{"spout":{"gaps_ms":[33.4],"disconnected":false}},
 "hold_ms":412.0,"to_healthy_ms":455.0,"parts":["take_part1.webm"],"log_reason":"Crashed","log_attempt":1,
 "health_text":"Renderer restarted (crashed), back in 0.46 s","handoff_ms":null}
```

## 16. The re-cut, and the tasks this design unlocks

**Confirmed, with changes** (each issue's body is updated by the lead to point at this page):
- **#1024, the watchdog policy:** confirmed, with the §15.4 changes (`Down` and `Up`, `slow_after`, `Abandoned`,
  `status`), and its observable types placed in `omuzaki-stream-api::supervisor` (§15.1). Its acceptance cases
  stand with the renames; one case is added for `Abandoned` and one for a per-kind `slow_after`.
- **#1025, the supervisor runtime:** confirmed, with the §4.2 protocol in place of one byte per beat, `Handoff` in
  place of `OutputHold`, `spawn` taking the `ChannelNames`, `status` (rows of `WorkerStatus`) in place of `health`,
  `start` taking a `SupervisorConfig` and returning a `Result`, event-driven wake-up, `exit_wait`, `send` and
  `TaskLauncher`. Its acceptance stands; one case
  is added for an unkillable child (`exit_wait`) and one for a supervised task's panic and hang.
- **#1224, the remote encoder:** confirmed as §15.5, over `omuzaki-shm`'s `FrameSlots` and `PacketChannel`.
- **#1456, the acceptance run:** confirmed as §14 and §15.6, with two corrections: an encoder restart rolls to
  `_part2` only when `extradata` changes (record.md D6), and an audio kill leaves a marked silent gap, not a new part
  (§10). It runs on the real workers, so it cannot finish inside the runtime change: it moves out of change map row
  16 to after T1 to T6.
- **Change map row 16** (#1024, #1025, #1224) also builds `omuzaki-shm` (§15.3), the worker protocol (§15.2), the
  status types (§15.1) and the pacing rule (§12), in this order: status types and protocol; `omuzaki-shm`; the policy;
  pacing; the runtime with fake workers; the remote encoder. Its `omuzaki-stream-api` part is this area's own
  contract; the health meter (UI) and the ops layer (Agents and ops) are its callers, so those leads co-sign the
  §15.1 shapes.

**To file** (each with its fence, contract and acceptance from this page):

| | Task | Area | Depends on |
|---|---|---|---|
| T1 | The output host: the binary, the presenter (§12), the hold and the slate (§7), the frame intake, the outputs fan-out, the local socket | Broadcast and live | row 16 |
| T2 | The stage worker: the stage and renderer in `omuzaki-worker stage`, the scene snapshot reload, the `Ready` split (§8) | Render, stage and V0 (co-sign) | row 16, T1 |
| T3 | The Windows GPU frame channel (`SharedTextures`) in the product, after the owner run | Broadcast and live, local lane | T1, §13 |
| T4 | The tracker worker | Tracking and AI (co-sign) | row 16 |
| T5 | `Retarget::source_lost` (§9), a contract change | Character body | none |
| T6 | The audio worker and the host-bound `SampleRing` taps | Broadcast and live | row 16, audio engine core (row 11) |
| T7 | Encoder restarts in the recorder and the replay buffer (§10) | Broadcast and live | row 16, #2066 |
| T8 | Adapter supervision in the studio (`TaskLauncher`) | Broadcast and live | row 16, connector host (row 10) |
| T9 | The health meter rows (§11) | UI and UX | row 16 |
| T10 | The `supervisor.*` ops and events | Broadcast and live (ops co-sign) | row 16, T1 |
| T11 | Opt-in crash dumps (§11) | Broadcast and live, privacy review | T1 |
| T12 | The studio reattaching to a running host (§2) | Broadcast and live | T1 |
| T13 | The spike's missing Windows measurements (§13 items 3, 4, 6, 7) | Broadcast and live | none |

## 17. Open questions

- **DX12's cold start** decides whether §8's budget holds on Windows: the owner run (§13). If it does not, the warm
  standby of §8 is the plan, and its GPU memory cost is measured first.
- **Degrading after the budget:** is a "safe renderer" wanted after `Failed`, and what is it (§6)? Waits for data on
  what crashes a DX12 renderer.
- **GPU frame channels on macOS and Linux** (IOSurface, dma-buf): not designed; the CPU lane (3.2 ms per 1080p frame
  in the host, *spike*) is the route until they are.
- **Does the Windows host need its own wgpu device,** or only a D3D12 or D3D11On12 device for the hold texture and
  Spout (#1995's open question)? T1 decides it from the owner run.
- **A GPU hang that `TerminateProcess` cannot finish:** is `exit_wait` of 1.0 s right (§5, §13 item 7)?
- **Edits while live:** the cost of a large edit's reload in the stage worker (§3) is measured by T2; if it is too
  slow, a delta protocol replaces the revision reload.
- **The decision record and the spec:** #1023's fence includes an ADR and the §8 threads note. This change's file list
  (#2068) is this page only, so the ADR (the process model of §2 and the editor rule of §3) and the §8 amendment
  ("render on the main thread" becomes "in the stage worker while the outputs are on") follow when the design is
  accepted.
- **The Chromium and OBS sources** were read through search summaries only in #1995; the prior-art notes here rely on
  the same summaries and say no more than they do.
