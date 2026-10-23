---
title: "MJPG decode without per-frame allocation (2026-10-22)"
summary: "Which JPEG decoder the webcam path should use for MJPG: zune-jpeg, jpeg-decoder or our own baseline decoder, measured for time, p99 and allocations at 480p, 720p and 1080p; we use our own, now in omuzaki-pixfmt."
area: research
---

# MJPG decode without per-frame allocation (2026-10-22)

> Research for #886, done together with #1851 (PF-01, spec v3 §10: no allocation in real-time loops). Measured on
> synthetic frames only, made by the repository's test encoder; no camera clip was used.

## Recommendation

Use our own baseline decoder for webcam MJPG, and keep zune-jpeg for every other JPEG. This has landed in
`omuzaki-pixfmt` (`MjpgDecoder`):

- **No allocations.** It allocates nothing per frame once it has seen a frame size: 0 per decode at 480p, 720p and
  1080p, where zune-jpeg makes 17 and jpeg-decoder thousands.
- **As fast as zune-jpeg.** It is level with zune-jpeg and about twice as fast as jpeg-decoder.
- **Matches libjpeg-turbo.** Its pixels match libjpeg-turbo's bit for bit (libjpeg's integer IDCT, fancy upsampling
  and colour maths).
- **Decodes what webcams send.** It decodes frames with no Huffman tables, as webcams send them. Neither library
  decodes those frames.
- **Licence and scope.** It adds no dependency. Its scope is narrow: 8-bit baseline, grey or YCbCr, at 4:4:4, 4:2:2
  or 4:2:0.

No follow-up task is needed. MJPG stays last in the webcam's format preference. It no longer allocates, but a 720p
decode still costs about 1.7 times an NV12 conversion, and it carries the camera's compression.

## 1. Candidates

| Decoder | Licence | Reuses buffers between frames | SIMD | Maintenance | Table-less MJPG |
|---|---|---|---|---|---|
| zune-jpeg 0.5.15 (current dependency) | MIT OR Apache-2.0 OR Zlib | No | AVX2 IDCT and colour (runtime detection) | Active: 0.5.16-rc3 published 2026-10-19 | No |
| zune-jpeg 0.5.16-rc3 | same | No | same | release candidate | not tested |
| jpeg-decoder 0.3.2 | MIT OR Apache-2.0 | No | SSSE3 paths on x86 | Last release 2025-06; `image` now decodes through zune-jpeg | No |
| Our baseline decoder (`omuzaki-pixfmt/src/mjpg/`) | the project's own | Yes: tables, planes and row buffers persist | SSE2 IDCT, upsampling and colour (x86-64 baseline, so no runtime detection); scalar elsewhere | ours | Yes |

**zune-jpeg 0.5.15:**
- `JpegDecoder::new` takes the stream by value, so every image builds a new decoder: its tables, its MCU row buffers
  and its component state.
- The 17 allocations per decode come from there. The pixels themselves go straight into our buffer through
  `decode_into`.

**zune-jpeg 0.5.16-rc3:**
- It adds incremental decoding (`decode_next_imcu_row`, `set_incremental_mode`) and plane output
  (`decode_into_planes`).
- `into_inner` gives the stream back, but a decoder still can't take a new image, so the per-image allocations stay.

**Frames without Huffman tables:**
- A UVC camera's MJPG frames usually carry no DHT segment. The decoder is expected to use the T.81 Annex K tables.
- Both libraries return an error on such a frame, at least without an `AVI1` APP0 marker.
- Ours fills table slots 0 and 1 with the Annex K tables whenever a frame leaves them out.
  `huffman::tests::the_standard_tables_are_the_ones_libjpeg_writes` checks those constants against the tables
  libjpeg-turbo writes.

**Our decoder:**
- **Header parsing.** It parses SOF0 and SOF1, DQT (8 and 16 bit), DHT, DRI and SOS. It handles restart markers.
- **Decoding:**
  - The bit reader adds eight bytes at once when none of them is 0xFF.
  - One table read decodes most AC coefficients, run and value together.
  - All-DC blocks skip the transform.
- **SSE2 paths.** The IDCT follows libjpeg-turbo's `jidctint-sse2`: 16-bit rows and `pmaddwd` rotations. The
  upsampling and the colour conversion split each 17-bit constant into a shift and a 16-bit multiply. Tests hold each
  SSE2 path to its scalar reference exactly:
  - 20,000 random blocks;
  - every Cb and Cr value at nine luma levels;
  - every row length.
- **Unsafe code.** The SSE2 code needs one `unsafe` call per entry point, for the `#[target_feature]` boundary. A
  compile-time assertion that SSE2 is enabled guards each one. SSE2 is part of the x86-64 baseline.
- **Fallbacks.** Anything outside its scope goes to zune-jpeg, and so does any frame it rejects. `fallbacks()` counts
  them.

## 2. Measurements

**Bench:** `cargo bench -p omuzaki-pixfmt --bench mjpg` (`crates/omuzaki-pixfmt/benches/mjpg.rs`). Each case decodes
four different frames in turn, 100 rounds after a warm-up pass. It counts allocations with a counting global
allocator, as the crate's zero-allocation tests do.

**Machine:** one thread of a 4-core cloud VM. Its timings are noisy: max and p99 move by a few milliseconds from run to
run, and p50 is the steady figure.

**Frames:**
- Synthetic, at quality 85: gradients, a soft disc, fine stripes and ±8 noise.
- About 54 KB a frame at 480p, 157 KB at 720p and 348 KB at 1080p, for 4:2:2.
- Our decoder gets the frames without Huffman tables. zune-jpeg and jpeg-decoder get the same frames with the tables
  written out, which decode to the same coefficients.

| Size and sampling | Our decoder: p50 / p99 | zune-jpeg 0.5.15: p50 / p99 | Allocations per decode (ours / zune-jpeg) |
|---|---|---|---|
| 640x480, 4:2:2 | 1.49 / 3.59 ms | 1.58 / 2.33 ms | 0 / 17 |
| 640x480, 4:2:0 | 1.36 / 1.84 ms | 1.38 / 1.69 ms | 0 / 17 |
| 1280x720, 4:2:2 | **4.32** / 5.13 ms | 4.57 / 5.45 ms | 0 / 17 |
| 1280x720, 4:2:0 | 4.02 / 6.65 ms | 4.13 / 6.42 ms | 0 / 17 |
| 1920x1080, 4:2:2 | 10.49 / 16.94 ms | 10.50 / 20.85 ms | 0 / 17 |
| 1920x1080, 4:2:0 | 9.30 / 15.74 ms | 9.31 / 13.99 ms | 0 / 17 |

**jpeg-decoder 0.3.2:**
- It was measured on the same frames in a scratch crate outside the repository, since it is not a dependency here. That
  scratch crate is the same harness with `jpeg-decoder = { version = "=0.3.2", default-features = false }`.
- In the same run, our decoder took 4.34 ms (p50) at 720p and 10.52 ms at 1080p.

| Size, 4:2:2 | jpeg-decoder: p50 / p99 | Allocations per decode |
|---|---|---|
| 640x480 | 4.17 / 6.36 ms | 2,207 |
| 1280x720 | 9.10 / 16.23 ms | 3,257 |
| 1920x1080 | 17.39 / 27.26 ms | 4,838 |

**Before and after:**
- **Our decoder's first version** was scalar, with planes for the whole frame. It took 8.3 ms p50 at 720p 4:2:2.
- **The SSE2 IDCT** cut its cost per block from about 110 ns to about 45 ns, measured by `idct::tests::speed`.
- **SSE2 colour conversion and upsampling, the word-at-a-time bit reader and the fused AC lookup** brought the decode
  to 4.3 ms.
- **The PF-01 budget** for MJPG is 8 ms at 720p (`benches/convert.rs`). The decoder is inside it at p50 and at p99.

**What else the tests prove:**
- **No allocations.** `tests/alloc.rs` decodes 720p and 1080p frames in 4:2:2 without tables and in 4:2:0 with
  restart markers. Two different pictures alternate. It counts 0 allocations after the first frame of each size, and
  0 when switching between the sizes. `omuzaki-track`'s `tests/webcam.rs` proves the same through `FrameConverter`.
- **Accuracy.** `tests/convert.rs` matches Pillow (libjpeg-turbo) exactly on the 4:4:4, 4:2:2 and 4:2:0 fixtures. The
  old path was within 3 levels.
- **Coverage.** `tests/mjpg.rs` covers:
  - every sampling at odd sizes from 1x1 to 320x240;
  - restart intervals of 1 and 3, with and without tables;
  - a progressive fixture that goes to zune-jpeg;
  - a frame cut at a restart marker, which is an error;
  - 256 corrupted frames that never panic and leave the decoder working.

## 3. What was not done

- **AVX2.** Running the IDCT and the colour conversion with AVX2 (with runtime detection) would cut a further third or
  so. It is not needed inside the budget.
- **Multi-buffer MJPG capture.** The Media Foundation camera's gather buffer is sized for multi-buffer MJPG samples at
  two bytes per pixel, which a valid frame can exceed (#1804, raised again on #1851). That code belongs to the Render
  area and is filed separately as #2333, so this note doesn't cover it.
