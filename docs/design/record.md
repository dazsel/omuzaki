---
title: "The record pipeline: encoders, recording with alpha, the replay buffer"
summary: "encoders, recording with alpha, the replay buffer"
area: design
---

# The record pipeline: encoders, recording with alpha, the replay buffer

**Status:** accepted (2026-10-13) with the default record format changed to WebM VP9 alpha
(D1).
**Features:** BR-18 (local recording with alpha) and BR-17 (replay buffer).
**Decision record:** `docs/decisions/0039-record-pipeline.md`.
**Spec:** v3 §9 (`omuzaki-stream`), §10 (budgets), §25 (BR-13, BR-17, BR-18, BR-20, BR-22, BR-23), §26 (SH-01), §40.3.
**Builds on:** `spikes/broadcast-out/README.md` §5 (the `Output` shape, alpha belongs to the stream layer, outputs on
their own thread), `docs/design/audio-api.md` §4 and §10 (taps, `TapRead`, `EngineClock`, mute is privacy),
`docs/design/render-api.md` (premultiplied sRGB-encoded targets, `read_target`), `docs/design/settings.md` (scopes,
`AgentWrite`), the automation design (live actions), `omuzaki-budget`, the FFmpeg licence and codec patent review, the
GPU output path, and the live-show supervisor (designed in parallel).

The page settles twelve questions with a reason each, in the style of `scenes.md`. Types are named and
their shape given; their docs live in the code. Numbers marked *estimate* are not measured: the build machine has no GPU
and no hardware encoder, and the primary sources below were unreachable from it. The measurement run (D12) measures them on `Dev`.

## 1. What this covers

| Asked for | Where |
|---|---|
| 1. The encoder route, hardware encoders, the banned encoders | D1 |
| 2. Where encoding sits, sharing an encode, a full queue, the budget | D2 |
| 3. The replay buffer's stored form, with the rate table | D3, §3 |
| 4. Cap semantics and defaults | D4 |
| 5. The save path, keyframe alignment, the 1 s reading, disk speed | D5 |
| 6. Recording resilience per format | D6 |
| 7. Audio tracks, PCM or lossy, patents, mute and private | D7 |
| 8. The input track and SH-01 | D8 |
| 9. Alpha conversion, overlays, clean outputs | D9 |
| 10. Ops, scopes, live actions, settings | D10 |
| 11. Supervision hooks for BR-23 | D11 |
| 12. Fakes, tests, benches, the measurement run | D12 |
| The contract, crates, the task re-cut | §5, §6 |

**Not here:** Spout, NDI and the virtual cameras (BR-01, BR-02; they implement the same `Output` trait, D2), direct
streaming (BR-05; it reuses the encoders and the packet path), the OBS-side replay buffer (BR-19), and the
process model of the live show.

## 2. The shape of it

```
render thread ── submit(&Frame) ──► Output (recorder | replay | spout | ndi | vcam)    returns at once
                                      │ copy into a pooled frame slot (shared memory when the encoder is a process)
                                      ▼
                              encoder worker: alpha conversion (D9) ─► Encoder::send/receive ─► Packets
                                      │                                                         │
                                      ▼ packets (small: 2–50 MB/s)                              │
 output host:  ┌── take writer: Muxer (MOV | WebM | MP4) or PNG files + WAV sidecars (D6, D7)   │
               └── replay ring: RollingBuffer<Payload>, byte meter (D3, D4) ── save ──► writer thread: stream copy
 audio taps (program, buses; DropNewest, 2 s) ─► AudioAligner per track ─► ring chunks and take tracks (D7)
```

## 3. Rates at 1080p30 (one table for D3, D4 and D5)

Video bytes per second per candidate, then the whole stream with audio and input added: four 48 kHz stereo f32 tracks
(program, mic, soundboard, AI voice: 4 × 384,000 = 1,536,000 B/s, computed) plus the input track (about 64,000 B/s,
*estimate*: 60 tracker frames a second of about 1 KB). Total = video + 1.6 MB/s. MB = 10⁶ bytes; 1 GiB = 1,073.7 MB.

| Candidate | Video | Total | 60 s | 5 min | Alpha | Keyframes | Status |
|---|---|---|---|---|---|---|---|
| Raw RGBA8 (8,294,400 B/frame) | 248.8 MB/s | 250.4 MB/s | 15.0 GB | 75.1 GB | yes | every frame | computed |
| FFV1, lossless, RGBA | ~80 MB/s | ~81.6 MB/s | ~4.9 GB | ~24.5 GB | yes | every frame | estimate (3:1 on flat transparent areas) |
| PNG sequence, fast preset | ~45 MB/s | ~46.6 MB/s | ~2.8 GB | ~14.0 GB | yes | every frame | estimate (~1.5 MB/frame) |
| ProRes 4444, `prores_ks` | ~45 MB/s | ~46.6 MB/s | ~2.8 GB | ~14.0 GB | yes, 16-bit | every frame | estimate: Apple's target for 1080p29.97 is 330 Mbit/s (41.3 MB/s) without alpha, +~10% for alpha; the issue's 264 Mbit/s is the 24p figure. Apple ProRes White Paper, unverified (unreachable from the build box, 2026-10-13) |
| VP9 alpha, realtime, 16 Mbit/s colour | ~2.5 MB/s | ~4.1 MB/s | ~246 MB | ~1.23 GB | yes, 8-bit | every 1 s | estimate (configured rate; the alpha plane is a second VP9 stream, ~25%) |
| H.264 hardware over the green key, 12 Mbit/s | 1.5 MB/s | 3.1 MB/s | 186 MB | 930 MB | no | every 1 s | configured rate; real rate within ±10% of target (estimate) |
| HEVC with alpha, VideoToolbox (macOS only) | ~2.5 MB/s | ~4.1 MB/s | ~246 MB | ~1.23 GB | yes | every 1 s | estimate; patents need legal review |

## 4. Decisions

### D1. The encoder route: LGPL FFmpeg as shared libraries, linked into an encoder worker

**Decision.** Encoders and muxers use LGPL FFmpeg's C API (`libavcodec`, `libavformat`, `libswscale`), built without
`--enable-gpl` and `--enable-nonfree` and loaded as shared libraries, through `ffmpeg-sys-next` bindings (never its
`static` or `build` features). This answers "FFmpeg or GStreamer": FFmpeg. Everything FFmpeg sits behind the
`ffmpeg` feature of `omuzaki-stream`; the default build, the CI build and every test except the real-FFmpeg round trips need none of it.

**Why not a spawned `ffmpeg` process fed on stdin.** It is the cleanest licence story, but it fails three needs:
- the replay ring needs *packets* with timestamps and keyframe flags, not a muxed byte stream we would parse back;
- hardware encoders on the GPU path take a device texture (D3D11, CVPixelBuffer), which no pipe can carry;
- one pipe copy is 249 MB/s at 1080p30 on top of the read-back, on the reference laptop's memory bus.
The process boundary it offers is still had: the encoder runs as a supervised worker (D11).

**Why not GStreamer.** Its licence is per plugin (the `ugly` and parts of `bad` sets are banned), so proving a build
LGPL-only means auditing every element; it is a larger runtime to ship; and the spec row and the earlier FFmpeg-or-GStreamer question already name FFmpeg.

**Bindings licence.** `ffmpeg-next` and `ffmpeg-sys-next` are WTFPL, which is a question for legal review. If legal review
rejects them, the fallback is our own `bindgen` (BSD-3) layer over the same headers: same route, no design change.

**The spine's VP9 route (2026-10-22).** Until #1424 settles the bindings, the `WebM` take with alpha does not go through
FFmpeg at all: `omuzaki-stream::vp9` loads libvpx (BSD-3) at run time with `libloading` (ISC), calls its C API
directly (two encoders, colour and alpha, as `libvpxenc.c` does), and `sinks::webm` writes the Matroska itself
(`AlphaMode` 1, alpha in `BlockAdditional` id 1, crash-safe clusters, cues). No new licence question, no build-time
dependency, and FFmpeg 6 reads the result as `yuva420p` with alpha. Measured at 1080p30 on a 4-core VM: 2.5 ms to
convert a frame and about 12 ms to encode both pictures in libvpx's real-time mode (speed 6), against a 33 ms frame.
FFmpeg stays the route for H.264/HEVC, ProRes, hardware encoders and streaming.

**Encoders per format and OS** (probed at start by opening a real 1080p encoder, never by name alone):

| Format | Software (all OSes) | Hardware, probed in this order |
|---|---|---|
| ProRes 4444 | `prores_ks` (in libavcodec, LGPL) | `prores_videotoolbox` (macOS, Apple silicon) |
| VP9 alpha | `libvpx-vp9` (libvpx, BSD-3) | none offers VP9 with alpha |
| H.264 (green-screen MP4, replay `hardware`) | none: never `libx264` | Windows `h264_nvenc`, `h264_amf`, `h264_qsv`, `h264_mf`; macOS `h264_videotoolbox`; Linux `h264_nvenc`, `h264_vaapi` |
| PNG | the `image` crate (MIT/Apache, already in the register), not FFmpeg | — |

**Defaults while legal review decides.** FFmpeg's `prores_ks` is not licensed by Apple and hardware
H.264 carries open patents, so both are **opt-in** until legal review answers; the default record format is **WebM VP9 alpha**,
and the PNG sequence is the fallback when FFmpeg is missing. The default changes to ProRes 4444 if legal review clears it.

**The banned list is enforced twice.** The pinned build cannot contain `libx264`, `libx265` or `libfdk_aac` (no
`--enable-gpl`/`--enable-nonfree`), and `probe()` also refuses any encoder with those names, so a user-supplied
FFmpeg folder with a GPL build still cannot route frames into them. A missing library is
`StreamError::Unavailable { hint: "FFmpeg libraries not found: …; record a PNG sequence, or install …" }`.

### D2. Where encoding sits: off the render thread, one queue per encode, never a silent drop

**Decision.**
- `Output::submit` copies the frame into a free slot of a fixed pool and returns. It never waits for the GPU, a lock
  held by the encoder, or the disk. The pool is allocated at `start` (no allocation once warm). With the encoder in a
  worker process, the pool is the shared-memory frame channel, so the copy happens once.
- **CPU path first.** Until the renderer gives outputs the target texture, the frame arrives as `read_target`'s `Image`. That
  read-back waits for the GPU on the render thread (about 1.2 ms at 1080p, spike §5); the measurement run measures it and the GPU
  path (a ring of output textures, the wait on the output thread) removes it. This is the only known wait on the
  render thread and it is named, not hidden.
- **One `Output` trait.** The recorder and the replay buffer implement the spike's `Output`, and so do the Spout,, NDI and virtual-camera outputs: there is one trait, in `omuzaki-stream-api` (§5).

**Sharing one encode.** The recorder and the replay buffer share one encoder, one queue and one packet stream when
their codec, size, frame rate, alpha handling and feed (program or clean, D9) are all equal; the packets then fan out
to the take writer and the ring. In practice that means a WebM VP9 recording with the default replay codec (every VP9
encode uses 1 s keyframes, so the rule needs no special case). They cannot share when the recording is a PNG sequence
(no packets), ProRes (the ring would cost 2.8 GB a minute, D3), the green-screen MP4 (no alpha), or when one is clean
and the other is not. Two encodes then run, each with its own queue.

**A full queue.** Queue depth: 16 frames for a recording (0.53 s, 133 MB of slots), 4 for the replay buffer (33 MB).
"No dropped frame over a 2-hour take" is met by choosing an encoder that keeps up, and the pipeline proves it rather
than assumes it:
- each encoder reports `load` = encode time per frame ÷ frame period (EMA, alpha 0.1, as `omuzaki-budget`);
- `load` above 0.85, or a queue more than half full, raises `RecordEvent::Falling behind` at once: the status strip
  turns amber with "Recording can't keep up: <encoder> at 0.9× real time" and offers the formats that will;
- a frame that finds the queue full is not encoded. The take repeats the previous frame in its place (the previous
  packet's duration grows by one frame period; a PNG sequence hard-links the previous file), so audio stays in sync
  and the timeline exact, counts it in `dropped_frames`, and the strip turns red with the
  count. Nothing is ever dropped silently, and nothing stalls the render thread instead.

**The budget.** `SubmitReport { cpu_ms, gpu_wait_ms, dropped }` goes back to the app with every submit, and
`OutputStatus` carries `load` and `queue_fill`. The app passes the highest encoder `load` to `omuzaki-budget` as output
pressure (a follow-up for the `omuzaki-budget` crate). Under pressure the scheduler may step down its ladder (render
scale, shadows, samples), which frees CPU when the encoder and the renderer share it. It never changes what an output
encodes: the recording's size and frame rate are fixed at `start` and the renderer upscales to the target size.

### D3. The replay buffer stores encoded packets: VP9 alpha by default, hardware H.264 as the option

**Decision.** The ring stores encoded packets with 1 s keyframe intervals, plus audio and input chunks. The codec is
`replay.codec`:
- `vp9_alpha` (default): about 4.1 MB/s in all (246 MB for 60 s, 1.23 GB for 5 min, §3), alpha kept, royalty-free,
  libvpx BSD. It costs a software encode while the buffer is on measures the CPU share on `Dev` and estimates R.
- `hardware`: H.264 over the green key, 3.1 MB/s, almost no CPU, no alpha. Offered only where a hardware encoder opens,
  and patent-cleared or not by legal review before it can become a default.

**Why not intra-only.** Exact cuts need every frame to be a keyframe, which means ProRes, PNG or FFV1: 2.8 to 4.9 GB a
minute, beyond any sensible memory cap (D4) and beyond the strict save reading's disk speed (D5). A 1 s GOP costs at
most 1 s of extra clip at the start (D5), which nobody minds in a "save the last minute" clip.
**Why VP9 over the hardware path by default.** The program output is the avatar with alpha for compositing, so a clip
without alpha cannot be re-composited, and H.264's patents are still open (§40.3). If the measurement run shows `vp9_alpha` takes more
than its share of R's CPU budget (spec §10), the maintainers re-decide the default on those numbers.

**Chunk kinds** are generic (`ChunkKind::{Video{keyframe}, Audio{track}, Input}`), so the input track (D8) adds a
producer, not ring code.

### D4. The cap counts every byte the buffer keeps alive, wherever it is

**Decision.**
- **What counts:** the payload bytes of every chunk (video, audio, input) that the buffer holds *or that a save in
  progress still holds after the ring evicted it*, side data included (§5). Disk payloads are slices of segment files
  whose store counts file bytes, including reservations in flight. A save's pins therefore shorten the buffer while it
  runs (about 1 s of data for a 1 s save); they never sit on top of the cap.
- **Admission comes before the bytes exist** (raised in review). The meter is never charged after the fact:
  0. `len` is what the chunk will occupy in its backing, computed before anything is written: in memory,
     `data.len()` plus every side datum's length; on disk, the complete serialized record
     (`SegmentStore::record_len(&chunk)`: the record header, the data, and each side datum with its kind, id and
     length fields). The meter and the store count the same number, so the cap holds for file bytes on disk;
  1. the owner of the ring asks `RollingBuffer::admit(len)`. It evicts whole keyframe groups, oldest first, until
     `meter + len <= cap`, then charges `len` to the meter and returns a `Reservation`;
  2. only then is the payload made, from the reservation (`MeteredBytes::from_reservation` for memory, the segment
     store's write for disk), which takes over the charge, so nothing is counted twice;
  3. a reservation dropped unused, or a failed write, gives its bytes back.
  If evicting every group but the newest still leaves no room (a save pins nearly the whole cap), `admit` refuses: the
  chunk is never created, the refusal is counted, the ring skips to the next keyframe (asking the encoder for one with
  `request_keyframe`), and a clip never spans the hole. So the meter is at or under the cap at every instant, not only
  after each push.
- **What does not count, and is bounded and shown separately** as `working_bytes`: the frame slots (D2), the encoder's
  own buffers, the one open disk segment's write buffer (at most one group). The measurement run checks process memory against
  cap + `working_bytes`, not cap + a guess.
- **Span.** The buffer holds `min(replay.length, cap ÷ rate)` seconds, with rate measured over the last 10 s, and says
  so: "Replay: last 60 s (of 60 s asked)" or "Replay: last 23 s: the 1 GiB cap holds 23 s at 46 MB/s".

**Defaults:** disabled (it is a continuous capture of the person, so a person turns it on, D10). BR-17's "always-on"
means it runs continuously once enabled, across scenes and outputs; enabling it is one click in the Record panel and the
go-live checklist; length 60 s (5 s to
300 s); cap 1 GiB (at least 64 MiB); memory-backed. At the default codec 1 GiB holds 262 s, so the default length never
hits the cap and 5 min needs 1.23 GB (the Settings page says so). Disk backing: one segment file per keyframe group
(about 1 s), appended as chunks arrive, deleted when its last slice drops; the folder is a cache, wiped at `start`,
never a recovery source (it is a recording of the person nobody asked to keep).

### D5. Saving: the window is fixed at once, the file completes in the background, by stream copy

**Decision.** `record.save_replay` sends a save command to the thread that owns the ring.
It handles it between two frames: the selection is fixed (payload handles cloned, no bytes copied) within one frame
period of the request, well inside 1 s. The op then returns an acknowledgement with the target path and the window
(`actual`, `early_by`); a writer thread at below-normal priority stream-copies the packets into the container (temp
name, then rename) and announces `ReplayEvent::Saved { path, bytes, took }`. Nothing is re-encoded, ever.

- **Keyframe alignment:** a clip starts at the newest keyframe at or before `end − length`, so it may start up to one
  keyframe interval (1 s) earlier than asked, never later; with an intra codec, under one frame. The report states
  `early_by`, and `short_by` when the buffer holds less than asked.
- **"Saves the last 60 s in under 1 s with no dropped frame"** reads: the window is fixed within 1 s (the hard
  requirement, met on any disk), the file completes in the background, and no frame is dropped from the running take,
  the stream or the clip. The completion time is reported per save and is not the acceptance.
- **Why not the strict reading** (a complete file within 1 s): it needs a sustained write of the whole clip in a second:

  | Ring codec, 60 s | Bytes | Strict: write speed needed | Disk-backed ring: read + write | Background time on a ~450 MB/s SATA SSD (estimate) |
  |---|---|---|---|---|
  | VP9 alpha | 246 MB | 246 MB/s | 492 MB/s | ~0.55 s |
  | H.264 hardware | 186 MB | 186 MB/s | 372 MB/s | ~0.4 s |
  | ProRes 4444 (not offered for the ring) | 2.8 GB | 2.8 GB/s | 5.6 GB/s | ~6.2 s |

  So the strict reading holds for the two ring codecs on an SSD with a memory-backed ring, fails on a hard disk, and
  never holds for ProRes. Tying correctness to the person's disk would make the feature flaky; fixing the window does not.
- **Queueing:** a second save while one runs waits (queue of 2); a third is `Busy` with "a save is already running".
  Two saves overlapping in time share payload handles, so the overlap costs no memory.

### D6. Resilience: every format leaves a readable file after a crash, `kill -9` or a full disk

| Format | While recording | After a crash or `kill -9` | Clean stop |
|---|---|---|---|
| PNG sequence | each frame `<name>.png.tmp`, then renamed | every renamed frame is whole; stale `.tmp` removed next start | — |
| WAV sidecars (all formats but MOV) | f32 WAV; the header's sizes rewritten once a second | the audio is whole up to the last second; the header covers it | sizes final |
| ProRes 4444 MOV | fragmented MOV, 1 s fragments | readable up to the last fragment | a normal MOV if the pinned FFmpeg has `movflags=hybrid_fragmented` (FFmpeg 8.0, unverified), else kept fragmented; the acceptance run checks both in DaVinci Resolve |
| WebM VP9 | Matroska clusters at each keyframe (1 s), cues at the end | playable up to the last cluster; seeking slow (no cues) | cues and duration written |
| Green-screen MP4 | fragmented (`frag_keyframe+empty_moov`) | readable up to the last fragment | final |

- **Default format:** WebM VP9 alpha when FFmpeg is present (ProRes 4444 and the MP4 are opt-in until legal review answers,
  D1), the PNG sequence when it is not. The PNG sequence is the *safe fallback*, not the default: 216,000 files and ~336 GB for 2 hours.
- **A 4 GiB WAV limit:** a stereo f32 WAV reaches it after 3.1 hours; the sink rolls the track to a new part first.
- **The disk guard** (`DiskGuard`, judged once a second on the writer thread): `Low` when fewer than 600 s of
  recording fit above `record.min_free` (amber strip, event), `Stop` under 10 s (every file finalised cleanly,
  `StopCause::DiskLow`, never a corrupt file). If another program fills the disk anyway, a failed write ends the take
  with `StopCause::WriteFailed`, finalises what it can, and leaves the crash state of the table.
- **An encoder restart** (error or worker death, D11) does not end the take: frames wait in the queue while a new
  encoder starts with a keyframe. The take continues **in the same file** when the new encoder's codec parameters
  (`StreamSpec::extradata`) are byte-identical to the old ones (ProRes and VP9 have none that change); otherwise it
  rolls to `<stem>_part2.<ext>` (and `_part2_<track>.wav`), finalising part 1 first. Each part's MOV timecode starts at
  its offset in the take, so an editor lays the parts end to end.

### D7. Audio tracks: PCM always, inside MOV, WAV sidecars elsewhere, never lossy by default

**Decision.**
- **Tracks:** `program` (the mix), `mic`, `soundboard`, `ai_voice`, `music` (`record.tracks`; all but `music` on by
  default). Each reads its tap as audio-api §10 says: `program` and per-bus post-fader taps, `DropNewest`, 2 s.
- **Where they go:** with the default WebM VP9 alpha, and with the PNG sequence and the MP4, each track is one f32 WAV
  sidecar, `<stem>_<track>.wav`; the opt-in MOV (ProRes) carries one PCM s24le stream per track instead, titled by
  track name. WebM cannot hold PCM, and BR-18 asks for separate
  tracks, which sidecars give every editor.
- **Why PCM:** editors want lossless stems; PCM has no encoder to fail, no patents and no extra CPU. Opus would be the
  only lossy candidate (BSD, patent-free) and is not needed by default. **AAC is never used** unless legal review clears it; the MP4 fallback is video-only with WAV sidecars.
- **Timing:** each frame's samples are `samples_for_frame(clock, t_frame, t_next)` (`EngineClock`), so the tracks tile
  the take exactly and end within one frame of the video. A gap (`TapRead::gap_before`) is filled with silence.
- **Mute and private:** a run whose `TapRead` says `muted` or `private` (audio-api §4.2) is written as silence; none of
  its sample values is copied anywhere, in the ring or on disk. Lost, muted and private stretches are listed with
  their cause in the take report and in the take manifest `<stem>.take.json` (format, files, frames, dropped frames,
  gaps, parts, stop cause), so an editor-side tool can show them.

### D8. The input track: SH-01's format, produced into the generic ring

**Decision: SH-01's design defines the take format, and lands first.** This page does not invent a record format that
SH-01 would then inherit unreviewed. What it fixes:
- **What is kept beside a clip:** tracker frames (`TrackFrame`, capture-stamped), audio-derived layers (cues and
  visemes with performance time, so a re-render matches even after a model update), events (ops and live actions with
  their actor), and the state at the clip's start (project document version, scene and camera ids, character id and
  version, the live state of the show).
- **How it enters the ring:** an `InputTap` hands the ring one `ChunkKind::Input` chunk of opaque bytes per frame
  period, stamped like the video. A saved clip writes them, unchanged, to `<stem>.input` in SH-01's format.
- **Until then** the replay buffer and recordings carry no input track; BR-17 stays open until the follow-up "replay:
  the input track" (blocked by an SH-01 design task) lands.

### D9. Alpha: convert once per need, after the render thread, with exact formulas

**Decision.** The program target is premultiplied, sRGB-encoded RGBA8 (render-api). Conversion runs in the encoder
worker, once per distinct need, shared between outputs (spike §5 item 2):

| Need | Who | Formula (on the stored 8-bit values) |
|---|---|---|
| Straight (PNG, ProRes, VP9) | `unpremultiply_u8` | `a == 0 → 0`, else `min(255, (c·255 + a/2) / a)` |
| Flattened (MP4, replay `hardware`) | `flatten_over(key)` | `c + (key·(255 − a) + 127) / 255`, alpha 255 |
| Premultiplied (Spout) | — | none |

- The CPU functions are the reference; a GPU pass on the GPU path must match them byte for byte on the
  8-bit target. Then `swscale` makes YUVA (`yuva444p10le` for ProRes, `yuva420p` for VP9).
- **10-bit honesty:** un-premultiplying an 8-bit premultiplied value cannot restore precision lost at low alpha, so
  ProRes's 10 bits carry 8-bit data until the renderer offers a straight, higher-precision resolve output. That is a
  render-api request, not something the stream layer can fake.
- **Overlays:** recordings and replays record what viewers see, overlays included (BR-13,
  BR-20). Each output has a `clean` option (`record.clean`, `replay.clean`) that takes the program without overlays.
  A clean feed is a second target with its own render and encode, so it never shares an encode with a normal one; the
  Settings page says it costs a render.
- **Alpha only when the format carries it:** PNG, ProRes 4444 and VP9 alpha keep alpha; the MP4 and the `hardware`
  replay flatten over `record.green_key` (default `#00b140`).

### D10. Ops, scopes and settings

| Op | Kind | Scope | Returns |
|---|---|---|---|
| `record.start { format?, tracks?, clean?, name? }` | live action (`Edit`, empty patch, `Undo::None`) | `perform` | the take's stem and status |
| `record.stop` | live action | `perform` | the take report (paths and numbers) |
| `record.status` | `Query` | `read` | `RecordStatus` |
| `record.save_replay { seconds? }` | live action | `perform` | the acknowledgement of D5: path, `requested_s`, `actual_s`, `early_by_s`, job id |
| `record.replay_status` | `Query` | `read` | `ReplayStatus`, including the last saves and their completion |

- **Why `perform`:** recording captures the person, as performing does, but publishes nothing (that is `broadcast`
  and `publish`). Agents do not hold `perform` by default (`Grants::agent_default()`), so an agent records only with a
  grant. No op takes a path: files go to the person's configured folders, so no op needs `fs`; `name` is one file-name
  component, sanitised (no separators).
- **Why live actions:** undoing a take or a saved clip is not an edit of the project; Ctrl+Z must never delete a file.
- **Outputs are paths and numbers only, never audio or pixels.**
- **Settings** (`register_setting!`):

| Key | Scope | Agent | Why |
|---|---|---|---|
| `record.format`, `record.tracks`, `record.name_template`, `record.green_key`, `record.split_s`, `record.clean`, `replay.length_s`, `replay.codec`, `replay.clean` | User | Free | taste and quality |
| `replay.enabled` | User | Confirm | turning it on starts a continuous capture (settings.md: capture is never `Free`) |
| `record.dir`, `replay.dir`, `record.min_free_mb`, `replay.max_mb`, `replay.backing` | Machine | Never | file roots and this disk's limits |

### D11. Supervision: the encoder is a worker; the ring and the files outlive it

**Decision.** The encoder is a supervised worker of kind `Encoder` (the live-show supervisor decides process or thread; BR-23's `kill -9`
acceptance implies a process). What this page requires of that model:
- **The ring, the take writers and the muxers live on the far side of the encoder,** in the output host. A worker death
  loses at most the frames in the queue and the encoder's internal look-ahead (none in realtime mode); the ring is not
  emptied and the take is not ended.
- **On restart:** the supervisor's `Spawned` makes the host create a fresh encoder from the factory with a forced
  keyframe, `pts` continuing from the frame index; the take continues or rolls as D6 says; `encoder_restarts` counts it.
- **Muxing in the host links `libavformat` there** (stream copy only, no codec opened); encoders and drivers, the
  crash-prone part, stay in the worker. PNG encoding is in the worker too.
- **Heartbeat:** the worker's progress counter is packets out; hung after `max(1 s, 4 × frame period × queue depth)`.
- **Until that supervisor runtime lands**, the recorder and the replay buffer run the encoder on a thread
  behind the same `Encoder` trait; the remote encoder is a later adapter, with no change to either.

### D12. Tests, fakes and budgets

- **Fakes**: `FakeEncoder` (deterministic packets, scripted failures and delays), `FakeEncoderFactory`,
  `MemoryMuxer` and its factory with a small test container, `ScriptedSource` (audio runs with gaps, mute and private
  flags), a `DiskProbe` fake. Clips are synthetic only: generated frames, ramps and sines; no private material.
- **Property tests:** the timeline tiles, the ring's invariants including the meter, the segment store's
  cap under concurrency.
- **Regressions from review:** an exact-cap, pinned-save case (a 64 MiB cap, a selection pinning all 64 MiB,
  then a 1 MiB chunk: the meter never reads above 64 MiB, the chunk is refused and never allocated, and pushes resume at
  the next keyframe once the selection drops); the same near-cap case on disk, with chunks carrying side data, checks
  the sum of the actual segment-file lengths (`metadata().len()`) after every append and during a pinned save, never
  above the cap and always equal to the meter; and side-data round trips: `FakeEncoder` emits a BlockAdditional per
  packet, and it survives the ring (memory and disk), the test container and a save byte for byte; a real-FFmpeg test decodes a
  saved VP9 clip and finds the alpha plane within 2 levels of the input.
- **Benches:** `push_frame` (ring push and audio alignment, ≤ 50 µs, no allocation warm) and `select` (≤ 100 µs over a
  60 s buffer); `submit` (the copy into a slot) on the push path.
- **The measurement run**: every format for 2 hours on `Dev`, the replay buffer with 20 saves, `kill -9` per
  format, the read-back wait of D2, the CPU share of `vp9_alpha`, and every *estimate* of §3 replaced by a measurement.

## 5. The contract (`omuzaki-stream-api`), confirmed and changed

The outline from the first design pass is confirmed, with these changes (the implementation turns them into code):
- **`Packet` gains `side_data: Vec<SideData>`**, with `SideData { kind: SideKind, data: Bytes }` and a
  `#[non_exhaustive] enum SideKind { BlockAdditional { id: u64 } }` (raised in review). FFmpeg's VP9 encoder puts
  the encoded alpha plane in `AV_PKT_DATA_MATROSKA_BLOCKADDITIONAL` side data, not in `data` (libvpxenc.c; matroskaenc.c
  writes it back as a BlockAdditional), so dropping it would make every default recording and replay clip opaque. Side
  data is carried through every stage: the encoder copies it out of `AVPacket`, the worker channel sends it with the
  packet, the ring stores it in the same payload (a disk segment writes a small record header: data length, then per
  side datum its kind, id and length), the muxer attaches it again with `av_packet_new_side_data`, and the meter counts
  each chunk's *stored* size (D4: the full serialized record on disk, headers included). Encoders that emit none leave the `Vec` empty, which allocates nothing.
- `StreamSpec` gains `extradata: Bytes` (D6's same-file rule). `Encoder` gains `request_keyframe(&mut self)` (parts,
  restarts). `EncodeConfig` gains `keyint: u32` in place of `gop: Option<u32>` (1 s by default, D3).
- `Output::info` returns `OutputInfo { kind: &'static str, alpha: AlphaNeed, lanes: Lanes }`: the kind is the output's
  registered name, not a central enum (§39 rule 2). `OutputConfig { name, size, fps: (u32, u32), feed: Feed }` with
  `Feed::{Program, Clean}` (D9).
- `OutputStatus { state, receivers: Option<u32>, frames, dropped, queue_fill: f32, load: f32, working_bytes: u64,
  last_error: Option<StreamError> }` (D2, D4).
- `StreamError` gains `Busy` (D5) and keeps `Unavailable { what, hint }`.
- `ByteMeter` (an `Arc<AtomicU64>` with `add`, `sub`, `get`) is in the API crate, so payload types in any crate meter
  the same cap (D4).

## 6. Worked example: one saved clip on disk

The person streams with the replay buffer at its defaults (`vp9_alpha`, 60 s, 1 GiB, memory), overlays included, and
presses the save hotkey 100.4 s into the session. The keyframe at or before 40.4 s is at 40.0 s, so the clip is 60.4 s.

```
Videos/Omuzaki/Replays/                                  replay.dir (Machine)
├── 2026-10-13_21-04-05_main_replay001.webm           VP9 alpha, 1,812 frames, 1 s clusters, ~151 MB
├── 2026-10-13_21-04-05_main_replay001_program.wav    f32 stereo, 2,899,200 frames (60.4 s × 48,000)
├── 2026-10-13_21-04-05_main_replay001_mic.wav
├── 2026-10-13_21-04-05_main_replay001_soundboard.wav
├── 2026-10-13_21-04-05_main_replay001_ai_voice.wav
└── 2026-10-13_21-04-05_main_replay001.take.json
```

```json
{ "kind": "replay", "format": "webm_vp9_alpha", "feed": "program", "requested_s": 60.0, "actual_s": 60.4,
  "early_by_s": 0.4, "frames": 1812, "dropped_frames": 0, "fps": [30, 1],
  "tracks": ["program", "mic", "soundboard", "ai_voice"],
  "gaps": [{ "track": "mic", "at_s": 12.5, "len_s": 3.0, "cause": "muted" }],
  "input": null, "encoder": "libvpx-vp9", "snapshot_ms": 21, "took_s": 0.55 }
```

The op returned `{ path, requested_s: 60, actual_s: 60.4, early_by_s: 0.4 }` 21 ms after the key press; the file was
complete 0.55 s later (244 MB: the video and four WAV tracks). The muted 3 s are zeros in `mic.wav` and in `program.wav` (the mic was muted to the mix too).
`input` stays `null` until D8's follow-up lands.

## 7. Crates

- **`omuzaki-stream-api`** (new, API): §5's types and traits. Depends on `omuzaki-core`, `omuzaki-render-api`, `bytes`,
  `serde`, `schemars`, `thiserror`; builds for wasm.
- **`omuzaki-stream`** (new, implementation): the timeline, alpha, sinks, recorder, replay, FFmpeg (feature), watchdog
  modules, ops and settings (module map). Its `crates/README.md` rows land with the crates; "crates so far" lists only crates that exist.

## 8. Sources

the design brief; spec v3 §9, §10, §25, §26,
§40.3; `spikes/broadcast-out/README.md`; `docs/design/audio-api.md`, `render-api.md`, `settings.md`, `scenes.md`.
Apple ProRes White Paper (apple.com, ProRes target data rates) and the FFmpeg formats documentation (`movflags`):
unverified (unreachable from the build box, 2026-10-13). Rates in §3 not marked computed or configured are estimates
until the measurement run.
