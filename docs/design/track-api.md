---
title: "`omuzaki-track-api`: the tracker contract"
summary: "the tracker contract (PF-01): the camera seam, the `Tracker` trait, threads and real-time rules, clock mapping, the newest-wins mailbox, lost tracking, budgets, the permission seam and the unlocked tracker tasks."
area: design
---

# `omuzaki-track-api`: the tracker contract

**Status:** proposed (2026-10-14). **Crates:** `crates/omuzaki-track-api` (API), `crates/omuzaki-track-mock` (Mock).
**Feature:** PF-01. **Spec:** v3 §8 (threads), §10 (`track.face`, `latency.*`, `cpu.total`), §22 (PF-01, PF-26,
PF-28). **Builds on:** `docs/design/anim-api.md` §12 (where this crate sits) and §13 (allocation tests),
`omuzaki-track` (the face tracker that goes behind it), `omuzaki-pixfmt` (frame conversion), `spikes/webcam-track`
(timings), `docs/design/settings.md` (devices are person-only). **No ADR:** anim-api §12 already placed the crate;
nothing here is an architecture call.

## 1. What this contract is for

Between "a camera has a frame" and "the pipeline polls a `TrackFrame`" there are four things every tracker needs, and
each would be written differently, and wrongly, if each tracker wrote its own:

1. **A camera seam.** The tracker port (T2) must not know whether frames come from Media Foundation, AVFoundation,
   V4L2 or a test. `Camera` and `CameraStream` are that seam, and `CameraFrame` is what crosses it.
2. **A device trait.** The stage and the ops start, stop and meter a tracker without knowing what it is: `Tracker`.
3. **Clock mapping.** Frames are stamped on the camera's clock; the pipeline runs on the stage clock. `ClockMap`.
4. **The hand-off.** Spec §8: "lock-free hand-off (newest frame wins, never queue)". The mailbox.

It stays small on purpose. `omuzaki-anim-api` already owns `TrackFrame`, `FrameSource`, `DeviceProfile`,
`TrackStatus`, `CalibrationPrompt`, `Calibration` and `Calibrator`; this crate depends on it and defines no twins.
Calibration is the retarget's job and is not repeated here. Network protocols (PF-02 to PF-04) are also `Tracker`s,
but their parsing lives in their own crates; this contract gives them the mailbox and the clock map.

## 2. The API

Everything public, one line each (the PR summary repeats this list).

| Item | What it is |
|---|---|
| `CameraId` | the backend's name for a camera; person settings only, never a project (it can name the machine) |
| `PixelFormat` | `Nv12`, `Yuy2`, `Mjpg`, `Rgb8`, `Bgra8`, with `tight_stride` and `frame_len` |
| `CameraFormat` | width, height, nominal fps, pixel format: one mode of a camera |
| `CameraInfo` | id, OS name and the formats of one camera |
| `CameraFormatRequest` | the wanted mode; `pick` chooses the closest one the same way on every backend |
| `DeviceTime` | nanoseconds on the camera's own clock |
| `CameraFrame<'a>` | borrowed bytes, width, height, stride, pixel format, capture time, device sequence number |
| `Camera` | `enumerate`, `open(id, request) -> Box<dyn CameraStream>` |
| `CameraStream` | `format`, `next_frame(timeout)`: the newest frame, borrowed until the next call |
| `QualityTier` | `Full`, `Balanced`, `Light` (PF-01 tiers) |
| `TrackerConfig` | camera request, tier, `lost_after` |
| `LatencyStats` | frames, dropped, p50, p95, max of capture-to-publish |
| `Tracker` | `devices`, `start(device, config) -> Box<dyn FrameSource>`, `stop`, `latency` |
| `ClockMap` | `new`, `reset`, `map(captured, arrived) -> Seconds` |
| `ClockError` | `Backwards { last, got }` |
| `CLOCK_WINDOW`, `MAX_DRIFT`, `RESYNC_GAP` | the clock map's 64 pairs, 1000 ppm and 1 s |
| `mailbox()` | a connected `MailboxWriter` and `MailboxReader` |
| `MailboxWriter` | `publish(&TrackFrame)`: the newest wins |
| `MailboxReader` | implements `FrameSource`: `poll` returns the newest unread frame or `None` |
| `TrackError` | `NoDevice`, `Busy`, `FormatUnsupported`, `PermissionDenied`, `Lost`, `Backend` |

Mock: `FakeCamera` (`from_frames`, `synthetic`, `synthetic_luma`, `id`, `ID`) and `FakeTracker` (`scripted`).

## 3. Threads

```text
 tracker thread (one per running Tracker)                                     stage thread
 CameraStream::next_frame ─▶ convert ─▶ infer ─▶ ClockMap::map ─▶ publish ┄┄▶ poll ─▶ Retarget ─▶ … ─▶ render
 (blocks on the device)      (pixfmt)   (track)                     (wait-free)  (wait-free, once per frame)
```

- **The tracker owns one thread** from `start` to `stop`: capture, conversion and inference run in sequence on it.
  The spike measured one thread as enough at 30 fps and found that more inference threads cost more CPU than they
  save when paced (`spikes/webcam-track/README.md`, "one thread is enough"). A separate capture thread would add a
  second hand-off and a second copy of the frame for nothing: the camera driver already buffers.
- **Inference stays on that thread**, as `omuzaki-ai-api` sessions do: no spinning workers (ONNX Runtime one
  intra-op thread, the spike's recommendation 3).
- **The stage thread only polls**, once per render frame. `poll` is the only tracker call on the stage's hot path.
- **The UI and ops** call `Tracker::start`, `stop`, `devices` and `latency` from wherever they run; these are cold
  except `latency`, which is a few relaxed loads.

The newest frame wins twice. At capture, `next_frame` returns the newest frame the device has, and skipped frames
show as gaps in `CameraFrame::seq` (counted as `LatencyStats::dropped`): if inference is slower than the camera, the
tracker tracks the latest image instead of falling behind. At hand-off, the mailbox overwrites an unread frame.

## 4. Real-time rules

Hot path means: runs every frame, may not allocate, lock or block, and is tested to allocate nothing.

| Call | Thread | Rule |
|---|---|---|
| `MailboxReader::poll` | stage | hot: wait-free, one atomic load when empty, one swap and 71 relaxed loads when not |
| `MailboxWriter::publish` | tracker | hot: wait-free, 71 relaxed stores and one swap |
| `ClockMap::map` | tracker | hot: fixed arrays, O(`CLOCK_WINDOW`) arithmetic |
| `CameraStream::next_frame` | tracker | may block up to `timeout`, may not allocate per frame: frames are borrowed from the stream's own buffers |
| `Camera::open`, `Tracker::start` | any | cold: allocate, open devices, load models |

`omuzaki-track-api/tests/hot_path.rs` proves the first three with a counting allocator (10,000 rounds, zero
allocations), the anim-api pattern. `FakeCamera`'s stream draws synthetic frames into one buffer allocated at `open`.

## 5. Camera frames

The spike read PNGs, so it fixed nothing about capture; what `CameraFrame` carries is what `omuzaki-pixfmt` needs to
convert a frame and what the tracker needs to time it:

- **bytes, width, height, stride and `PixelFormat`.** The five layouts are what UVC, Media Foundation, AVFoundation
  and V4L2 webcams deliver, and exactly what `omuzaki-pixfmt` converts. NV12 is one buffer: the Y plane, then the UV
  plane, at one stride, as Media Foundation and V4L2 hand it over. MJPG has stride 0 and carries the compressed
  image.
- **`captured: DeviceTime`**, the sensor timestamp, and **`seq`**, the device's frame counter. Arrival time is not
  in the frame: the tracker reads the stage clock when the frame arrives and gives both to `ClockMap`.
- **Borrowed, not owned.** `next_frame(&mut self)` returns `CameraFrame<'_>`, so the borrow checker ends the frame
  before the next call and the backend reuses its buffer. A backend whose OS hands out pooled buffers (Media
  Foundation samples, V4L2 mmap buffers) lends them directly and returns them on the next call.

Format choice is in the contract (`CameraFormatRequest::pick`) rather than in each backend so one setting opens the
same mode on every OS: nearest pixel count, then nearest frame rate, then the cheaper conversion (RGB, BGRA, NV12,
YUY2, MJPG), then the backend's order. The default request is 1280 x 720 at 60 fps (PF-01: "60 fps when the camera
allows").

## 6. Clock mapping

A webcam timestamps frames on its own clock; arrivals on the stage clock carry driver and USB jitter of a few
milliseconds. The pipeline wants capture times on the stage clock with the jitter removed, so the retarget's
filters and the A/V playout delay (anim-api `PlayoutDelay`) see the camera's real spacing.

`ClockMap::map(captured, arrived)` keeps the last `CLOCK_WINDOW` (64) pairs, about 2 s at 30 fps:

1. **Drift:** a least-squares fit of arrival against device time gives the rate, clamped to `1 ± MAX_DRIFT`
   (1000 ppm, several times what real crystals do, so a bad fit cannot run away). With fewer than 8 pairs the rate
   is 1.
2. **Offset:** the line goes through the lowest arrival in the window (the frame that waited least), not the mean,
   because transport delay is never negative: the lower envelope is the best estimate of "capture plus the
   smallest delay". That smallest delay is constant and cannot be seen from timestamps; the tracker's measured
   latency (`LatencyStats`) reports it.
3. **Monotonic output:** a mapped time never goes below the previous one, so a refit cannot make time step back.

The rules for bad timestamps:

- **Backwards** (a device time before the newest accepted one): rejected with `ClockError::Backwards`; the tracker
  drops the frame and the map is unchanged. Cameras do not reorder frames, so this is a driver fault or a device
  reset, and clamping it would give a frame a time that is a lie.
- **A forward jump over `RESYNC_GAP` (1 s)**: the fit restarts from that frame (a paused stream, or a clock reset
  forward). Output stays monotonic.
- **A reopened stream** calls `reset`, because a new session can have a new device epoch.

Tests (`clock.rs`): a fixed offset maps exactly; ±200 ppm drift with 0 to 5 ms jitter over 60 s stays within 1 ms of
the truth after the first window and stays monotonic (without the fit, 200 ppm is 12 ms off after a minute); a
backwards jump is rejected and the next frame maps normally; a one-hour forward jump resyncs.

## 7. The mailbox

A triple buffer: three slots, one owned by the writer, one by the reader, one in the middle, and one `AtomicU8`
naming the middle slot plus a "fresh" bit. `publish` fills the writer's slot and swaps it into the middle (taking
back whichever slot was there); `poll` returns `None` if the fresh bit is clear, else swaps its own slot into the
middle and reads the one it got. Each side does one atomic swap, so both are wait-free, and since a slot is only
touched by its current owner a frame can't be torn. An unread frame is overwritten: there is no queue to grow.

- **Why not a seqlock:** its reader retries while the writer writes, so `poll` is only lock-free, not wait-free,
  and a reader on a busy machine can spin. The triple buffer never retries.
- **Why not a `Mutex<Option<TrackFrame>>`:** a lock on the stage thread is exactly what §8 forbids; a tracker thread
  preempted while holding it would stall a render frame.
- **Why atomic words and no `unsafe`:** the slots are `[AtomicU32; 71]`, the frame encoded field by field (the
  `omuzaki-audio-api` ring does the same). The swap's acquire/release ordering publishes the words, and their own
  accesses are relaxed. That keeps the workspace's `unsafe_code = "deny"` intact in an API crate; the cost is
  encoding and decoding about 300 bytes per frame, a copy the frame needed anyway. The encoding covers every
  `TrackFrame` field and destructures the frame exhaustively, so a field added in anim-api fails to compile here
  until it is carried; `DeviceProfile` is non-exhaustive, so a test walks its JSON schema and fails when a variant
  is added without an encoding.
- **One producer, one consumer** by construction: neither end is `Clone`.

Tests: newest-wins and take-once (`mailbox.rs`); no allocation in `poll` or `publish` (`tests/hot_path.rs`); a
producer thread publishing 1e6 frames against a spinning consumer, checking that every frame seen is whole and newer
than the last (`tests/mailbox_stress.rs`; it fails within a few hundred frames if the reader reads the middle slot
without taking it). loom was not added: it would be a new dependency, and the issue accepts the stress test.

## 8. Lost tracking

The signal is anim-api's own: a `TrackFrame` with `TrackStatus::Lost` (`TrackFrame::lost`). The retarget eases to
rest and hands over to the life layer after its grace period (PF-23), so the tracker only has to say it promptly:

- **No face in a frame:** that frame is published as `Lost` (same `seq`, its capture time).
- **No frames at all** (camera stalled, inference stuck) for `TrackerConfig::lost_after` (200 ms by default, six
  frames at 30 fps): the tracker publishes **one** `Lost` frame, with the last frame's `seq`, since no new camera
  frame exists. Without it the mailbox would hold the last face for ever and the avatar would freeze mid-expression.
- **The camera is gone:** `next_frame` returns `TrackError::Lost`; the tracker publishes one `Lost` frame and its
  status becomes an error the UI shows (PF-26). Recovery is a new `start`.

`FakeTracker` implements the stall rule, so pipeline tests get the real signal with no threads.

## 9. Budgets

Spec §10 rows this contract touches, all on tier `R`:

| Key | Budget | Where it is spent |
|---|---|---|
| `track.face` | ≤ 5 ms p95, camera frame in to channels out | convert + infer + `map` + `publish` on the tracker thread |
| `latency.laptop` | ≤ 80 ms p95 camera to photon (`latency.desktop`: ≤ 50 ms on `D`) | the chain below |
| `cpu.total` | ≤ 3 % of 8 cores, tracker included | one tracker thread, no spinning workers |

The issue's brief says 100 ms glass-to-glass; the spec's rows are tighter, and the spec wins. Camera to photon at
60 fps, as an estimate (nothing is measured on `R` yet):

| Step | Estimate | Source |
|---|---|---|
| exposure, readout, USB, driver | about 17 to 25 ms | one frame at 60 fps plus transfer |
| convert to RGB | ≤ 2.5 ms (NV12, YUY2), ≤ 8 ms (MJPG) at 720p | `omuzaki-pixfmt` bench budgets |
| inference | about 6 ms (quiet) to 9 ms (loaded) on `Dev`; about 1.4x that per core on `R` | spike: crop 1.55, landmarks 5.9, blendshapes 0.82, pose 0.02 ms; +1.4 ms when the detector runs |
| map + publish + poll | under 0.01 ms | this crate |
| wait for the next render frame | 0 to 17 ms | 60 Hz render |
| render, present, display | about 17 to 33 ms | one to two refreshes |

That sums to roughly 60 to 95 ms, so `latency.laptop` is tight and `track.face` (5 ms) is not met by the spike's
numbers today. This contract does not change those numbers; it makes them visible (`LatencyStats` p95 against
`track.face`, `dropped` for the health meter) and gives the levers a place: the quality tier (PF-01), a camera
request at 640 x 480 when the tier is lowered, converting only the face region instead of the whole frame (the
tracker's input is a 256 x 256 crop), and the spike's integer crop fast path. T2 measures them.

## 10. The permission seam

Choosing and starting a camera is a device decision. Ops that call `Camera::open` or `Tracker::start` are
person-only (`AgentWrite::Never` in `docs/design/settings.md`: privacy, consent, devices): an agent gets
`OpError::person_only`, never a queued confirmation. Listing devices and reading latency stay open to agents, but
`CameraInfo::name` is what the OS shows and may carry a person's name, so it is not written to projects or logs.
The OS camera prompt is the OS's: a refusal arrives as `TrackError::PermissionDenied`, and the UI tells the person
where to grant access. Nothing in this contract can grant it.

## 11. The mock

- `FakeCamera::from_frames(format, frames)` serves given bytes and device timestamps in order;
  `FakeCamera::synthetic(format, n)` draws `n` test-pattern frames (a diagonal ramp moving one step per frame) at
  exactly `i / fps`. Every layout but MJPG. Neither waits: `timeout` is ignored. After the last frame the stream
  fails with `Lost`, like an unplugged camera. A second `open` while a stream lives is `Busy`.
- `FakeTracker::scripted(Vec<(Duration, TrackFrame)>)` releases each entry once the poll's `now` reaches its time,
  stamped with that time; a slow poller gets only the newest, and a gap longer than `lost_after` gives one `Lost`
  frame.

`omuzaki-track-mock/tests/pipeline.rs` runs the contract end to end: 90 synthetic NV12 frames through `ClockMap`
and the mailbox come out of `poll` in order at their capture times; the camera's errors; and a scripted 0.5 s gap
gives exactly one lost frame at 0.7 s, after which tracking resumes.

## 12. Decisions and alternatives

| Decision | Alternative rejected | Why |
|---|---|---|
| Triple buffer over atomic words | seqlock; `Mutex`; `unsafe` `UnsafeCell` slots | wait-free on both sides; no lock on the stage thread; no `unsafe` in a contract |
| Mailbox in the API crate | each tracker writes its own | the hand-off is where real-time bugs live; one tested copy |
| Frames borrowed from the stream | `Arc` pooled buffers | the borrow checker enforces the lifetime; no refcounts per frame; a backend may still pool behind it |
| Format choice in the contract | each backend picks | one setting means the same mode on every OS |
| Lower-envelope clock fit | mean fit; trust device time alone; trust arrival alone | delay is never negative; device time alone ignores drift; arrival alone keeps the jitter |
| Backwards time rejected | clamped | a clamped time is a false capture time; dropping one frame is harmless |
| `Tracker::start` returns `Box<dyn FrameSource>` | returns `MailboxReader` | network and replay trackers may not use the mailbox; the stage only needs `FrameSource` |
| No loom | loom model test | a new dependency; the stress test plus the mutation check cover the protocol |

## 13. Unlocked tasks

Each names its fence, contracts and acceptance, in the style of the earlier designs; the lead re-cuts the names.

**T2: `omuzaki-track`: the `Tracker` behind the face tracker.**
Fence: `crates/omuzaki-track`. Contracts: `omuzaki-track-api` (`Camera`, `Tracker`, `ClockMap`, mailbox),
`omuzaki-ai-api`, `omuzaki-anim-api`; `omuzaki-pixfmt` (lib). Work: `FaceTrackerHost` owns a thread that runs
`next_frame → pixfmt → FaceTracker::track → to_track_frame → ClockMap::map → publish`; lost and stall frames per §8;
`LatencyStats` from a fixed histogram; `QualityTier` mapped to the tracker's settings. Acceptance: runs over
`FakeCamera::synthetic` and `omuzaki-ai-mock` sessions; zero allocations per frame after warm-up (the anim-mock
test); `stop` joins the thread within 100 ms; p95 of the tracking step reported against `track.face`.

**T3: `omuzaki-camera`: Windows capture (Media Foundation).**
Fence: new `crates/omuzaki-camera` (Implementation, `cfg(windows)` backend first). Contracts: `omuzaki-track-api`.
Work: `Camera` over `IMFSourceReader` (enumerate, formats, `pick`, open), `CameraStream` lending the sample buffer,
sensor timestamps as `DeviceTime`, `seq` from the sample counter, `PermissionDenied` from the OS privacy setting.
Acceptance: builds for `x86_64-pc-windows-gnu`; unit tests for format mapping and error mapping run anywhere; a
local-lane hardware check lists a webcam and streams 300 frames with monotonic timestamps. macOS and Linux follow
as their own tasks.

**T4: `omuzaki-stage`: the perform loop.**
Fence: `crates/omuzaki-stage`. Contracts: `omuzaki-anim-api` (`FrameSource`, `Retarget`, `LayerStack`),
`omuzaki-track-api` (`Tracker`). Work: each render frame `poll`s the current source once, feeds the retarget and the
tracker layer, and falls back to the life layer on `Lost`; swapping trackers mid-performance without a frame
hitch. Acceptance: a stage test over `FakeTracker::scripted` with a lost gap gives the expected channel output; the
stage's frame loop still allocates nothing.

**T5: `perform.*` ops for tracking.**
Fence: the op crate the lead names (with T4's wiring). Contracts: `omuzaki-ops-api`, `omuzaki-track-api`. Work:
`perform.camera.list` (open to agents, names not logged), `perform.track.start {device, config}` and
`perform.track.stop` (person-only, §10), `perform.track.status` (tier, `LatencyStats`, lost or tracking).
Acceptance: an agent calling `start` gets `person_only`; ops tests over `FakeTracker`; generated CLI help and MCP
schemas regenerated, not hand-edited.

## 14. Open questions

1. **`track.face` against the spike's numbers.** The budget is 5 ms on `R`; the spike measured 6 to 9 ms on `Dev`.
   T2 measures on `R` once it exists; if it still misses, the maintainers choose between a lower default tier and
   a budget change.
2. **The issue's 100 ms against the spec's 80 ms.** This design follows the spec (`latency.laptop`); confirm.
