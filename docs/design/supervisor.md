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
   │                      the last frame, the presenter tick, the Supervisor; no renderer, no ONNX, no encoder
   │                      library (libavformat for stream copy only, record.md D11); on Windows's GPU lane, one
   │                      D3D device for copies and Spout only (§2, "The host's own GPU device")
   │  a duplex control channel per worker, as its stdin (§4.2); shared memory: data (§4.3)
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
| Output host | **process, never restarted** | It owns everything a viewer can see disconnect: the senders, the RTMP session, the last frame, the files. It renders nothing, compiles no shaders of ours and loads no renderer, tracker or encoder library, so most of the code that can crash is elsewhere. The one exception is the Windows GPU lane's device (below). The spike's host stayed at about 9 % of one core and 29 MB through 600 kills. |

- **Its name** is the *output host*, binary `omuzaki-output-host` (`omuzaki-host` is taken by #444). Workers are one
  binary, `omuzaki-worker <kind>`, so an install ships one more executable, not four; each kind loads only its own
  native libraries at run time.
- **The supervisor lives in the output host,** next to the outputs it protects: a worker death is observed there
  first (the control channel's end, §4.2), and the hold (§7) needs no hop. The studio runs a second `Monitor` for its adapter tasks; it
  merges the host's status into one health view (§11).
- **The studio is not a supervised worker.** If it crashes during a show, the host keeps streaming: the tracker still
  feeds the stage through the host, so the avatar keeps performing on the last scene. Reattaching a restarted studio
  to a running host is a follow-up task (§16, T12). Until it lands, the host keeps the show up after the studio is
  gone, and a newly started studio that finds the host's socket asks the person, then tells the host to stop, which
  finalises every file.
- **Not supervised here:** speech hosts, the AI performer and LLM clients run where their designs put them; BR-23 does
  not list them, and a failure there does not touch the outputs.
- **Children die with the host, with no window.** A worker learns of its host's death three ways, and none depends on
  winning a race with the spawn:
  - **on every OS, the control channel's end:** each worker reads its control channel (§4.2) on a thread of its own,
    and its end of file (the host died, so the kernel closed the host's end) makes the worker exit at once. The
    channel exists before the child does, so there is nothing to register late;
  - **on Linux, also the parent-death signal:** the worker sets it to SIGKILL (rustix's safe
    `set_parent_process_death_signal`) and then **checks `getppid()` against the host's pid** (passed in its
    arguments); if the host died before the signal was set, the worker has already been reparented and exits. This
    is the usual recheck that closes the registration race;
  - **on Windows, a job object the host joins first:** at its own start, before any spawn, the host puts *itself* in
    a new job with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. A child process is created inside its parent's job, so every
    worker is in the job from its first instruction (no suspended start and later assignment is needed), and the
    job's last handle closing with the host kills them all.

  A host crash therefore never leaves orphan workers holding the camera or the GPU.
- **The host's own GPU device (Windows GPU lane).** The host owns a D3D device to open the shared textures, copy each
  new frame into its hold texture and hand it to Spout (§4.3, §7), so GPU runtime and driver code does run in the
  host. Three things contain that:
  - **its work is the smallest a GPU can do:** copies and Spout's D3D11 interop, no shader of ours, no pipeline
    compilation, no uploads of assets;
  - **a host device loss is recovered in place, not fatal:** on `DXGI_ERROR_DEVICE_REMOVED` (a TDR) the host keeps a
    CPU copy of the last frame (an asynchronous read-back once per new frame on the GPU lane too, 8.3 MB at 1080p, off
    the presenter's path; its cost is measured by T1), feeds NDI,
    the virtual camera, the encoders and the preview from it, re-creates its device and hold texture, re-creates the
    Spout sender under the same name (Spout receivers reconnect to a sender by name), and asks the stage for a new
    generation of shared textures (a restart of the stage, counted as `Requested`). Spout drops for that time; the
    other outputs do not;
  - **a native crash in the host's driver code still ends the show.** That residual risk is stated, not hidden. If the
    owner run (§13, item 8) or the field shows it is real, the next step is to move the GPU presentation (the hold
    texture and the Spout sender) into a *presenter* worker of its own, which the host restarts like any other while
    the CPU-fed outputs carry on; it is not built until the numbers ask for it (§17).

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

### 4.2 Host and worker: a duplex control channel as the child's stdin

**Decision.** Each worker's control channel is one **duplex** channel that the host creates before the spawn and
hands to the child as its **stdin**: a `socketpair` on Linux and macOS (`UnixStream::pair`, the child's end passed
with `Stdio::from(OwnedFd)`), and on Windows one instance of a duplex named pipe with a random name, created with
`FILE_FLAG_FIRST_PIPE_INSTANCE` and `PIPE_REJECT_REMOTE_CLIENTS`, its client handle opened before the spawn and
passed with `Stdio::from(OwnedHandle)`. Messages are framed as in 4.1 (at most 64 KiB), each frame starting with a
fixed magic. **The channel's end is the crash signal:** the host's reader thread sees end of file the moment the
worker dies, whatever killed it; the spike measured kill to the next process creation at 13.1 / 20.2 / 43.4 ms
(p50 / p95 / max), reap and respawn included.

- **The protocol is not on stdout.** ONNX Runtime, FFmpeg and drivers may print to stdout, through the Rust runtime,
  a C runtime's `printf` or a statically linked CRT of their own, and no redirect inside the worker reaches all of
  those on Windows (`SetStdHandle` does not retarget a CRT's already-open descriptor). So the worker's stdout and
  stderr are both plain log pipes that the host reads, and nothing a library prints can touch the protocol.
- **The host reads worker logs with fixed limits:** lines are cut at 4 KiB (the rest of an over-long line is
  discarded up to its newline and counted), at most 100 lines a second per worker are logged at debug level and the
  rest only counted, and each reader holds one 8 KiB buffer, never more. A worker that floods its output costs the
  host a counter, not memory. A malformed control frame (bad magic, an over-long length, a message that does not
  decode) is a protocol fault: the host kills that worker and restarts it with the reason `Protocol` (§15.1).
- **Shared memory travels as handles, not names, on Unix:** the host sends each object's descriptor over the
  socketpair with `SCM_RIGHTS` (rustix's safe `sendmsg` and `recvmsg`), so no name needs to exist (§4.3).
- **No hot loop writes the channel.** A worker's real-time and per-frame loops (the audio callback, the render loop,
  the tracker's inference loop) never do I/O on it: they bump a wait-free counter (`HeartbeatCell::beat`, one relaxed
  add) and push announcements (`Frame`, `SlotFree`) into a small wait-free queue. One **writer thread** per worker,
  outside every real-time path, owns the channel's write end: every 50 ms it sends one coalesced `Beat` with the
  counter's current value, and it drains the announcement queue as it fills. If the host is slow to read, the writer
  thread blocks, never a hot loop; the announcement queue cannot overflow because a worker never has more
  announcements outstanding than it has slots (3 for the stage, the queue depth for an encoder) and the queue holds
  twice that. A worker whose main loop is truly stuck stops advancing the counter, so the beats the writer sends stop
  showing progress, and the host sees a hang.
- **Messages** (`omuzaki-stream-api::worker`, §15.2): the worker says `Hello` (kind, protocol version, pid,
  generation) and then `Beat`, `Ready`, `Frame`, `SlotFree`, `Opened`, `Gone`; the host sends `Start`, `Frame`,
  `SlotFree`, `Keyframe`, `Scene`, `Attach`, `SourceDown`, `SourceUp`, `Stop`.
- **The handshake,** for every kind: spawn; `Hello`; the host sends `Start` with the generation's shared objects and
  the kind's start parameters (`StartParams`: for an encoder its `EncodeConfig`); then it **replays the worker's
  latched state** (below); the worker attaches, opens what it drives and answers with its kind's reply (an encoder:
  `Opened { spec }`, the `StreamSpec` with its `extradata`, which the host compares byte for byte with the previous
  generation's for record.md D6's same-file rule; others: nothing); then `Ready` with its start-up split, then work.
- **Failures in the handshake feed the policy.** A spawn that fails (no executable, out of resources), a channel that
  ends before `Hello`, or a worker that says `Gone { reason }` (a missing codec, a bad config) is reported to the
  `Monitor` as `LaunchFailed` (§15.4): it is an attempt like a crash, consumes the restart budget, waits out the
  backoff and is logged with its reason, so a config that can never work ends in `Failed` with that reason, not in an
  endless loop. A failure that cannot get better by retrying (a protocol version or generation mismatch on `Hello`,
  an executable that is not ours) is `LaunchFailed { permanent: true }` and makes the worker `Failed` at once.
- **State messages are latched and replayed; commands are not.** `Scene`, `SourceDown`, `SourceUp` and `Attach` carry
  state that a worker must have to be correct, so the host keeps the newest of each (per source for `SourceDown` and
  `SourceUp`, per role for `Attach`) for every worker, whether or not it is up, and sends them all right after
  `Start` on every spawn. An edit made while the stage is restarting therefore reaches the new stage before its first
  frame. `Frame`, `SlotFree` and `Keyframe` are commands about one generation's objects: while a worker is down they
  are refused with `SendError::Down` so the caller knows (a fresh encoder starts with a keyframe anyway, §10), never
  queued for a generation that will not understand them.
- **Why not one byte per beat** (#1025's first outline): the stage worker already posts frame messages on the same
  channel, and a worker needs to tell the host *what* it did, not only that it is alive. A beat is a small message;
  the fake worker of §14 speaks the same protocol.

### 4.3 Data: shared memory, one owner per slot

The data channels live in a new lib crate, **`omuzaki-shm`** (`kind = "Lib"`, `layer = "lib"`, so `omuzaki-stream` and
the workers' crates may depend on it), the only crate in this design with `unsafe` (allowed locally with a
why-comment, as `omuzaki-link-probe` does): mapping a shared region and viewing its header as atomics. `memmap2` (MIT or
Apache-2.0) maps every object. Every type below is safe to use and has a producer end and a consumer end.

**No object outlives the show, even after a host crash.** POSIX shared memory keeps its name until `shm_unlink`, and
a host killed by SIGKILL runs no cleanup, so the objects are made so that no name exists to leak:
- **Linux:** `memfd_create` (rustix's safe `memfd_create`): an anonymous object with no name at all, freed when its
  last descriptor or mapping goes. The host sends the descriptor to the worker over the control socketpair
  (`SCM_RIGHTS`, §4.2).
- **macOS** (no `memfd`): `shm_open` with a random 128-bit name, mode 0600, `O_EXCL`, and `shm_unlink` **at once**,
  before the host does anything else with it; the descriptor then travels like Linux's. Only a crash in the
  microseconds between the two calls can leave a name; macOS cannot list its POSIX shared-memory names, so that
  window is stated, not scavenged.
- **Windows:** `CreateFileMappingW` backed by the paging file, named with a random 128-bit name in the session's
  `Local\` namespace with the user's default DACL. A Windows section is reference-counted: it disappears when its
  last handle closes, so a host crash frees every section the workers are not still holding, and theirs go when they
  die with it (§2).

**Fresh objects for every worker generation.** Each spawn of a worker is a new *generation* (a `u32` the host
counts per worker). The host creates every shared object that worker touches anew for that generation, hands them
over with `HostMsg::Start` (descriptors on Unix, names on Windows, §4.2), and drops the previous generation's objects once that generation has ended. Nothing is
reused across generations, so:
- a dead producer's half-written slot, an odd seqlock count or a ring index frozen mid-update is never seen by its
  successor's consumer: the consumer moves to the new objects (`HostMsg::Attach` tells a peer worker, such as the
  stage, to switch to a new tracker's `Latest`);
- an **abandoned thread or task** (§5: only the thread and task launchers abandon; a process worker is never
  replaced while it is alive) cannot race its replacement: it keeps writing into its own generation's objects, which
  no one reads any more (*quarantined*: the host has closed its ends and stopped reading the old channel); the memory
  is freed when that thread finally drops its ends;
- a stale message from an old generation is refused, because every `WorkerMsg` is read from that generation's own
  channel and every `Frame` names a slot of that generation's objects.

The cost is one more set of slots while an abandoned thread lingers (24.9 MB for three 1080p frames), only for as
long as it lives.

| Channel | Type | Producer → consumer | Semantics |
|---|---|---|---|
| Frames (CPU lane) | `FrameSlots` | stage → host; host → encoder (record.md D2) | 3 slots (16 for a recording's encode queue, 4 for the replay buffer, record.md D2). A slot belongs to the producer until it is announced (`Frame { slot, index }`) and to the consumer until it hands it back (`SlotFree { slot }`): the spike's hand-back, which keeps a slow consumer from reading a slot being rewritten. A slot never announced is never read. The message directions follow the data: the stage announces with `WorkerMsg::Frame` and gets `HostMsg::SlotFree`; for an encoder the host announces with `HostMsg::Frame` and gets `WorkerMsg::SlotFree` (§15.2). |
| Frames (GPU lane, Windows) | `SharedTextures` | stage → host | The spike's design: 3 committed D3D12 textures with NT shared handles, a shared fence signalled with `index + 1`, the same hand-back after the host's GPU copy out of the slot completes; new textures and a new fence per generation. The host copies into its own *hold texture* (§7). macOS (IOSurface) and Linux (dma-buf) GPU routes are not designed yet; they use the CPU lane. |
| Tracker frames, voice cues, live inputs | `Latest` | tracker → stage; audio → stage; studio → host → stage | Newest wins, never queued (spec §8): a seqlock over one record of at most N bytes (a `postcard` `TrackFrame`, at most 4 KiB). **Reads are bounded:** a reader tries at most `LATEST_READ_TRIES` (4) times and returns `None` if every try saw a write in progress or a torn record, never spinning; a producer killed mid-write leaves the count odd for good, so every later read of that generation returns `None` at once, which the stage treats as a silent source until `Attach` names the next generation. A stale or absent record is how the stage sees a silent source. |
| Audio | `SampleRing` | audio → host (program, buses, taps of record.md D7) | A single-producer, single-consumer ring sized for 2 s (audio-api's tap length), the same shape as `omuzaki-audio-api`'s ring (whose `Arc`-backed heap storage cannot sit in shared memory), with what recording needs carried alongside the samples: per run, the **stream position** of its first sample, the positions **lost** before it (`gap_before`) and the **`muted` and `private` flags**, exactly `TapRead`'s fields (record.md D7). A muted or private run crosses as its length and flags only; **no sample value of it is written to shared memory**, so record.md's rule that none of its values is copied anywhere holds across the process boundary. The header also carries the generation's **clock mapping** (`ClockMapping`, from the engine clock's latest snapshot: a sample position, its time and the seconds per sample, published as a seqlock like `EngineClock`), with times on the OS monotonic clock that every process reads alike (`omuzaki_shm::host_now()`, nanoseconds), so the host can place any sample on the take's timeline (§10). `push` and `pop` are wait-free and allocate nothing; the audio engine writes its host-bound taps into it instead of a heap ring, and its internal rings are unchanged. |
| Packets | `PacketChannel` | encoder → host | A byte ring of records: a fixed header (stream, `pts`, `dts`, `duration`, keyframe flag, data length, side-data count), then per side datum its kind, id and length, then the bytes. Sized for 1 s at the encode's maximum rate (record.md §3: 64 MiB for ProRes, 8 MiB for VP9 and H.264). A full channel makes the encoder wait; that backs up the frame queue, whose full state is record.md D2's `StreamError::Full`. |

## 5. Heartbeats and hang detection

**Decision.** Each worker's main loop advances one **progress counter** (a wait-free add); the worker's writer thread
sends its value in a coalesced `Beat` every 50 ms (§4.2). The host's reader thread stores it in that worker's
`HeartbeatCell`; the `Monitor`
compares it each tick (§15.4). Crash detection needs no deadline (the control channel ends); a deadline is only for
hangs.

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
driver call (a GPU hang mid-reset) can stay unkillable for seconds. **A process worker is never replaced while it is
alive:** a second instance would contend for the same exclusive devices (a camera, an exclusive-mode audio device, a
hardware encoder session) and the invariant "a spawn only after the old instance ended" (#1024) would no longer
hold. So the `Monitor` waits for `Exited` however long it takes; if `exit_wait` (1.0 s, *budget*) passes first, the
runtime reports `Observation::Stuck`, which changes nothing in the policy but emits `HealthEvent::Stuck` once, so the
health meter and the person see "the renderer is stuck in the driver and cannot be stopped yet" instead of a silent
hold. The hold (§7) continues meanwhile, and the restart follows the exit. How long a Windows `TerminateProcess` of
a worker in a TDR takes is an owner-run question (§13, item 7).

**Threads and tasks are the exception, because they cannot be killed at all.** The thread launcher (tests, and the
interim in-process encoder of record.md D11) and the task launcher (adapters) report a hung instance that does not
end within `exit_wait` as `Observation::Abandoned`: it is counted, its stop flag is set, and the `Monitor` treats it as
ended for spawning. That is safe only because such an instance holds nothing exclusive: its closure or task gets
its own generation's channel ends, which are quarantined (§4.3), and adapters reconnect to network services that
accept a second session. A launcher whose worker holds an exclusive device must be a process.

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
| detect, reap, spawn | 13.1 / 43.4 ms | 50 ms | the control channel's end; the supervisor wakes on the event, not on its tick (§15.4) |
| process start to device | 68 / 127 ms (instance, adapter, device) | 400 ms | DX12 unmeasured: owner run |
| pipelines | 6.5 / 11.4 ms (one pipeline) | 300 ms | the real renderer has many; warm pipeline cache |
| scene snapshot and re-upload | not in the spike (no assets) | 500 ms | measured by the stage worker task (T2) with the kit character |
| first frame drawn and posted | about 47 ms (spawn to first frame 125 ms minus spawn to `Ready` 78 ms) | 100 ms | |
| the host picks it up | at most one period | 33 ms | next tick |
| **total** | **hold 155 / 368 ms; 0.74 s worst of 600 kills** | **1.42 s** | 0.58 s of margin under 2.0 s |

- **A hang adds `hung_after` and the kill's own time.** A hung renderer's last frame is already 0.5 s old when the
  `Monitor` decides, so the hold for a hang is `hung_after` (0.5 s) + the time the kill takes to end the process +
  the crash path (1.42 s). With a kill that ends the process at once (the normal case: milliseconds) that is 1.92 s
  on the budget, at the edge. A kill that does not finish adds its whole duration and has no bound the supervisor
  can promise: at `exit_wait` (1.0 s) the hold is already 2.92 s, and it lasts until the kernel lets the process go
  (`HealthEvent::Stuck`, §5). BR-23's acceptance is `kill -9` (a crash), and the 2.0 s bound is for that case; a
  hang's hold is reported with `slow` set when it passes 2.0 s, never hidden.
- **If the owner run shows DX12's cold start eats the margin,** the fallback is a **warm standby** stage worker (device, pipelines and assets loaded, not rendering), promoted on a crash: it removes device,
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
and outlive the encoder worker. When a new encoder generation answers `Opened` (§4.2), the host creates a fresh
`RemoteEncoder` (§15.5) with a forced keyframe and `pts` continuing from the frame index (the presenter's tick index, §12, which never resets); the
take continues in the same file when the new `extradata` is byte-identical, else it rolls to `_part2`; frames wait in
the queue meanwhile, and `encoder_restarts` counts the restart. The RTMP session sends nothing during the gap (no
packets exist) and its timestamps jump by the gap, which servers accept; it is not disconnected.

**Audio: one timeline across generations.** The take's audio timeline belongs to the host, not to the audio worker:
its position is the take's start on `host_now`'s clock plus samples at the take's rate. Each audio generation's
`SampleRing` carries its stream positions and its clock mapping (§4.3), so the host's aligner places every run on
that timeline exactly as record.md D7 does in one process (`samples_for_frame` per video frame), with the mapping
read from the current generation:
- **within a generation** a run's `gap_before` is a gap as today, and muted or private runs arrive as flags with no
  sample values and are written as silence and listed with their cause (record.md D7), unchanged;
- **across a restart** the new generation's positions start from 0 on a new clock mapping. The aligner ends the old
  generation at the last sample it received, starts the new one at the time its mapping gives for its first sample,
  and fills the stretch between with silence, listed in the take's gaps with the cause `restart` (start, length) so
  an editor can find it; it never splices the two sides together, and no position is counted twice or skipped (a new
  generation starts only after the old one ended, so the stretch is never negative);
- **the take does not roll to a new part** for an audio gap: the silence fills the time, so the WAV sidecars and the
  program track stay aligned with the video. (#1456's draft said "continues in a new part"; this page corrects it,
  §16.)

## 11. Visibility and records

- **The restart log:** every restart appends a `RestartRecord` (when, which worker, why, the attempt, the downtime
  once recovered); the `Monitor` keeps the newest 256 (#1024).
- **The health meter (PF-26)** gets one row per worker: its name, `Health`, restarts in the current window, the last
  reason and downtime, and a red state for `Failed` or a hold past `tell_after`. The rows read `WorkerStatus` (§15.1),
  which is why the observable types sit in `omuzaki-stream-api`: the UI reads them without depending on the
  implementation. The rows themselves are the UI area's task (T9).
- **Ops:** `supervisor.status` (a query: every `WorkerStatus` of both supervisors and the logs' newest records) and
  `supervisor.restart { worker }` (a person only for a `Failed` worker, §6). They are registered in `omuzaki-stream`
  and run in the studio. A worker is addressed by its `WorkerId`, whose `scope` names its supervisor, or by its
  unique name (`stage`, `tracker.webcam`, `encoder.record`, `adapter.twitch`; names are unique across both scopes,
  and going live refuses a duplicate). **Routing:** a `Studio` worker goes to the studio's own `Monitor`; a `Host` worker goes over the local socket
  as `StudioMsg::Restart { worker }` to the host's `Supervisor`. The merged status is the studio's rows followed by
  the host's latest snapshot, each row keeping its scope, so two workers with index 0 never merge.
- **Events:** each `HealthEvent` is published as an op-log event, so the Theatre and automation (a "renderer
  restarted" trigger) see it.
- **Crash reports follow `docs/design/privacy.md`, unchanged:** no minidump is written, stored or sent for any worker
  or for the host (its §4: a stack-only dump still carried a camera-frame-sized stack buffer 32,867 times). Each
  worker binary installs the same small native handler (`crash-handler`) and panic hook as the studio, gated by
  `privacy.crash_reports` (read from the settings the host passes at go live; default `SaveOnly`): under `SaveOnly`
  and `AskToSend` a crash leaves privacy.md's local marker file (version, channel, time, the last panic location) and
  a panic leaves its sanitised report; under `Off`, nothing. The supervisor adds no report of its own: what it knows
  (the worker kind, the reason, the exit code or signal, the attempt) is in the restart log, which stays on the
  machine like the op log. The relaunch prompt of privacy.md §5 (Consent) lists the show's worker markers with the studio's.
  Wiring the handler into `omuzaki-worker` is T11.

## 12. Output pacing

The host's presenter is the output cadence: Spout, NDI, the virtual camera and the encoders' frame feed all tick from
it. #1995 left two known limits in the spike's loop; this design fixes both in the contract.

- **The deadline advances after the present completes.** The spike computed the next deadline at wake-up, before the
  output work, so a present that ran long (Spout's send, a GPU wait) could be followed by another less than a period
  later, and the time spent sending was not counted as missed (Codex's finding on #1995, `cpu_host.rs:114`). Here
  `next_tick` takes `done`, the time *after* every output's `submit` returned, and picks the next deadline from it.
- **The next present starts at least half a period after this one completed.** A present that finished late in its
  period must not be followed by another a moment later (the spike's 1.6 ms "short follower"): a receiver polling at
  the output rate would miss one of the two. So the next deadline is the first grid point that is at least
  `MIN_GAP = period / 2` after `done`; the grid points skipped are counted as missed ticks (shown in
  `OutputStatus.dropped`). A full period of gap was rejected: any present that takes time at all would then skip every
  other grid point and halve the rate. Half a period keeps the normal case exact (the spike's host spends about 3.2 ms
  of a 33.3 ms period on the CPU lane's copy) and costs one tick only for a present that took more than half a period,
  which is itself a fault the health meter shows.
- **The cadence is a grid, not a chain.** The spike's `max(deadline, now) + period` added each wake-up's overshoot to
  the period, so its output ran at about 29.7 fps (median interval 33.5 ms). Here deadlines stay on
  `origin + k × period`. Each present carries its grid index `k` and grid time, never the wall time of the call, so
  receivers and encoders see an exact cadence and `pts` follows `k`.
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
    pub fn min_gap(self) -> u64;           // nanos / 2, rounded up
}
pub const MIN_PERIOD: Duration = Duration::from_millis(1);
pub const MAX_PERIOD: Duration = Duration::from_secs(1);

/// The tick after the one due at grid point `index` (time `deadline`), given that its output work finished at `done`
/// (`host_now` nanoseconds). Pure: no clock read.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct NextTick { pub index: u64, pub deadline: u64, pub missed: u64 }
pub fn next_tick(index: u64, deadline: u64, period: FramePeriod, done: u64) -> NextTick;
// j = max(1, ceil((done + min_gap - deadline) / period))
//   -> { index + j, deadline + j * period, missed j - 1 }
// so the next deadline is on the grid, after `deadline`, and at least min_gap after `done`.
```

**Tests** (in the runtime change), with a 33 ms period (`min_gap` 16.5 ms) and `D` the current deadline:

| `done` | next deadline | `missed` | why |
|---|---|---|---|
| `D + 3` | `D + 33` | 0 | the normal case |
| `D + 16.5` | `D + 33` | 0 | exactly `min_gap` before the next point |
| `D + 17` | `D + 66` | 1 | less than `min_gap` left |
| `D + 32` | `D + 66` | 1 | the review's case: no present 1 ms after the last one |
| `D + 50` | `D + 99` | 2 | overran a whole period |

Plus: **actual presentations**, not only deadlines: a presenter test on a `FakeClock` with scripted present
durations (0 to 2.5 periods, 10,000 ticks, seeded) records each present's start and completion and asserts that every
start is on the grid, every next start is at least `min_gap` after the previous completion, consecutive starts are at
least a period apart, and with durations under `min_gap` there is no missed tick and the rate is exactly
`num / den`; the fake output host of §14 presents through the same loop with the real clock. `from_rate` rejects
`(0, 1)`, `(1, 0)`, `(2000, 1)` and `(1, 2)` and accepts `(30000, 1001)`.

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
7. For this design, beyond the spike: `taskkill /F` of a worker inside the host's job leaves no orphan and ends its
   control pipe; killing the host itself ends every worker; a `Local\` file mapping is reachable from the worker and
   not from another user's session; and how long a `TerminateProcess` of a worker in a GPU hang (a TDR) takes to
   complete (`exit_wait`, §5).
8. The host's own device (§2): a TDR forced while the host presents (`dxcap -forcetdr` or a timeout shader in the
   worker): does the host see `DXGI_ERROR_DEVICE_REMOVED` and recover in place, does Spout reconnect under the same
   name, and do the CPU-fed outputs keep presenting meanwhile.

Items 3, 4, 6, 7 and 8 are not yet in the spike's binary; adding them is a task on the spike (T13).

## 14. Tests and the `kill -9` plan

- **The policy table** (#1024): pure, deterministic, `now` as a parameter; the cases of #1024's acceptance, renamed for
  `Down` and `Up` (§16), plus: `Stuck` changes nothing and no `Spawn` comes before `Exited` however late it is;
  `Abandoned` (thread and task specs only) acts as `Exited` for spawning; `LaunchFailed` consumes an attempt and
  backs off, five in a row make the worker `Failed`, and `LaunchFailed { permanent: true }` makes it `Failed` at
  once; a worker's `slow_after` sets `slow`; an id from another scope or out of range is `Err(UnknownWorker)` from
  every method, never a panic.
- **Fake workers** (#1025): the `fake-worker` binary speaks the §4.2 protocol: `--beat-ms N` (default 33),
  `--startup-ms N`, `--crash-after-ms N` (exit code 3), `--hang-after-ms N` (stops beating and stays alive),
  `--spam-stdout` (prints megabytes without newlines and garbage to its stdout: the log limits hold and the protocol
  is untouched), `--bad-frame` (sends a malformed control frame: restarted with `Protocol`), `--role encoder` (#1224).
  A kill that does not finish cannot be produced on a real child (SIGKILL ends even a stopped process), so `Stuck` is
  tested with a fake `WorkerHandle` whose kill takes effect only after a set delay: no replacement before it ends,
  one `Stuck` event, and `stop` returns within `stop_wait` + `exit_wait` naming it. Thread quarantine is tested with
  a hung `ThreadLauncher` closure that keeps writing frames and messages after it is `Abandoned`: none reaches the
  host's presenter, the replacement's slots or the log. Latched state is tested by sending `Scene` while the stage is
  down: the new generation's first frame reflects it. A fake output host thread presents a frame counter at 30 Hz
  through the real `next_tick` and the real hold.
- **Orphans:** the test runs a `Supervisor` inside a small fake host process of its own, `SIGKILL`s that process with
  three fake workers running (one of them spawned in the instant before the kill), and checks that no `fake-worker`
  pid survives it by more than 1 s.
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
/// Which supervisor runs a worker: the output host's (processes) or the studio's (adapter tasks). §2 has two.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum Scope { Host, Studio }

/// A supervised worker: its supervisor and its index in that supervisor's spec list. Unique across the show, so the
/// merged status (§11) and `supervisor.restart` never confuse the host's worker 0 with the studio's. The fields are
/// private: an id is made by a `Monitor`, and one that arrives from outside (deserialized from the local socket or an
/// op) is checked by every method that takes it, which returns `UnknownWorker` for a wrong scope or index.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct WorkerId { scope: Scope, index: u16 }
impl WorkerId { pub fn scope(self) -> Scope; pub fn index(self) -> u16; }

/// An id that no worker of this supervisor has.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("no worker {id:?} in this supervisor")]
pub struct UnknownWorker { pub id: WorkerId }

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
    Protocol { what: String },            // a malformed control frame (§4.2): killed and restarted
    LaunchFailed { what: String },        // the spawn or the handshake failed (§4.2)
}

#[non_exhaustive]
pub enum HealthEvent {
    Restarted { worker: WorkerId, reason: RestartReason, attempt: u32 },
    Recovered { worker: WorkerId, downtime: Seconds, slow: bool },
    GaveUp { worker: WorkerId, restarts: u32 },
    Stuck { worker: WorkerId },           // a process kill that has not finished after exit_wait (§5)
    Abandoned { worker: WorkerId },       // a hung thread or task given up after exit_wait (§5)
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

**Every `Seconds` in these types is on the show clock:** seconds since the show went live, read from the OS
monotonic clock that every process of the show reads alike (`omuzaki_shm::host_now`, §15.3). `omuzaki-stream`'s
`ShowClock { origin_ns }` implements `Clock` over it; the host makes it at go live and sends `origin_ns` to the studio
in its first `ShowMsg`, and the studio's own `Monitor` (adapters) runs on a `ShowClock` with the same origin. So
`Backoff { until }`, `RestartRecord::at` and the downtimes mean the same instant in the host and in the studio, and
the health meter can show "retrying in 0.4 s" from either.

### 15.2 The worker protocol: `omuzaki-stream-api::worker` (new module)

```rust
pub const PROTOCOL_VERSION: u32 = 1;
pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

#[non_exhaustive]
pub enum WorkerMsg {                      // worker → host, on the control channel (the worker's stdin, §4.2)
    Hello { kind: WorkerKind, version: u32, pid: u32, generation: u32 },
    Beat { progress: u64, work: u64 },
    Opened { spec: StreamSpec },          // an encoder's reply to Start: its stream, extradata included (§10)
    Ready { device_ms: f32, pipelines_ms: f32, upload_ms: f32 },   // the §8 split, measured on every start
    Frame { slot: u32, index: u64 },      // stage: a frame complete in slot (FrameSlots or SharedTextures)
    SlotFree { slot: u32 },               // encoder: done reading the slot the host announced
    Gone { reason: String },              // a clean, explained exit (a config error): the Monitor still counts it
}

#[non_exhaustive]
pub enum HostMsg {                        // host → worker, on the same channel
    Start { channels: ChannelNames, params: StartParams },
    Frame { slot: u32, index: u64, pts: i64 },   // encoder: a frame to encode is in slot; pts from the tick index (§12)
    SlotFree { slot: u32 },               // stage: the host is done with the slot
    Keyframe,                             // encoder: the next frame is a keyframe
    Scene { rev: u64 },                   // stage: reload the snapshot of that revision (§3, §8)
    Attach { role: ChannelRole, channel: ChannelRef, generation: u32 },  // stage: a peer's new generation (§4.3)
    SourceDown { source: WorkerId },      // stage: §9
    SourceUp { source: WorkerId },
    Stop,                                 // encoder: flush, send the last packets, exit 0; others: exit 0
}

/// What a worker of each kind needs to start, beyond its channels.
#[non_exhaustive]
pub enum StartParams {
    Stage { scene_rev: u64, size: UVec2, fps: (u32, u32) },
    Tracker { source: String },           // the camera's stable id and the tracker config name
    Audio,
    Encoder { config: EncodeConfig, first_index: u64 },   // first_index continues the take's frame index (§10)
}

/// The shared objects of one worker generation (§4.3).
pub struct ChannelNames { pub generation: u32, pub entries: Vec<(ChannelRole, ChannelRef)> }
/// How one object travels: on Unix as a descriptor sent with the same message (`SCM_RIGHTS`; the number is its
/// position in that message's descriptor list), on Windows as the section's random name.
#[non_exhaustive]
pub enum ChannelRef { Fd { position: u16 }, Name { name: String } }
#[non_exhaustive]
pub enum ChannelRole { Frames, Preview, Tracker, VoiceCues, LiveInputs, Audio, Packets }

/// Length-prefixed postcard framing over any byte stream.
pub fn write_msg<W: Write, M: Serialize>(w: &mut W, msg: &M) -> Result<(), StreamError>;
pub fn read_msg<R: Read, M: DeserializeOwned>(r: &mut R, buf: &mut Vec<u8>) -> Result<Option<M>, StreamError>; // None at end of stream
```

### 15.3 The channels: `omuzaki-shm` (new lib crate)

```rust
pub struct ShmError { /* what, os error */ }

/// One end of a shared object, as it is handed to another process: the descriptor on Unix (sent over the control
/// socketpair), the section name on Windows (§4.3). Made by `create`, consumed by `open`.
pub enum Share { #[cfg(unix)] Fd(OwnedFd), #[cfg(windows)] Name(String) }

/// The OS monotonic clock in nanoseconds, the same in every process of the show (CLOCK_MONOTONIC,
/// QueryPerformanceCounter, mach_continuous_time). The only time base that crosses a process boundary.
pub fn host_now() -> u64;

/// Frame slots with ownership hand-back (§4.3). One set per worker generation.
pub struct FrameSlots { /* header + n slots of `slot_bytes` */ }
impl FrameSlots {
    pub fn create(slots: u32, slot_bytes: usize) -> Result<(Self, Share), ShmError>;  // the host; `Share` goes to the other end
    pub fn open(share: Share) -> Result<Self, ShmError>;                                  // the other end
    pub fn write(&self, slot: u32, bytes: &[u8]) -> Result<(), ShmError>;              // producer, owned slots only
    pub fn read_into(&self, slot: u32, out: &mut [u8]) -> Result<(), ShmError>;       // consumer, announced slots only
}

/// Newest-wins record (seqlock). Reads are bounded (§4.3).
pub struct Latest { /* header (seq) + one record of at most max_bytes */ }
pub const LATEST_READ_TRIES: u32 = 4;
impl Latest {
    pub fn create(max_bytes: usize) -> Result<(Self, Share), ShmError>;
    pub fn open(share: Share) -> Result<Self, ShmError>;
    pub fn publish(&self, bytes: &[u8]) -> Result<u64, ShmError>;        // returns the new seq
    /// `(seq, len)` of a whole record, or `None` when every one of LATEST_READ_TRIES tries met a write in progress
    /// or a torn record (a producer killed mid-write leaves its generation's record unreadable for good). Never spins.
    pub fn read(&self, out: &mut [u8]) -> Option<(u64, usize)>;
}

/// Where a generation's audio sits in time: sample `sample` was at `at_ns` on `host_now`'s clock.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct ClockMapping { pub sample: u64, pub at_ns: u64, pub seconds_per_sample: f64 }

/// What a pop returned besides the samples: `TapRead`'s fields (omuzaki-audio-api), across processes.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RunInfo { pub len: usize, pub start_sample: u64, pub gap_before: u64, pub muted: bool, pub private: bool }

/// SPSC ring of interleaved f32 frames with per-run positions and flags (§4.3).
pub struct SampleRing { /* header (head, tail, channels, clock seqlock) + per-slot meta + samples */ }
impl SampleRing {
    pub fn create(channels: u16, frames: usize) -> Result<(Self, Share), ShmError>;
    pub fn open(share: Share) -> Result<Self, ShmError>;
    /// Producer: a run of samples at `start_sample`. Wait-free, no allocation; returns the frames written.
    pub fn push(&self, start_sample: u64, interleaved: &[f32]) -> usize;
    /// Producer: a muted or private run of `frames` frames: positions and flags only, no sample values.
    pub fn push_withheld(&self, start_sample: u64, frames: usize, muted: bool, private: bool) -> usize;
    /// Consumer: one contiguous run into `out`; withheld runs come back as zeros with their flags set.
    pub fn pop(&self, out: &mut [f32]) -> RunInfo;
    pub fn publish_clock(&self, mapping: ClockMapping);                 // producer, at most once per block
    pub fn clock(&self) -> Option<ClockMapping>;                        // consumer; bounded like Latest::read
}

/// Byte ring of packet records (§4.3).
pub struct PacketChannel { /* header + bytes */ }
impl PacketChannel {
    pub fn create(capacity: usize) -> Result<(Self, Share), ShmError>;
    pub fn open(share: Share) -> Result<Self, ShmError>;
    pub fn send(&self, packet: &Packet) -> Result<bool, ShmError>;       // false when full: the caller waits
    pub fn recv(&self, out: &mut Vec<Packet>) -> Result<usize, ShmError>;
}
```

`omuzaki-shm` is a lib crate (`kind = "Lib"`, `layer = "lib"`, ADR 0011) in the Broadcast and live area: it depends
on `omuzaki-stream-api` (for `Packet`) and external crates only, holds no global state and registers nothing, so
`omuzaki-stream`, the output host and the workers may all depend on it. Its tests:
- every type round-trips across two real processes;
- **a producer killed mid-write** (a child process `SIGKILL`ed between the two halves of a write, by a test hook)
  never yields a torn record: `FrameSlots` never announces the slot, `Latest::read` and `SampleRing::clock` return
  `None` within `LATEST_READ_TRIES` tries (timed: under 1 ms) and keep returning it, and a new generation's objects
  work at once;
- a withheld run puts no sample value in the mapping (the test scans the whole mapped region for a marker it fed
  as muted audio, as the crash canary of `privacy.md` §4 scans a dump);
- loom or Miri on the ring indices and the seqlock.

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

pub enum Observation {
    Spawned,
    Beat(u64),
    Exited { code: Option<i32> },
    Stuck,                              // a process kill not finished after exit_wait: an event, no state change
    Abandoned,                          // thread and task launchers only: treated as ended for spawning
    LaunchFailed { what: String, permanent: bool },   // counts as an attempt; permanent goes to Failed at once
}
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
    pub fn new(scope: Scope, specs: Vec<WorkerSpec>, now: Seconds) -> Self;
    pub fn ids(&self) -> impl Iterator<Item = WorkerId> + '_;     // the only source of valid ids
    pub fn observe(&mut self, now: Seconds, id: WorkerId, obs: Observation, out: &mut Vec<Action>)
        -> Result<(), UnknownWorker>;                               // `out` cleared first
    pub fn tick(&mut self, now: Seconds, out: &mut Vec<Action>);   // deadlines, due spawns
    pub fn request_restart(&mut self, now: Seconds, id: WorkerId, out: &mut Vec<Action>)
        -> Result<(), UnknownWorker>;                               // also revives Failed
    pub fn health(&self, id: WorkerId) -> Result<Health, UnknownWorker>;
    pub fn status(&self, id: WorkerId) -> Result<WorkerStatus, UnknownWorker>;
    pub fn log(&self) -> &RestartLog;
}

pub struct ExitInfo { pub code: Option<i32> }
pub trait WorkerLauncher: Send + Sync {
    /// Starts one generation: the handle sends `Start { channels, params }` after `Hello` and checks its generation.
    fn spawn(&self, id: WorkerId, channels: &ChannelNames, params: &StartParams) -> Result<Box<dyn WorkerHandle>, LaunchError>;
}
pub trait WorkerHandle: Send {
    fn heartbeat(&self) -> &HeartbeatCell;            // set by the handle's reader from `Beat`
    fn try_wait(&mut self) -> Option<ExitInfo>;       // Some once the worker has ended (crash, kill or exit)
    fn kill(&mut self);                               // SIGKILL / TerminateProcess; never waits
    fn send(&mut self, msg: &HostMsg) -> Result<(), LaunchError>;
    fn pid(&self) -> Option<u32>;
    fn revocable(&self) -> bool;                      // true only for thread and task handles (§5)
}
pub struct WorkerCommand { pub program: PathBuf, pub args: Vec<OsString>, pub env: Vec<(OsString, OsString)> }
pub struct ProcessLauncher { /* a WorkerCommand; env applied on every OS (#1995's Windows finding); the duplex
                                control channel as stdin, stdout and stderr to the capped log reader (§4.2); the
                                host's pid in the args for the worker's getppid recheck (§2) */ }
pub struct ThreadLauncher { /* a closure per worker; a panic is a crash; a hung thread is abandoned and counted */ }
pub struct TaskLauncher { /* an async fn per adapter on the studio's runtime; a panic is a crash; kill aborts */ }
#[non_exhaustive] pub enum LaunchError { Spawn { what: String }, Protocol { what: String }, Closed, Permanent { what: String } }
#[non_exhaustive] pub enum SendError { Unknown(UnknownWorker), Down }

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
    /// State messages (`Scene`, `SourceDown`, `SourceUp`, `Attach`) are latched per worker and replayed after
    /// `Start` on every spawn, and Ok even while the worker is down; commands (`Frame`, `SlotFree`, `Keyframe`) to a
    /// worker that is down are refused with `SendError::Down` (§4.2).
    pub fn send(&self, id: WorkerId, msg: HostMsg) -> Result<(), SendError>;
    pub fn restart(&self, id: WorkerId) -> Result<(), UnknownWorker>;   // a person's restart (request_restart)
    /// `Stop` to every worker, kill after `stop_wait`, then wait at most `exit_wait` more; a process still alive after
    /// that is named in the report, its handles are closed and it is left to the job object or the parent-death
    /// signal, which end it as the host exits. Never blocks longer than `stop_wait + exit_wait`.
    pub fn stop(self) -> StopReport;
}
pub struct StopReport { pub stuck: Vec<WorkerId> }
pub struct SupervisorConfig { pub tick: Duration, pub exit_wait: Duration, pub stop_wait: Duration }
// defaults: tick 50 ms, exit_wait 1 s, stop_wait 2 s
```

**The rules** are #1024's, exact, with these changes:
- `Down` and `Up` replace `HoldOutputs` and `ReleaseOutputs` and are emitted for every kind (the `Handoff` decides
  what they mean; `WorkerSpec.hold_outputs` is gone).
- `Recovered`'s `slow` is `downtime > slow_after` instead of a fixed 2.0 s.
- `Stuck` changes no state and emits one `HealthEvent::Stuck`; a `Spawn` still waits for `Exited`, so a process
  worker never has two instances.
- `Abandoned` (sent only for a revocable handle: thread or task) counts as `Exited` for spawning and emits one
  `HealthEvent::Abandoned`.
- `LaunchFailed` is an attempt: it is logged with `RestartReason::LaunchFailed`, counts against `max_restarts`
  within `within`, and the next `Spawn` waits for that attempt's backoff; `permanent` makes the worker `Failed` with
  one `GaveUp`.
- `WorkerId` is `{ scope, index }` (`Monitor::new` takes the scope), so the studio's and the host's monitors never
  issue the same id, and every method taking an id checks it.

`observe` and `tick` allocate nothing once `out` has capacity (the `String` of `LaunchFailed` is moved into the log,
which happens only on a failed launch, not on the hot path); `beat` is one relaxed atomic add.

**The runtime** maps actions onto launchers and the `Handoff`; one reader thread per child parses `WorkerMsg` into
the handle's `HeartbeatCell` and forwards `Frame` and `Ready` to the host's frame intake. The supervisor thread waits
on a channel the readers signal at end of stream, with `tick` as the timeout, so a crash is observed at once (§8's
50 ms detect budget), not at the next tick. `stop` leaves no zombie among the children that died: each is waited;
a stuck one is reported and left to the OS (above).

### 15.5 The remote encoder (#1224): `omuzaki-stream::encode_remote`

```rust
pub struct RemoteEncoder { /* producer end of a FrameSlots queue; consumer end of a PacketChannel; the worker's id */ }
impl RemoteEncoder {
    pub fn new(sup: &Supervisor, worker: WorkerId, frames: FrameSlots, packets: PacketChannel, spec: StreamSpec) -> Self;
}
impl Encoder for RemoteEncoder {
    // send: copy into a slot the worker has handed back and announce it (HostMsg::Frame { slot, index, pts });
    //       a slot returns with WorkerMsg::SlotFree; StreamError::Full when none is free (record.md D2).
    // receive: drain the PacketChannel, side data included, metered by data.len() plus every side datum.
    // request_keyframe: HostMsg::Keyframe.
    // spec: the StreamSpec of the worker's Opened (§4.2's handshake), which the host passes to new; it is compared
    //       byte for byte with the previous generation's extradata to continue the take or roll a part (§10).
    // A dead or hung worker surfaces as StreamError::Encode; the host makes a fresh RemoteEncoder, with fresh
    // FrameSlots and PacketChannel, for the next generation (§4.3, §10).
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
- **#1024, the watchdog policy:** confirmed, with the §15.4 changes (`Down` and `Up`, `slow_after`, `Stuck`,
  `Abandoned` for threads and tasks only, `LaunchFailed`, `status`, the scoped and checked `WorkerId`, the show
  clock), and its observable types placed in `omuzaki-stream-api::supervisor` (§15.1). Its acceptance cases
  stand with the renames; cases are added for `Stuck`, `Abandoned`, `LaunchFailed`, an unknown id and a per-kind
  `slow_after` (§14).
- **#1025, the supervisor runtime:** confirmed, with the §4.2 protocol in place of one byte per beat, `Handoff` in
  place of `OutputHold`, `spawn` taking one generation's `ChannelNames` and `StartParams` (fresh shared objects per
  generation, §4.3), `status` (rows of `WorkerStatus`) in place of `health`, `start` taking a `SupervisorConfig` and
  returning a `Result`, event-driven wake-up, `exit_wait`, latched `send`, a bounded `stop` and `TaskLauncher`. Its
  acceptance stands; cases are added for a kill that does not finish (`Stuck`), a supervised task's panic and hang,
  the log limits, a malformed frame, latched state and orphans after the host's own death (§14).
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
| T11 | Crash markers and sanitised panic reports in `omuzaki-worker` and the host, as `privacy.md` sets out (§11); no minidumps | Broadcast and live | T1, `omuzaki-crash` |
| T12 | The studio reattaching to a running host (§2) | Broadcast and live | T1 |
| T13 | The spike's missing Windows measurements (§13 items 3, 4, 6, 7, 8) | Broadcast and live | none |

## 17. Open questions

- **DX12's cold start** decides whether §8's budget holds on Windows: the owner run (§13). If it does not, the warm
  standby of §8 is the plan, and its GPU memory cost is measured first.
- **Degrading after the budget:** is a "safe renderer" wanted after `Failed`, and what is it (§6)? Waits for data on
  what crashes a DX12 renderer.
- **GPU frame channels on macOS and Linux** (IOSurface, dma-buf): not designed; the CPU lane (3.2 ms per 1080p frame
  in the host, *spike*) is the route until they are.
- **Does the Windows host need its own wgpu device,** or only a D3D12 or D3D11On12 device for the hold texture and
  Spout (#1995's open question)? T1 decides it from the owner run.
- **A GPU hang that `TerminateProcess` cannot finish:** how long does it last, and is `exit_wait` of 1.0 s the right
  moment to tell the person (§5, §13 item 7)? The design waits rather than run two instances.
- **The host's own GPU device on Windows** (§2): if a host-side driver crash proves real, the GPU presentation moves
  into a presenter worker; the owner run (§13 item 8) and the field decide.
- **Edits while live:** the cost of a large edit's reload in the stage worker (§3) is measured by T2; if it is too
  slow, a delta protocol replaces the revision reload.
- **The decision record and the spec:** #1023's fence includes an ADR and the §8 threads note. This change's file list
  (#2068) is this page only, so the ADR (the process model of §2 and the editor rule of §3) and the §8 amendment
  ("render on the main thread" becomes "in the stage worker while the outputs are on") follow when the design is
  accepted.
- **The Chromium and OBS sources** were read through search summaries only in #1995; the prior-art notes here rely on
  the same summaries and say no more than they do.
