---
title: "Takes: the recorded inputs beside a recording or a replay clip (SH-01)"
summary: "the recorded inputs beside a recording or a replay clip (SH-01)"
area: design
---

# Takes: the recorded inputs beside a recording or a replay clip (SH-01)

**Status:** proposed (PR, 2026-10-13); a maintainer accepts it.
**Features:** SH-01 (record takes); the input track of BR-17 and BR-18; the input side of SH-05
(deterministic offline render).
**Decision record:** `docs/decisions/0052-take-format.md`.
**Spec:** v3 §26 (SH-01, SH-05, SH-11), §25 (BR-17), §10 (budgets), §8 (real-time plane).
**Builds on:** `record.md` D3 to D8, §3, §6 (ADR 0039); `audio-api.md` §4.2, §5.1, §5.2 (mute, performance time,
`PlayoutDelay`); `anim-api.md` (channels, bus, `TrackFrame`, `Layer`, `Cues`); `ai-performer.md` §7 (live
actions); `journal.md` and `omuzaki-history-api`; ADRs 0009, 0011 (the replay input track). Pages named alone are in `docs/design/`.

record.md D8 left the take format to SH-01 and fixed only its outline: what is kept, and that it enters the replay ring
as one opaque `ChunkKind::Input` chunk per frame period. This page settles the six questions below, with a reason each.
Sizes come from the byte layout; those depending on producers not yet built are marked *estimate*.

## 1. What this covers

| Asked for | Where |
|---|---|
| 1. What is recorded | T1 |
| 2. The framing, byte layout, version field, older and newer readers | T2, T3 |
| 3. `<stem>.input` on disk: header, index, checksum, crashes, size budget | T4, §3 |
| 4. Privacy: muted and private stretches | T5 |
| 5. Re-rendering (SH-05), model and character changes | T6 |
| 6. Who owns the producers and the reader; the API crate | T7, §5 |
| The task cut and a worked example | planned task breakdown |

**Not here:** the offline renderer and its queue, the timeline and cleanup layers that edit a take (SH-02,
SH-11; they use the same reader), the live-show supervisor.

## 2. The shape of it

```
tracker sources ─► stage frame loop ─► PlayoutDelay ─► LayerStack::resolve ─► Bus ─► life ─► ChannelBuf ─► rig, face
 voice taps (lip sync, cues) ──┘  │ (what entered)                                               │ (what resolved)
 op event stream ─► EventFeed ────┤                                                              │
 live-state providers ─► StateFeed┤                                                              │
                                  ▼                                                              ▼
                     StageInputTap: one chunk per frame period, records in time order ◄──────────┘
                                  │ InputTap (omuzaki-stream-api)
                 ┌────────────────┴──────────────────┐
     replay ring: ChunkKind::Input (D3)     recorder: take writer
     save ─► `<stem>.input` (header, chunks unchanged, index, trailer)  ─► `<stem>.input`
                                  │
                         omuzaki-take reader ─► TakeReplay ─► SH-05 offline render, SH-02 timeline, SH-11 cleanup
```

## 3. Size budget (one table for T2 and T4)

One performer, a webcam tracker at 60 Hz sending 51 blendshapes and a head pose with position, lip sync and cues at
100 Hz (*estimate*: omuzaki-voice's hop is not built yet), one State record a second of about 2,000 B and one event a
second of about 400 B (both *estimate*). Record sizes include the 8-byte record header (T2).

| Item | Bytes each | Per second at 30 fps | At 60 fps | Status |
|---|---|---|---|---|
| Chunk header | 32 | 960 | 1,920 | computed |
| `Track` (51 present, head with position) | 264 | 15,840 (60 Hz) | 15,840 | computed |
| `Bus` (89 channels set) | 384 | 11,520 | 23,040 | computed |
| `Voice` (15 mouth channels) | 100 | 10,000 (100 Hz) | 10,000 | computed size, estimated rate |
| `Cue` | 28 | 2,800 (100 Hz) | 2,800 | computed size, estimated rate |
| `State` | ~2,000 | ~2,000 | ~2,000 | estimate |
| `Event` | ~400 | ~400 | ~400 | estimate |
| **Total** | | **~43,500 B/s** | **~56,000 B/s** | |

Both are under record.md §3's 64,000 B/s estimate (it assumed 1 KB tracker frames). That is 2.6 MB a minute, 313 MB
for 2 hours at 30 fps: 1.1% of the VP9 alpha stream. Each extra performer adds 11.5 KB/s (`Bus`), plus ~13 KB/s with a
voice. No compression in format 1: raw little-endian floats stay bit-exact (ADR 0009); a minor may add a flag (T3).

## 4. Decisions

### T1. What is recorded: what entered the pipeline, and what it resolved to

**Decision.** Per frame period the take keeps, in time order:

| Kind | What | Why |
|---|---|---|
| `Track` | each `TrackFrame` the stage *polled* (not every frame a sender sent), with its capture `time`, `seq` and a source index | the pipeline's newest-frame-wins drops are part of the performance; replaying what was polled reproduces them |
| `Voice` | each audio-derived `Layer` (visemes) pushed into `PlayoutDelay`, stamped with `perf_time` | a lip-sync model update must not change an old take |
| `Cue` | each `Cues` sample (loudness, onset, laugh) with `perf_time` | same |
| `Bus` | per cast member, the final `ChannelBuf` after the life layer, before the rig (mask plus set channels) | the life layer's blinks and saccades come from an RNG whose state is private (`omuzaki_core::Rng`) and from timers; recording its output is the only way a re-render blinks where the live show did |
| `Event` | each journal entry during the take, in LSN order (`HistoryEntry`: `Op`, `Undo`, `Redo`, `Rollback`, `Begin`, `Commit`, `Dropped` in full; `Mark` and `Session` as a bare `{lsn, kind}` placeholder so LSNs stay contiguous), with its `lsn`, its `applied_frame` (T2) and its physical patch; `Op` keeps its record (op, args, actor, cause kind, tx, `at`, version, `on_behalf_of`) | hotkeys, expressions, AI intents (`performer.intent`), scene and camera switches are ops or live actions, and undo, redo and rollback change the document through their own entries, so only the journal's ordered, physical stream reconstructs the document (Codex review) |
| `State` | the start state, repeated at every group start (T2) | a clip can start at any keyframe group |
| `Withheld` | the start and end of a muted or private run (T5) | the gap is shown, never silent |
| `Delay` | the `PlayoutDelay` `D` when it changes | `re_perform` (T6) replays the same delay |

**State** is JSON: `doc_version`, `project`, `scene`, `camera`, `cast` (member id, character id, the character's
content hash or imported-avatar store hash), `sources` (index → member, `DeviceProfile`, source kind), `voices` (index →
member, tap), `models` (registry slot → model ref, for the tracker and lip sync in use), `delay_s`, `after_lsn` (the
last journal entry already reflected), `withheld` (the muted or private runs open at this frame: voice, cause, `from`;
T5), `omitted` (providers left out by the size cap, T2), `overflow` and `live`: a map filled by **live-state providers** (`register_take_state!`, one per
subsystem with live state: held expressions, the show's variables, the OBS scene, the mic mode). An unknown provider
name is skipped and reported by readers; a new provider never needs a format change.

**Why record both the raw inputs and the resolved bus.** The bus alone gives an exact "as performed" re-render with any
camera, quality or character (the bus is the character-agnostic ARKit-native vocabulary, anim-api §2), but cannot
re-run the retarget or another character's life personality. The raw inputs alone cannot reproduce live blinks. Both
together cost ~27 KB/s of the ~43.5, and give the two modes of T6.

**Not recorded:** audio samples (they are the take's WAV sidecars, record.md D7, so the `.input` never duplicates them
and the mute rule has one owner); captions and transcripts (derived text, recomputed from the WAV if wanted); `cause`
text (prompts and chat messages: personal data a re-render does not need, T5); render settings (the re-render chooses
its own).

### T2. The framing: one self-describing chunk per frame period

**Decision.** A chunk is a 32-byte header and a payload of records. The ring stores the whole chunk as its opaque
`ChunkKind::Input` bytes, and a save or a recording writes the same bytes to disk unchanged (record.md D8).

```
chunk header (32 B, little-endian)
 0  magic     [u8; 4]  b"TKCH"
 4  major     u8       1
 5  minor     u8       0
 6  flags     u16      bit 0 GROUP_START (carries a State), bit 1 HAS_WITHHELD, bit 2 TAIL (T6); bits 8-15 must-understand
 8  frame     i64      video frame index in the session's timebase (the video packet's pts)
16  tick      f64      pipeline time of the frame tick (`Tick::time`), seconds
24  len       u32      payload bytes
28  crc       u32      CRC-32 (IEEE) of bytes 0..28 and the payload
payload: records, each   kind u8 | rflags u8 | reserved u16 | len u32 | body[len]
```

- **Record bodies** are fixed little-endian layouts for the hot kinds (`Track`: seq u64, time f64, source u8, profile
  u8, status u8, has-flags u8, present u64, then one f32 per present channel in channel order, head 3×f32 and position
  3×f32 if flagged, eyes 4×f32 if flagged; `Voice`: voice u8, mode u8, 2 reserved, perf_time f64, weight f32, mask
  u128, one f32 per mask bit; `Cue`: voice u8, onset u8, 2 reserved, perf_time f64, loudness f32, laugh f32; `Bus`:
  member u8, 3 reserved, mask u128, f32 per bit) and serde JSON (`float_roundtrip`) for the cold kinds (`State`,
  `Event`, `EventRef`, `Withheld`, `Incomplete`, `Delay`). Floats are stored as their bits, so they round-trip exactly.
- **Group start.** The output host sets `FrameStamp::group_start` on each video keyframe of an inter codec, and every
  `fps` frames (and on forced keyframes) for intra formats. The chunk of that frame carries a `State` first. A replay
  clip starts at a keyframe (record.md D5), so its first chunk always holds its start state, and the ring needs no
  special case: the State lives in the group the ring evicts with it.
- **Stamping.** `frame` is the same index as the video frame's packet. Records inside carry their own capture or
  performance time, so the chunk says *which video frame they belong to* and the record says *when it happened*.
- **Why not JSON per frame** (the mock's `RecordedTrack`, which stays the fixture form): ~2 KB a `TrackFrame`, and
  parsing on the frame thread. **Why a CRC per chunk:** a reader resyncs after a torn write and checks a clip without
  the index; `crc32fast` (MIT OR Apache-2.0) is already in the register as transitive, so going direct adds a row.

**No allocation on the frame thread, and bounded chunks** (Codex review). `StageInputTap` encodes into its own
buffer, reused: `frame_max + 16 KiB (State) + 32 KiB (events)`, where `frame_max` is computed from the cast and sources
(each hot record has a fixed maximum size, and at most `ceil(rate ÷ fps) + 1` of a kind fit one frame period; extra
pushes in one period are newest-wins drops, counted). A cast change hands over a larger buffer, allocated off-thread,
with the next State. `Event` and `State` bytes are serialised off-thread (T7) and only copied in. The limits:
- **State ≤ 16 KiB, the whole serialised record.** The mandatory part is bounded by fixed limits: ≤ 16 cast members,
  sources and voices, ≤ 32 models, ids ≤ 128 B, `withheld` ≤ one run per voice, which keeps it under 8 KiB. Providers
  are then added in name order; one that would pass the cap is left out and named in `omitted`, which holds at most 32
  names and then a count (`omitted_more`). If the serialised record still passes 16 KiB (a limit was raised without
  this rule), the StateFeed writes the **minimal State** instead: `doc_version`, `after_lsn`, `withheld` and
  `overflow: true`, bounded by construction; readers then treat the cast and live state as unknown for that group
  and the take report counts `state_overflow` (Codex review).
- **Events ≤ 32 KiB per chunk, placed by `applied_frame`**, the first frame whose tick began after the executor appended
  the entry (so undo and redo are placed too). The tap drains the queue in LSN order within the budget; the rest wait,
  in order, and count in `deferred`. **Readers apply every entry at its `applied_frame`.** No read-ahead bound holds (93
  entries of 11 KiB drain two a chunk, 46 frames late), so completeness is stated against `boundary(f)`, which covers
  entries still awaiting serialisation. Single-writer `SeqCst` atomics, nobody waits: the tee (executor thread) does
  `in_flight += 1`, reads `frame` (`applied_frame = frame+1`), stores `stamped_lsn`, `in_flight -= 1`; the stage stores
  `frame = f`, then loads `in_flight` and `stamped_lsn`. If `in_flight` was 0, that load is `boundary(f)`, an **upper
  bound**: an entry may stamp itself for `f + 1` between the two loads and be covered too, which only delays a
  watermark, never claims an unwritten entry (any entry not counted reads `frame ≥ f`). If not, `f` stays open until a
  later frame start, or `tail`, reads 0. The tap tracks `written_lsn`; a chunk for `f` with `written_lsn ≥ boundary(f)`
  is complete through `f`, any other carries **`Watermark { complete_through: w }`**, the newest `w` with `boundary(w) ≤
  written_lsn` (body: `w` as i64, 8 bytes; `Record::Watermark(i64)`). Readers read ahead until covered, the TAIL ends or
  the file ends (`incomplete_tail`); the report gives the largest lag (`max_defer_frames`).
- **Stop and save: a TAIL of bounded chunks** through `InputTap::tail` (§5): `Chunk` (one `TAIL` chunk, within the
  budget, with its watermark), `Pending` (not yet written through `boundary(last frame)`, and for a save through
  `upto_lsn`) or `Done`. `tail` itself re-reads `in_flight` and `stamped_lsn` and closes the last frame's open boundary
  as soon as `in_flight` reads 0, so no later frame is needed; the host retries `Pending` after the next frame, or at
  stop for up to 2 s, then writes `Incomplete`. A **stop** drains the queue. A **save** copies and never drains: its
  TAIL holds the queued entries up to the window's last frame, then those up to `snapshot_lsn` (from the feed's retained
  last 2 s). At a clip's start, entries with `lsn ≤ after_lsn` of the first State are already in it and are skipped.
- **Invariants** (a project decision), proved by a planned `loom` model test, where every interleaving found in
  review is a named case: **(S)** `boundary(f)` is at or after every entry applied at or before `f`, and may cover
  entries of `f + 1`; **(L0)** once stop is requested and `in_flight` reads 0, `tail` closes the last frame's boundary
  with no new frame; **(L1)** stop reaches `Done` whenever, before the 2 s deadline, the tee drains and the serialiser
  writes and the TAIL drains through `boundary(last frame)`, and `Incomplete` otherwise; **(L2)** the frame thread never
  blocks on the tee or on a save; **(D)** a save never drains the live queue.
- **One entry over 16 KiB** (a large import's patch) is written as `EventRef { lsn, applied_frame }`, resolved from the project journal
  by the reader, and counted.
- **The queue is bounded** (1,024 entries or 1 MiB) and never blocks the executor: the journal tee uses `try_send`. On
  overflow the feed stops taking entries, writes `Incomplete { from_lsn, cause: "event_overflow" }` in the next chunk,
  and resumes only at the next State (which re-bases `after_lsn`); the take report says `incomplete`, and `DocAt` refuses
  to cross the hole (`StateGap`) instead of guessing.

### T3. Versions: what an older or newer reader does

| The reader meets | It does |
|---|---|
| Same major, same or older minor | reads everything |
| Same major, newer minor | reads; skips unknown record kinds by `len` and counts them (`TakeReport::skipped`); a known kind's body may grow at its end in a later minor, so it reads the prefix it knows; unknown JSON fields are ignored |
| An unknown must-understand flag bit (8-15) | refuses the file: `TakeError::Newer { found, supported, hint: "update Omuzaki to read this take" }` |
| Newer major | the same refusal |
| Older major | decodes with that major's decoder: decoders are append-only, like library migrations (ADR 0019) |

Minor versions are additive by rule (a new kind, a longer body, a new flag in bits 0-7); anything else is a new major,
which needs a superseding ADR. The file header and every chunk carry the version, so a chunk copied out of the ring is
still self-describing.

### T4. `<stem>.input` on disk: header, chunks, index, trailer

```
file header   magic b"OMUZAKIINP" | major u16 | minor u16 | json_len u32 | crc u32 | header JSON
chunks        as T2, appended in frame order
index         magic b"TKIX" | count u32 | count × (frame i64, offset u64)     one entry per GROUP_START chunk
trailer (24)  index_offset u64 | chunks u64 | crc u32 (of index and trailer) | magic b"TKND"
```

The header JSON (`TakeHeader`): `take` (the stem), `kind` (`recording` | `replay`), `fps` `[30, 1]`, `first_frame`
(chunks keep their session frame numbers, so the file is the ring's bytes unchanged; file frame = `frame −
first_frame`), `created` (UTC), `app` (Omuzaki version), `snapshot`, `snapshot_lsn` and `snapshot_at` (T6), `doc_version`, `audio` (the WAV sidecar names
by track) and `video` (the video file name).

**Seeking.** `seek(frame)` finds the newest index entry at or before `frame`, reads its State, and applies the `Event`
records from there; with the trailer present that is one lookup. **Crash and `kill -9`:** the header is written and
flushed at start, chunks are appended and flushed once a second (the WAV cadence of record.md D6), the index and
trailer only on a clean finish. A reader that finds no valid trailer scans: it hops chunk headers by `len`, checks each
CRC, and stops at the first bad or truncated chunk, reporting `truncated_at`. So a crash loses at most the unflushed
second, like every other track of the take (record.md D6), and nothing before it. `take.repair` writes the missing
index and trailer (a scan of a 2-hour, 313 MB file reads it once: under 1 s on a SATA SSD, *estimate*). Recordings that
roll to `_part2` (record.md D6) roll the `.input` with them; each part has its own header and starts with a State.

The take manifest `<stem>.take.json` (record.md D7) gets `"input": { "path", "major", "minor", "chunks", "bytes",
"withheld": [...], "event_refs": n }`, or `null` when the input track is off.

### T5. Privacy: a muted or private run leaves a marker, never derived values

**Decision.** For a voice whose `TapRead` says `muted` or `private` (audio-api §4.2, including `cohost_only`), the tap
writes **no `Voice` and no `Cue` record** for that run, only `Withheld { voice, cause, from, to }` (an open record at
the start, closed at the end), exactly as record.md D7 writes silence and never copies samples. Visemes and loudness
are a function of the voice, so they are the voice; copying them would leak what record.md D7 protects.

**A window that cuts a run** (Codex review). Every State lists the runs open at its frame (`withheld`), so a
clip that starts inside a run, or lies wholly inside one, still knows it: the reader opens each listed run at the clip's
first frame, closes any run still open at the clip's last frame, and clamps every run to the window. The take report
and `take.json` list the clamped runs, so a 10 s clip cut from the middle of a 60 s mute reports 10 s withheld.

- **What stays:** `Track` frames and the `Bus`. Mute is an audio control; the avatar stays on stream and viewers see it,
  so the take keeps what viewers saw. A private co-host turn already applies no intents (ai-performer §5.4), so no
  `Event` records exist for it to leak.
- **Events** keep `cause.kind` and drop `cause.text` always (prompts, chat lines, redeem messages). Args and patches are
  the same op log the project journal already keeps, so the take adds no new exposure of them.
- **The file is personal data** (tracking, bus, visemes, journal entries; no audio, no video). It is on by default
  with an off switch, `record.input` (User, agent `Confirm`); deleting a take deletes its `.input` and releases its
  snapshot pin (T6); no publish or upload op picks it up unless the person names it (a project decision).

### T6. Re-rendering: what SH-05 needs, and what happens when versions moved

**From the take:** the chunks from a State onwards, the video's `fps` and frame count, the WAV sidecars for the
render's audio, the cast and scene in State. **From the project:** the document as it was, pinned by a snapshot
(`Snapshots::take(Trigger::Take, stem)`, a new, additive `Trigger` variant). The header names the snapshot and the
exact journal position it holds, `snapshot_lsn`, and which side of the take it sits on (Codex review):

| Take | Snapshot taken | `snapshot_at` | The document at frame `f` |
|---|---|---|---|
| Recording | at start, before the first chunk; the feed records every entry with `lsn > snapshot_lsn`; at stop the queue drains into `TAIL` chunks (T2) | `start` | the snapshot with every entry whose `applied_frame ≤ f` applied forward, in LSN order |
| Replay save | when the window is fixed; the save's `TAIL` chunks (T2) carry the entries up to `snapshot_lsn` | `end` | the snapshot with every entry whose `applied_frame > f`, up to `snapshot_lsn`, inverted newest first (`Patch::inverse` is exact) |

A file never mixes the two: a forward reader refuses `end` snapshots and the reverse. Entries must be contiguous in
LSN (placeholders for `Mark` and `Session` count, so `Op → Mark → Op` has no gap); a real gap or an `Incomplete` marker is `StateGap { from_lsn }`, and the reader starts again from the next State whose
document it can prove, or from the current project if none, saying so. Example: a recording with `x: 0` at start,
an op setting `x: 1`, then its undo: forward from the start snapshot gives `0, 1, 0`; a replay save over the same
frames inverts the undo, then the op, from the end snapshot (`x: 0`), giving `0, 1, 0` too.

**Two modes**, chosen per render:

| Mode | Drives the character from | Keeps | Use for |
|---|---|---|---|
| `as_performed` (default) | the recorded `Bus` per member | exact timing of every expression, mouth shape, blink and look | higher quality, a new camera, a new resolution or frame rate, another character |
| `re_perform` | `Track`, `Voice`, `Cue` and `Event` records through the current retarget, `PlayoutDelay` (recorded `D`) and life layer, seeded from the project | the raw performance, re-interpreted | another character's personality and calibration |

Spring bones and physics run fresh in both, after a settle of 1 s holding the first frame's bus, since their state is
character-specific and is not recorded. Ticks come from the chunk headers at the recorded frame rate; a render at
another frame rate samples the bus between chunks (linear; booleans and onsets at the nearest earlier chunk).

**When versions moved:**
- *A tracker or lip-sync model was updated:* nothing changes in either mode: their outputs are recorded.
- *The character changed:* the snapshot renders it as it was; `--character current` uses today's, diff reported.
- *Another character:* the bus maps by channel id; `customNN` channels are per character, so they map by the target
  character's custom channel names when both define the name and are dropped otherwise, each drop reported.
- *A newer Omuzaki build:* deterministic per build; across builds the performance holds, pixels may differ (`app`).
- *The snapshot:* pinned while the take's `.input` exists (a project decision); if it is missing, the render says so and
  uses the current project.

### T7. Who owns what

- **`omuzaki-take-api`** (new, API, builds for wasm): the types and constants of T1 to T4 and the reader contract (§5).
- **`omuzaki-take`** (new, `layer = "lib"`, ADR 0011): the format itself, one codec for every user: chunk encode into a
  caller's buffer, zero-copy record views, the `.input` writer (header, append, flush cadence, index on finish) and
  reader (index or scan), `DocAt` (patch inversion) and `TakeReplay`. A format belongs in a lib crate, so the stream
  writer, the stage producer and the offline renderer share it without depending on each other.
- **`omuzaki-take-mock`** (new, Mock): scripted synthetic takes and an independent reference decoder, the oracle the lib is
  checked against (contracts and mocks never depend on a lib).
- **Producers:** `StageInputTap`, `EventFeed` and `StateFeed` in **`omuzaki-stage`** (App). The stage owns the frame loop,
  `PlayoutDelay` and `LayerStack::resolve`, so it is the only place that sees both what entered and what resolved;
  the feeds read a tee of the executor's `JournalSink` (every `HistoryEntry`, in LSN order) and the live-state
  providers on their own thread and hand pre-serialised
  bytes over a lock-free swap (State) and an SPSC queue (events).
- **The seam:** `InputTap` moves to **`omuzaki-stream-api`**, so the stage implements it without depending on the
  `omuzaki-stream` implementation; the ring and the recorder stay ignorant of the record kinds. This amends the stream API contract (below).
- **Writers:** `omuzaki-stream` (recorder and replay saves) writes `.input` through `omuzaki-take`.
- **The reader's users:** SH-05's runner (picks which crate to read with), SH-02's timeline, SH-11's cleanup.

## 5. The contract (`omuzaki-take-api`), outline

```rust
pub const MAJOR: u8 = 1; pub const MINOR: u8 = 0;
pub const CHUNK_MAGIC: [u8; 4] = *b"TKCH"; pub const FILE_MAGIC: [u8; 10] = *b"OMUZAKIINP";
pub const CHUNK_HEADER_LEN: usize = 32;
#[non_exhaustive] #[repr(u8)] pub enum RecordKind { Track = 1, Voice, Cue, Bus, Event, EventRef, State, Withheld, Delay, Incomplete, Watermark }
pub struct ChunkFlags(pub u16);   // GROUP_START, HAS_WITHHELD, TAIL; MUST_UNDERSTAND = 0xFF00
#[non_exhaustive] pub struct ChunkHeader { pub major: u8, pub minor: u8, pub flags: ChunkFlags, pub frame: i64, pub tick: Seconds, pub len: u32 }
// ChunkHeader::new(..); ChunkHeader::check() -> Result<(), TakeError>: a newer major or an unknown must-understand flag is TakeError::Newer
#[non_exhaustive] pub enum TakeKind { Recording, Replay }
#[non_exhaustive] pub enum SnapshotAt { Start, End }
#[non_exhaustive] pub struct TakeHeader { pub take: String, pub kind: TakeKind, pub fps: (u32, u32), pub first_frame: i64,
    pub created: String, pub app: String, pub snapshot: Option<SnapshotId>, pub snapshot_lsn: Lsn,
    pub snapshot_at: SnapshotAt /* Start | End */, pub doc_version: DocVersion,
    pub video: Option<String>, pub audio: BTreeMap<String, String> }   // TakeHeader::new(..) + with_snapshot, with_video, with_audio
pub struct ProjectName(/* private */ String);   // ProjectName::new(..) -> Option<Self>: None if empty or blank; the one rule for constructors and the reader
#[non_exhaustive] pub struct StateRecord { pub doc_version: DocVersion, pub project: Option<ProjectName> /* None only when overflow */, pub scene: Option<SceneId>,
    pub camera: Option<String>, pub cast: Vec<CastEntry>, pub sources: Vec<SourceEntry>, pub voices: Vec<VoiceEntry>,
    pub models: BTreeMap<String, String>, pub delay_s: f32, pub after_lsn: Lsn, pub withheld: Vec<Withheld>,
    pub omitted: Vec<String>, pub omitted_more: u32, pub overflow: bool, pub live: BTreeMap<String, Value> }
// StateRecord::new(doc_version, ProjectName, after_lsn, withheld) + with_*; StateRecord::minimal(doc_version, after_lsn, withheld) (the overflow State, no project).
// Reading refuses a State without a project unless overflow is true. CastEntry, SourceEntry and VoiceEntry are #[non_exhaustive] with new(..).
#[non_exhaustive] pub struct EventRecord { pub lsn: Lsn, pub applied_frame: i64, pub entry: TakeEntry }
#[non_exhaustive] pub enum TakeEntry { Full(HistoryEntry) /* Op's cause text cleared; never Mark or Session */, Placeholder { kind: String }, Unknown /* a newer form */ }
#[non_exhaustive] pub struct Incomplete { pub from_lsn: Lsn, pub cause: String }
#[non_exhaustive] pub struct Withheld { pub voice: u8, pub cause: WithheldCause /* #[non_exhaustive]: Muted | Private | CohostOnly | Unknown */, pub from: Seconds, pub to: Option<Seconds> }
#[non_exhaustive] pub enum Record<'a> { Track(TrackFrame, u8), Voice { voice: u8, perf_time: Seconds, layer: Layer }, Cue { voice: u8,
    perf_time: Seconds, cues: Cues }, Bus { member: u8, buf: ChannelBuf, mask: ChannelMask }, Event(&'a [u8]),
    EventRef { lsn: Lsn, applied_frame: i64 }, Watermark(i64), State(&'a [u8]), Withheld(&'a [u8]), Incomplete(&'a [u8]), Delay(f32), Unknown { kind: u8, body: &'a [u8] } }
#[non_exhaustive] pub struct ChunkView<'a> { pub header: ChunkHeader, pub payload: &'a [u8] }   // ChunkView::new(..)
pub trait TakeSource { fn header(&self) -> &TakeHeader; fn seek(&mut self, frame: i64) -> Result<(), TakeError>;
    fn next_chunk(&mut self) -> Result<Option<ChunkView<'_>>, TakeError>; fn report(&self) -> &TakeReport; }
#[non_exhaustive] pub struct TakeReport { pub chunks: u64, pub truncated_at: Option<u64>, pub skipped: BTreeMap<u8, u64>,
    pub withheld: Vec<Withheld> /* clamped to the window */, pub event_refs: u64, pub deferred: u64,
    pub omitted: BTreeMap<String, u64>, pub state_overflow: u64, pub incomplete: Vec<Incomplete>,
    pub max_defer_frames: u32, pub incomplete_tail: bool, pub indexed: bool }   // TakeReport::new() + with_*
#[non_exhaustive] pub enum TakeError { Newer { found: (u16, u16) /* the file header holds u16; a chunk's u8 pair widens */, supported: (u8, u8), hint: String }, BadMagic,
    Crc { offset: u64 }, Truncated { offset: u64 }, StateGap { from_lsn: Lsn }, WrongSnapshotSide, Json(String), Io(String) }
#[non_exhaustive] pub struct StateProvider { pub name: &'static str, pub capture: fn() -> Value }   // StateProvider::new(..), built by the macro
#[macro_export] macro_rules! register_take_state { /* (name, fn() -> serde_json::Value), inventory */ }
// omuzaki-stream-api (amends):
pub struct FrameStamp { pub index: i64, pub tick: Seconds, pub until: Seconds, pub group_start: bool }
pub enum TailMode { Stop /* drains */, Save { last_frame: i64, upto_lsn: Lsn } /* copies */ }
pub enum TailStep { Chunk, Pending, Done }
pub trait InputTap: Send {
    fn take(&mut self, stamp: FrameStamp, out: &mut Vec<u8>) -> bool;
    /// One TAIL step; the host calls it until `Done`. Default: no tail.
    fn tail(&mut self, mode: &TailMode, cursor: &mut u64, out: &mut Vec<u8>) -> TailStep { let _ = (mode, cursor, out); TailStep::Done }
}
```

`out` is the host's reused buffer (cleared, never shrunk): the tap appends one whole chunk and the host then admits
`out.len()` to the ring (record.md D4) and copies it. `tail` runs on the same thread, between frames (record.md D5);
`cursor` is the host's position in the copy (starts at 0), so a save never disturbs the live queue. Depends on `omuzaki-core`, `omuzaki-anim-api`, `omuzaki-ops-api`,
`omuzaki-doc-api`, `omuzaki-history-api`, `omuzaki-stage-api`, `serde`, `serde_json`, `schemars`, `thiserror`, `inventory`.

**Where the docs disagree, and the pick:**
- `InputTap::take(until, out)` is in `omuzaki-stream`, with no group signal. Picked: `FrameStamp` with
  `group_start` in `omuzaki-stream-api`: State must sit in the group the ring evicts (T2), and an App implements a
  contract, not an implementation's trait. The save still writes the chunks unchanged, inside T4's framing.
- record.md §3 assumed 1 KB tracker frames (~64,000 B/s); picked the encoded 256 B (~43.5 KB/s at 30 fps).
- ops-api's `Event` stream (`OpApplied`, `Undone`, …) carries no patch for undo, redo or rollback; the journal's
  `HistoryEntry` does. Picked: the journal, through a tee on the executor's `JournalSink` (Codex review).
- record.md D8's "state at the clip's start" is repeated here at every group start, so any keyframe starts a clip.

## 6. Worked example: the saved clip of record.md §6, with its input track

The person streams with the replay buffer on and a webcam at 60 Hz; the mic is muted from 12.5 s to 15.5 s of the clip.
The save at 100.4 s selects frames 1,200 to 3,011 (40.0 s to 100.4 s, 1,812 frames, 61 keyframe groups).

```
Videos/Omuzaki/Replays/
├── 2026-10-13_21-04-05_main_replay001.webm
├── 2026-10-13_21-04-05_main_replay001_{program,mic,soundboard,ai_voice}.wav
├── 2026-10-13_21-04-05_main_replay001.input            ~2.59 MB (estimate below)
└── 2026-10-13_21-04-05_main_replay001.take.json        "input": {…} replaces null
```

```
0x000000  "OMUZAKIINP" 01 00 00 00  json_len 0x1A8  crc  {"take":"2026-10-13_21-04-05_main_replay001","kind":"replay",
          "fps":[30,1],"first_frame":1200,"snapshot":"b3:9c41…","snapshot_lsn":5034,"snapshot_at":"end","doc_version":412,"video":"…replay001.webm",
          "audio":{"program":"…_program.wav","mic":"…_mic.wav",…},"app":"0.1.0","created":"2026-10-13T21:05:45Z"}
0x0001BC  "TKCH" 01 00 0100 frame=1200 tick=40.0 len=3,4xx crc   GROUP_START
            State {"doc_version":412,"scene":"main","camera":"cam_a","cast":[{"member":0,"character":"…"}],
                   "delay_s":0.12,"after_lsn":5021,"withheld":[],"omitted":[],"live":{"expressions":{"held":["smug"]},"mic":{"mode":"live"}}}
            Bus m0 (384 B) · Track s0 seq 88213 t=39.9861 (264 B) · Track s0 seq 88214 (264 B) · Voice ×3 · Cue ×3
next     "TKCH" 01 00 0000 frame=1201 tick=40.0333 len=1,4xx …
…         frame 1575 (clip 12.5 s): Withheld {"voice":0,"cause":"muted","from":52.5} — no Voice or Cue for 90 frames
…         frame 1665: Withheld closes {"to":55.5}; frame 2110: Event performer.intent {"emote":{"expression":"happy"}}
tail      "TKCH" 01 00 0004 frame=3012 TAIL: Event lsn 5034 (an `Op` applied between the last frame and the snapshot)
end       "TKIX" count=61 (1200, 0x1BC) (1230, …) …   then trailer: index_offset, chunks=1812, crc, "TKND"
```

Size: 60.4 s × 43,520 B/s = 2,628,608 B, less the 3 s with no `Voice` or `Cue` (3 × 12,800 = 38,400 B), plus the
header (~440 B) and index (8 + 61 × 16 = 984 B) and trailer (24 B): ~2,591,656 B, about 1.7% of the 151 MB video.
The take manifest reads `"input": { "path": "…replay001.input", "major": 1, "minor": 0, "chunks": 1812, "bytes":
2591656, "withheld": [{ "voice": "mic", "from_s": 12.5, "to_s": 15.5, "cause": "muted" }], "event_refs": 0 }`.
`omuzaki render take …replay001 --mode as_performed --size 3840x2160 --camera cam_b` (SH-05) re-renders it at 4K from
another camera with the same blinks and mouth; the muted 3 s show the tracked mouth only, as the stream did.

## 7. Sources

The issues, docs and ADRs under **Builds on**, plus spec v3 §8, §10, §25, §26; the
code read: `omuzaki-anim-api` (`frame.rs`, `blend.rs`, `life.rs`), `omuzaki-core/src/rng.rs`, `omuzaki-ops-api/src/runner.rs`,
`omuzaki-history-api/src/{entry,snapshot}.rs`. CRC-32 (IEEE) as `crc32fast` implements it: unverified (unreachable from
the build box, 2026-10-13).
