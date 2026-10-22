---
title: "MJPG decode allocations: measured, and why zune-jpeg stays"
summary: "zune-jpeg allocates 4 blocks (31 to 91 KiB) per MJPG decode, about 0.5 us of a 2 to 13 ms decode; the tail comes from the decode itself, so the decoder stays and no pooled adapter is needed"
area: research
---

# MJPG decode allocations (#886)

**Recommendation.** Keep zune-jpeg and keep `MjpgDecoder` as it is. A decode makes **4 allocations** (31 KiB at
640x480, 61 KiB at 720p, 91 KiB at 1080p), not about 17 as #886 assumed. Made and freed on their own they cost
**0.3 to 0.6 us**, under 0.01 % of a decode that takes 1.9 to 12.7 ms. The p99 tail (1.5 times the median) is the
decode's own variance on a shared machine, not the allocator. A buffer-reusing decoder could not move either number
in a way capture can see, so no follow-up task is filed. Revisit only if a profile on a reference machine shows
allocator time in the capture thread, or if zune-jpeg ships a scratch-reuse API in a stable release (none in 0.5.15).

**Bench:** `crates/omuzaki-pixfmt/benches/mjpg_jitter.rs`
(`cargo bench -p omuzaki-pixfmt --bench mjpg_jitter`).
**Machine:** one 4-core cloud VM, Linux, release build, nothing else running in the session. Not a reference machine:
read the times as relative, the allocation counts as exact.

## What was measured

The bench encodes one synthetic frame per size with the `image` crate's JPEG encoder at quality 90. The frame is a
smooth light falloff with rings and fine grain, so it has both the flat regions and the detail of a face in a room. It
is never a camera clip. `MjpgDecoder` (strict mode, RGB out, straight into the caller's buffer) then decodes it:

- 20 warm-up decodes, then 400 timed ones, each timed alone;
- every allocation during the timed loop counted by a global counting allocator (the zero-allocation tests use the same
  one), with the timing buffer reserved beforehand, so every count is the decoder's;
- "their cost": the same number of blocks of the same total size, allocated zeroed and freed, 2000 rounds. This is an
  upper bound on what reusing scratch buffers could save.

## Numbers

| size | JPEG | p50 | p99 | worst | allocations per decode | bytes per decode | their cost |
|---|---|---|---|---|---|---|---|
| 640x480 | 86 KiB | 1.91 ms | 2.97 ms | 3.61 ms | 4.0 | 31 KiB | 0.3 us |
| 1280x720 | 250 KiB | 5.63 ms | 8.36 ms | 10.54 ms | 4.0 | 61 KiB | 0.5 us |
| 1920x1080 | 552 KiB | 12.67 ms | 18.15 ms | 36.88 ms | 4.0 | 91 KiB | 0.6 us |

A second run gave the same counts and medians within 0.2 ms (1.77 / 5.49 / 12.70 ms). The worst single decodes moved
between runs (20.6 ms and 36.9 ms at 1080p), as preemption on a shared VM does; neither the allocator nor the decoder
explains a 3x outlier.

What the numbers say:

- **Allocation is not the jitter.** At 720p the allocations cost 0.5 us and the p99 sits 2.7 ms above the median. Even if
  every allocation landed in the tail, it would explain 0.02 % of it.
- **The counts are flat.** Four blocks per decode at every size, and their total grows with the width (31, 61, 91 KiB
  for 640, 1280, 1920 pixels across), not the area: row-sized scratch, not a frame. The pixels go straight into `out`.
- **The blocks stay under glibc's mmap threshold** (128 KiB by default), so they come from the heap's free lists, not
  from fresh pages: the cheap case, and the one this bench measured.
- **The budget.** PF-01 gives MJPG 8 ms at 720p. The median (5.6 ms) is inside it on this machine; the p99 (8.4 ms)
  is just over, on a VM without a dedicated core. A reference-machine run decides that, not this note.

## Candidates

| decoder | licence | reuses buffers between frames | SIMD | maintenance | verdict |
|---|---|---|---|---|---|
| zune-jpeg 0.5.15 (current) | MIT OR Apache-2.0 OR Zlib | no API for it; 4 small allocations per decode | yes (runtime-detected x86 paths) | active; a 0.5.16 release candidate is published | **keep** |
| jpeg-decoder 0.3.2 | MIT OR Apache-2.0 | no; `decode()` returns a fresh `Vec` per image, so it allocates the whole frame each time | limited | slower moving since the `image` crate moved its JPEG decoding to zune-jpeg | worse on the very axis #886 asks about |
| A pooled-buffer adapter over zune-jpeg | ours | would need zune-jpeg's internals; its public API (`new_with_options`, `decode_headers`, `decode_into`) takes no scratch | as zune-jpeg | ours to keep | nothing to save (0.5 us) |
| libjpeg-turbo through a binding crate | libjpeg-turbo itself is BSD-style (IJG, BSD-3, zlib); each binding has its own | a decompressor handle is reusable | yes | active | not worth a C dependency and a cross-compile step for 0.5 us |

Licences above are as each project declares them; every candidate allows commercial use, and a binding crate's own
licence would get its check if one were ever proposed. Nothing here is added, so no
licence-register row is needed.

## Why not switch anyway

The zero-allocation rule (spec §10) is about the hot loops whose frames must not stall: the tracker and the renderer.
MJPG decode runs on the capture thread, once per camera frame, and costs milliseconds; four heap blocks per frame
cannot stall it measurably, and the tracker's own zero-allocation test already excludes it (`MjpgDecoder` documents
the exception). A switch would trade a maintained pure-Rust decoder for either a slower allocating one or C code,
to save half a microsecond.

## If this is revisited

- Re-run the bench on a reference machine with a real 720p MJPG camera stream; keep frames synthetic in the repository.
- Check zune-jpeg's next stable release for a scratch-reuse or `reset`-style API; if one lands, `MjpgDecoder` already
  owns a decoder per camera and can hold the scratch without an API change.
