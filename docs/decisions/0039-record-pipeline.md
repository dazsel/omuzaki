# 0039: Record and replay with LGPL FFmpeg in an encoder worker, encoded packets in a metered ring, saves by stream copy (2026-10-13)

**Status:** accepted (2026-10-13). **Context:** BR-17,
BR-18, BR-23; spec v3 §9, §10, §25, §26 (SH-01), §40.3 (FFmpeg licence, codec patents); the GPU lane, live actions, `omuzaki-budget` and the live-show supervisor;
`spikes/broadcast-out/README.md` §5; `docs/design/audio-api.md` §4 and §10. Design: `docs/design/record.md`.

## Context

BR-17 and BR-18 had no design, no API crate and no task. The audio side was fixed (audio-api §10: program and per-bus
taps, `EngineClock`, gaps filled with silence), and the broadcast spike sketched an `Output` trait. Five facts shape
the answer:
- Raw 1080p30 RGBA is 249 MB/s: 15 GB a minute, 75 GB for the 5-minute replay maximum. Only encoded packets fit.
- Intra-only alpha codecs (ProRes 4444, PNG, FFV1) cost 2.8 to 4.9 GB a minute; VP9 with alpha costs about 0.25 GB.
- The render thread must never wait on an encoder or a disk (§10), and BR-23 wants encoders as workers that survive
  `kill -9` without dropping the stream.
- GPL and nonfree encoders (`libx264`, `libx265`, `fdk-aac`) are banned; H.264, HEVC, AAC and ProRes need legal review
  questions.
- Mute is privacy (audio-api §4.2): muted and private audio must never reach a file.

## Decision

1. **Encoding uses LGPL FFmpeg as shared libraries** through `ffmpeg-sys-next` (our own `bindgen` layer if legal review
   rejects WTFPL), behind the `ffmpeg` feature. Not a spawned `ffmpeg` process (no packets, no device textures, a
   249 MB/s pipe copy) and not GStreamer (per-plugin licences). The banned encoders are excluded by the build and
   refused again by `probe()`. The default record format is WebM VP9 alpha; ProRes 4444 (`prores_ks`, not licensed
   by Apple) and hardware H.264 are opt-in until legal review answers, and the PNG sequence is the fallback without
   FFmpeg. The default changes to ProRes 4444 if legal review clears it.
2. **One `Output` trait** in the new `omuzaki-stream-api`, shared by the recorder, the replay buffer and the local
   lane's Spout, NDI and virtual cameras. `submit` copies into a pre-allocated slot and returns; a full queue repeats
   the previous frame in the file, counts it and tells the person, never silently and never by stalling the renderer.
   Encoder load feeds `omuzaki-budget`, which may lower render quality but never an output's size or frame rate.
3. **The replay ring stores encoded packets**, with their side data (VP9's alpha plane travels as BlockAdditional side
   data), with 1 s keyframes: VP9 alpha by default (about 4.1 MB/s with audio),
   hardware H.264 over a green key as the option. Recording and replay share one encode only when codec, size, rate,
   alpha and feed are equal.
4. **The cap counts every byte kept alive** (ring plus a save's pins, side data included) through one byte meter,
   memory or disk, and capacity is admitted (evict, then reserve) before a payload is created, so the meter never
   passes the cap even for an instant; the
   buffer holds `min(length, cap ÷ rate)` and says so. Defaults: off, 60 s, 1 GiB, memory; "always-on" (BR-17) means continuous once enabled.
5. **A save fixes its window within one frame period** and completes the file in the background by stream copy; a clip
   may start up to one keyframe interval early, never late. The strict "file in 1 s" reading is rejected: it would tie
   correctness to the person's disk speed (246 MB/s for VP9, 2.8 GB/s for ProRes).
6. **Every format survives a crash readable:** PNG temp-then-rename, fragmented MOV and MP4, WebM clusters, WAV headers
   rewritten each second; a disk guard stops cleanly before the disk is full; an encoder restart continues in the same
   file when codec parameters match, otherwise rolls to `_part2`.
7. **Audio is PCM:** f32 WAV sidecars for WebM, PNG and MP4, s24 streams inside the opt-in MOV; never AAC unless legal review clears
   it. Muted and private runs are written as silence and logged, their samples never copied.
8. **SH-01 owns the take format** and its design comes first; the ring is generic over chunk kinds, so the input track
   is a later producer, and BR-17 stays open until it lands.
9. **Alpha converts once per need in the encoder worker** with exact integer formulas (the CPU reference that a GPU
   pass must match); recordings include overlays by default, with a per-output clean feed.
10. **`record.*` ops are live actions** (empty patch, `Undo::None`) under scope `perform`; queries need `read`; no op
    takes a path. Folders and caps are `Machine`, `AgentWrite::Never`; `replay.enabled` is `Confirm`.
11. **The encoder is a supervised worker;** the ring, the take writers and the muxers live in the output host and
    outlive it.

## Consequences

- `omuzaki-stream-api` and `omuzaki-stream` are created by the stream tasks; the design's contract changes (extradata,
  `request_keyframe`, `keyint`, a registered output kind, `ByteMeter`, `OutputStatus` load) go into the stream-api task.
- The output host links `libavformat` for stream-copy muxing; codecs stay in the worker.
- The read-back wait of the CPU lane stays on the render thread; it is measured, not hidden.
- 10-bit ProRes carries 8-bit data until the renderer offers a straight high-precision resolve output.
- Legal review decides: the WTFPL bindings, ProRes encoding through `prores_ks`, H.264 hardware encoding, AAC.
- Follow-ups: an SH-01 design and the input track, a remote encoder adapter over channels, output pressure in
  `omuzaki-budget`.
