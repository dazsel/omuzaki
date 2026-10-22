---
title: "The track.face budget: which runtime meets 5 ms p95 (2026-10-22)"
summary: "LiteRT with XNNPACK on MediaPipe's official TFLite graphs runs track_roi twice as fast as ONNX Runtime; the deciding run is on R (#903)"
area: research
---

# The `track.face` budget: which runtime meets 5 ms p95 (#903)

**Question.** Spec §10 budgets `track.face` at 5 ms p95 on `R`, from camera frame in to channels out. One-thread ONNX
Runtime misses it by a wide margin. Which inference path meets it, and at what CPU cost?

**Answer so far.** LiteRT with XNNPACK (the `tflite` runtime, ADR 0061), running MediaPipe's own TFLite graphs from the
official `face_landmarker.task`, is twice as fast as ONNX Runtime on the same machine. At two intra-op threads it
is the only measured path likely to meet 5 ms p95 on `R`. The deciding numbers on `R` are still to come.

## Measured

`track_roi` on a 640×480 frame, the steady state of the tracker: landmarks, blendshapes, smoothing and the head-pose
fit. The detector runs only when a face is new, so it is left out. Synthetic gradient frame; 1000 timed calls after 20
warm-up calls. Bench: `crates/omuzaki-test-track/benches/track.rs`.

| Runtime | Graphs | Threads | Machine | Mean | p95 |
|---|---|---|---|---|---|
| ONNX Runtime (CPU EP) | converted ONNX (`tools/models/mediapipe-face.sh`) | 1 | cloud VM, 4 vCPU | 22.0 ms | 27.3 ms |
| LiteRT 2.3.0 + XNNPACK | official TFLite, from the `.task` | 1 | the same cloud VM | 11.6 ms | 13.0 ms |
| LiteRT 2.3.0 + XNNPACK | official TFLite, from the `.task` | 2 | the same cloud VM | 8.7 ms | 10.5 ms |

The ONNX row is #311's measurement on the same kind of VM. The LiteRT rows used the linux-x64 archive of the
`runtime-litert.yml` run on `main` (2026-10-21), checked against that run's SHA-256.

Numbers from earlier work on `R` (`spikes/webcam-track/results/model-bench.txt`), for the landmark graph alone:
ONNX Runtime at one thread took 10 to 12 ms mean (about 19 ms p95), and TFLite with XNNPACK took 3.4 to 6.8 ms.

## Reading

- **The runtime is the lever.** The same networks run twice as fast through XNNPACK as through ONNX Runtime's default
  CPU kernels, on the same machine, at the same thread count. That matches what the spike saw on `R`.
- **Scaling to `R`.** On the cloud VM, ONNX at one thread costs about twice what it costs on `R`. If LiteRT scales the
  same way, `R` would see about 6 ms p95 at one thread and about 5 ms at two. That is at the budget, not under it, so
  `R` has to be measured, not estimated. The probe `track_roi_timing` exists for exactly this run.
- **CPU cost.** Two threads cut the mean by a quarter and use up to two cores for about 9 ms of each 33 ms camera
  frame. That is roughly half of one core on average; it counts toward `cpu.total`.
- **The model files.** The tracker loads the published `.task` directly (`omuzaki_track::task::unpack`): it is pinned
  by URL, SHA-256 and size, and each member is pinned too. No conversion step and no TensorFlow toolchain are needed.
  The ONNX folder stays available for the `onnx` runtime.
- **One fix was needed for this.** The scan used to refuse the landmark graph because its input carries an *empty*
  quantization table, which the TFLite converter writes on float ports. The scan now refuses only a table that
  actually holds quantization (min, max, scale, zero point or details). The test in `omuzaki-ai-litert/tests/scan.rs`
  covers both cases.

## Recommendation

1. Make `tflite` the face tracker's default runtime, at two threads, with the graphs from the official `.task`.
   Keep `onnx` as the fallback, for a machine where LiteRT cannot load.
2. Measure on `R` (owner run): `track_roi_timing` with LiteRT at one and two threads, at a 30 fps camera. Record
   mean, p95 and CPU %.
3. If `R` misses at two threads, the next levers, in order:
   - XNNPACK's fp16 inference, on CPUs that support it (the graphs are float16-weighted already);
   - running blendshapes on alternate frames, with the landmarks every frame;
   - moving the warp and the head-pose fit off the hot path (about 2 ms of the cloud VM's total).
4. Ship LiteRT to people: publishing the archive is #1748, which waits for the owner's decision on release hosting.
   Until then, the `tflite` path runs only where the CI archive is supplied.

## Licences

Nothing new. LiteRT and its dependencies are in `docs/licensing/deps/litert*.toml`. The `.task` is
`mediapipe-face-landmarker.toml` (Apache-2.0 per the model cards, conditions recorded there).
