---
title: "`omuzaki-track`: ROI reuse and frame skipping"
summary: "how the face tracker fits the per-frame budget: MediaPipe's tracking mode as prior art, the timed re-detection, the measured motion test that skips a frame, the bound on consecutive skips and the knobs."
area: design
---

# `omuzaki-track`: ROI reuse and frame skipping

**Status:** proposed (2026-10-15). **Crate:** `crates/omuzaki-track` (`src/pipeline.rs`, `src/pipeline/reuse.rs`).
**Task:** #1741. **Builds on:** ADR 0061 and `docs/design/litert-runtime.md` section 6.3 (a full pass every frame does
not fit `cpu.total`), `docs/design/track-api.md` section 9 (budgets).

## 1. The problem

A full video-mode pass is the landmark graph (84% of it), the blendshape graph, the crop warp and the pose fit. At one
thread it costs more than the 8 ms of CPU per frame that `cpu.total` allows at 30 fps, even at MediaPipe's own speed. The
bar is to match MediaPipe, then beat it with what we add around the models. Matching it was done earlier: video mode
already reuses the ROI. This note is the part that beats it: run the passes on fewer frames, without the result
getting old or wrong.

## 2. Prior art: MediaPipe's face landmarker in video mode

(Apache-2.0; `mediapipe/tasks/cc/vision/face_landmarker/`, read on 2026-10-15.)

- **Reusing the ROI.** In stream (video) mode `face_landmarker_graph.cc` feeds the previous frame's face rectangles
  back with a `PreviousLoopbackCalculator`. A `CollectionHasMinSizeCalculator` with `min_size = num_faces` says whether
  the last frame found enough faces, and `DisallowIf` gates the detector's input with it: the detector is skipped
  "when we successfully track the faces from the last frame". Otherwise (and always in image mode) it runs.
- **The ROI from landmarks.** `face_landmarks_detector_graph.cc` turns the 478 landmarks into a detection, then a rect
  with `DetectionsToRectsCalculator` (rotation from landmarks 33 to 263 so the eyes are level, target angle 0), then
  `RectTransformationCalculator` with scale 1.5 and `square_long`. `next_frame_rect` in `landmarks.rs` is that port.
- **When it re-detects.** The presence logit goes through a sigmoid and a `ThresholdingCalculator`; the boolean gates
  the landmarks and the next ROI with `AllowIf`. A face below the threshold produces no ROI for the next frame, so the
  next frame runs the detector. `FaceTrackerConfig::min_presence` is that threshold (0.5). The tracked rects are also
  associated with the detector's by an `AssociationCalculator` at `min_tracking_confidence`.
- **What it does not do.** It runs the landmark graph on every frame the face is tracked. It never re-runs the detector
  while the face is tracked, so a second face or a ROI that has drifted onto something else is found only after the
  face is lost. And it has no notion of a frame that does not need a pass.

## 3. The scheme

Video mode only. Three rules, applied in this order to each frame:

1. **No tracked ROI** (first frame, or the last frame lost the face): run the detector, then the full pass.
2. **Re-detect** when the last detection is `redetect_every` frames old (default 60, two seconds at 30 fps): run the
   detector, then the full pass in the ROI it found. If it finds nothing, the tracked ROI is used: a detector miss on a
   face the landmarks still follow is not a lost face. Low confidence is MediaPipe's rule above and stays: a presence
   under `min_presence` drops the face and the next frame detects.
3. **Skip** the frame when all of these hold, and otherwise run the full pass in the tracked ROI:
   - the last full pass's presence was at least `skip_presence` (0.9): only a confident result is repeated;
   - fewer than `max_skips` (2) frames in a row have been skipped;
   - the ROI shows no motion (section 4).

A skipped frame returns the last full pass's `FaceSample` unchanged and runs no graph and no smoothing: the smoothing
filter keeps its state and carries on at the next full pass with the elapsed time, as it does for any gap.

## 4. The motion test

The test compares the ROI as it looks now with how it looked on the last full pass. It uses pixels, not landmarks, so
it costs no model run and sees what the landmarks would have measured, a turn, a nod, a blink or a mouth opening.

- The signature is the luma on a 16 x 16 lattice through the tracked ROI (rotation included), each point the mean of
  the 2 x 2 pixels there. That is 256 bytes in a fixed array and about 1,000 pixel reads, a small fraction of one millisecond (`benches/pipeline.rs`, `video_reuse`).
- The reference signature is taken on the last full pass, from the frame that pass was run on, in the ROI that pass gave
  the next frame. It is **not** the previous frame's: a drift of a fraction of the threshold per frame adds up against
  the reference and ends the skips, so slow motion cannot hide.
- Two measures, both fractions of full scale, both of the absolute difference: the **mean** over the lattice
  (`skip_mean`, 0.02), and the largest **block mean** over the sixteen 4 x 4 blocks (`skip_peak`, 0.04). The mean
  catches a turn or a lighting change; the peak catches a local change the mean averages away (a mouth opening is a few
  percent of the ROI). A frame is skipped only below both.
- The thresholds sit above the noise and below any motion that matters. Sensor noise of a few levels per pixel, halved
  by the 2 x 2 average and averaged again over 256 points, measures well under 1% (the tests use +-2 levels of
  noise on every frame and still skip). Noise too strong to stay under the thresholds just means no skips, which is
  the safe direction. A change of one pixel of a high-contrast edge in the ROI is already over the mean threshold; the tests
  pin a fast turn (36 levels a frame), slow drift (3 levels a frame) and a 24 x 24 pixel patch.
- The ROI is the face box grown 1.5 times, so a turning head's edge, hair and ears are inside it.

All of it is integer arithmetic on the frame, and the thresholds are compared as `f32` against integer sums, so the
decisions are deterministic for a frame sequence.

## 5. The bound

At most `max_skips` frames in a row are skipped, so a returned result is never older than `max_skips` frames (66 ms at
30 fps with the default 2), and a face that moves is seen on the first frame whose ROI differs by more than the
thresholds, not later. The cost of the bound is that a still face is passed on one frame in `max_skips + 1`: the
scheme saves at most two thirds of the passes. A larger bound saves more and lets a result age longer; it is a field on
the tracker's config (`ReuseConfig::max_skips`), not a contract.

`redetect_every` bounds the other drift: a ROI that is tracking something other than the face is replaced within 60
frames. `ReuseConfig::OFF` gives MediaPipe's own behaviour: no skips and no timed detection.

## 6. What it does not change

- `omuzaki-track-api` and `FaceSample` stay as they are. A skipped frame's `StageTimings` are all zero, which is how a
  caller (and the timing probe, #1731) tells a skipped frame from a pass.
- Image mode never skips.
- `track_roi` (the parity and model-check hook) always runs the full pass and ends any run of skips.
- `FaceTracker::track` still allocates nothing after warm-up, skipped frames included (`tests/alloc.rs`).

## 7. Knobs

| Field of `ReuseConfig` | Default | Meaning |
|---|---|---|
| `enabled` | on | off is MediaPipe's video mode |
| `redetect_every` | 60 | frames between timed detections; 0 never |
| `max_skips` | 2 | most frames skipped in a row |
| `skip_mean` | 0.02 | mean luma change of the ROI under which a frame may be skipped |
| `skip_peak` | 0.04 | largest block change under which a frame may be skipped |
| `skip_presence` | 0.9 | least presence of the last pass for a skip |

## 8. Measured

The numbers, measured with the real graphs on the session's own box at 640 x 480 and one thread, are in the pull request
that added this (#1741); the reference-machine number comes from the timing probe (#1731, #1734) after merge. They are
relative: the same frames through the tracker with the scheme off and on, one after the other on every frame.
Skipping lowers the mean and the p50 on a still or slowly moving head; it cannot lower the p95, which is a full pass in
both cases, so the tail is the runtime's work (ADR 0061), and a skip-heavy sequence is where `cpu.total`'s mean is met.
