---
title: "0052: Takes record what entered the performance pipeline and what it resolved to, as one checksummed chunk per frame period, in `<stem>.input` (2026-10-13)"
summary: "Takes record what entered the performance pipeline and what it resolved to, as one checksummed chunk per frame period, in `<stem>.input`"
area: decisions
---

# 0052: Takes record what entered the performance pipeline and what it resolved to, as one checksummed chunk per frame period, in `<stem>.input` (2026-10-13)

**Status:** proposed (PR); accepted by the cloud lead when merged. **Context:** SH-01,
BR-17, BR-18, SH-05; spec v3 §8, §10, §25, §26; ADR 0039 decision 8; ADR 0009, ADR 0011;
`docs/design/record.md` D3 to D8; `docs/design/audio-api.md` §4.2, §5. Design: `docs/design/takes.md`.

## Context

ADR 0039 left the input track to SH-01: the replay ring takes one opaque `ChunkKind::Input` chunk per frame period, and
a saved clip writes them unchanged to `<stem>.input`. SH-01 asks that a take re-render later at higher quality, from a
new camera or with another character. Four facts shape the format:
- The life layer's blinks and saccades come from a private RNG state and timers, so raw inputs alone cannot reproduce
  the live performance; tracker and lip-sync models get updated, so their outputs must be kept, not recomputed.
- A replay clip starts at any video keyframe and the ring evicts whole keyframe groups, so the start state must travel
  in the group it belongs to.
- Everything on the frame thread must be allocation-free (spec §8); the ring admits a chunk's length before its bytes
  exist (ADR 0039 decision 4).
- Mute is privacy: muted and private audio never reaches a file, and visemes and loudness are derived from that audio.

## Decision

1. **What is kept:** the tracker frames the stage polled, audio-derived layers and cues with performance time, every
   journal entry in LSN order with its physical patch and the frame it took effect at (placeholders for `Mark`
   and `Session` keep LSNs contiguous) (ops, live actions, undo, redo and rollback; `cause` text
   dropped), the `PlayoutDelay` value, and per cast member
   the resolved `ChannelBuf` after the life layer (`Bus`). A JSON `State` (document version, scene, camera, cast,
   sources, voices, models, delay, the journal position, the muted or private runs open at that frame, and a `live` map filled by self-registered live-state providers) opens every group.
   Audio samples stay in the take's WAV sidecars and are never duplicated. Chunks are bounded: the whole State ≤ 16 KiB
   (bounded mandatory fields, providers over the cap named as omitted, a minimal State on overflow), events ≤ 32 KiB
   per chunk (the rest deferred in order, each applied at its `applied_frame`; completeness is judged against an acknowledged
   journal boundary per frame that covers entries still awaiting serialisation, and a `Watermark` record says how far
   a chunk is complete, since no fixed read-ahead bound holds), a stop or save finalises through `InputTap::tail` as
   several bounded `TAIL` chunks (a save copies the queue, a stop drains it), one entry
   over 16 KiB becomes a journal reference, and a full event queue never blocks the executor: it writes an
   `Incomplete` marker and the reader refuses to cross the hole.
2. **Framing:** a 32-byte chunk header (magic `TKCH`, major and minor version, flags with a must-understand byte, the
   video frame index, the tick time, the payload length, a CRC-32) and typed records (`kind`, `len`, body). Hot records
   are fixed little-endian layouts with floats stored as bits; cold records are serde JSON. The ring stores whole
   chunks; files store the same bytes.
3. **Versions:** minors are additive (new kinds skipped by length, bodies that grow at their end, new optional flags);
   a newer major or an unknown must-understand flag is refused with a hint; old majors keep their decoders.
4. **`<stem>.input`:** a header with its JSON and CRC, the chunks, then an index of group starts and a 24-byte trailer
   on a clean finish. Without a trailer a reader scans by length and CRC and stops at the first bad chunk, so a crash
   loses at most the last unflushed second. Size: ~43.5 KB/s at 30 fps for one performer, under ADR 0039's estimate.
5. **Privacy:** a muted or private run writes no visemes and no cues, only a `Withheld` marker; tracker frames and the
   bus, which viewers saw, are kept. Each State lists the runs open at its frame, so a clip cut inside a run still
   reports it, clamped to the clip. The input track is on by default with an off switch (`record.input`); deleting a
   take deletes its `.input` and releases its snapshot pin (a project decision).
6. **Re-rendering:** a take pins a project snapshot (`Trigger::Take`) at an exact journal position: a recording's at
   its start (entries replay forward), a replay save's at the save (entries invert backward, with the entries after
   the window's last frame in a closing `TAIL` chunk); the two never mix. Two modes:
   `as_performed` drives the character from the recorded bus (default; survives model updates and works with another
   character), and `re_perform` runs the raw inputs through today's retarget and life layer. Springs and physics settle
   fresh for 1 s.
7. **Crates:** `omuzaki-take-api` (types, reader contract, `register_take_state!`), `omuzaki-take` (lib: the one codec,
   writer, reader, `TakeReplay`), `omuzaki-take-mock` (scripted takes, independent decoder). `InputTap` and `FrameStamp`
   move to `omuzaki-stream-api`; `omuzaki-stage` produces; `omuzaki-stream` writes the files.

8. **The completeness protocol is fixed by five invariants** (a project decision), proved in code by a `loom`
   model test (planned), where every interleaving found in review is a named case: **(S)** `boundary(f)` is at or after every
   entry applied at or before `f`, and may cover entries of `f + 1`; **(L0)** once stop is requested and `in_flight`
   reads 0, the last frame's boundary closes with no new frame; **(L1)** stop reaches `Done` whenever, before the
   deadline, the tee drains and the serialiser writes and the TAIL drains through the last frame's boundary, and
   `Incomplete` otherwise (a project decision); **(L2)** the frame thread never blocks on the tee or on a
   save; **(D)** a save never drains the live queue. Findings on finer interleavings become model cases; a finding that
   shows an invariant wrong or missing reopens this record.

## Consequences

- `InputTap` signature changes to take a `FrameStamp` with `group_start`, gains `tail`, and lives in
  `omuzaki-stream-api`; its save writes the chunks unchanged inside the `.input` framing, then the TAIL chunks.
- `omuzaki-history-api` gains `Trigger::Take` (additive, non-exhaustive enum).
- `crc32fast` becomes a direct dependency (a register row; already present as a transitive one).
- Events over 16 KiB are recorded as references into the project journal, so such takes need the project to resolve
  them; the take report counts them.
- Re-renders are deterministic per build; across builds the performance is the same and pixels may differ.
- The `.input` file is a personal capture: publish and upload ops never include it unless the person names it.
