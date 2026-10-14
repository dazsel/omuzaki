# webcam-track: MediaPipe face tracking in Rust (spike)

Can the models behind MediaPipe's Face Landmarker run in Rust under `ort`, and do they give the same answers as the JS tracker? **Yes, to within the noise between two MediaPipe builds, at about the cost of MediaPipe's own native path.** The numbers and the recommendation for `omuzaki-track` are under [Findings](#findings). Where each model file and runtime came from, with hashes and licences, is in [`models/PROVENANCE.md`](models/PROVENANCE.md).

This is a spike in its own workspace (like `spikes/toon-viewport`), so `ort` and `nalgebra` stay out of the main workspace. The library is a port of MediaPipe's own C++ graphs; every module names the calculator it follows.

| Module | What it is | MediaPipe source it follows |
|---|---|---|
| `frame` | RGB frame, rotated-ROI bilinear warp into a model tensor | `ImageToTensorCalculator` (OpenCV path) |
| `roi` | rotated rectangles, letterbox padding, ROI to frame maths | `RectTransformationCalculator`, `DetectionsToRectsCalculator`, `image_to_tensor_utils` |
| `detector` | BlazeFace short range: anchors, decode, weighted NMS, face ROI | `face_detector_graph.cc`, `SsdAnchorsCalculator`, `NonMaxSuppressionCalculator` |
| `landmarks` | FaceMesh V2: 478 landmarks, projection, next-frame ROI | `face_landmarks_detector_graph.cc` |
| `blendshapes` | Blendshape V2: 146 landmarks to 51 ARKit coefficients | `face_blendshapes_graph.cc` |
| `metadata`, `geometry` | head pose: canonical face from the bundle, Procrustes fits | `face_geometry` pipeline, `procrustes_solver.cc` |
| `smoothing` | One-Euro landmark filter (video mode) | `LandmarksSmoothingCalculator` |
| `pipeline` | the tracker: image and video modes, per-stage timings | `face_landmarker_graph.cc` |
| `runtime` | ONNX Runtime session setup (CPU or CUDA) | |

## Run it

1. Get the models and runtime (once). Details and hashes are in `models/PROVENANCE.md`.

```sh
# a throwaway venv with TensorFlow, only to convert the models (dev time)
uv venv $TMPDIR/scratch/webcam-track/venv
uv pip install --python $TMPDIR/scratch/webcam-track/venv/bin/python "tensorflow-cpu==2.16.2" "tf2onnx==1.16.1" "onnx==1.16.2" "numpy<2" onnxruntime "protobuf<5"
curl -L -o face_landmarker.task https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task
$TMPDIR/scratch/webcam-track/venv/bin/python -I tools/convert_models.py --task face_landmarker.task --out $OMUZAKI_ASSETS/models/mediapipe-face
# ONNX Runtime, Microsoft's release build (CPU); the CUDA build is onnxruntime-linux-x64-gpu_cuda12-1.31.0.tgz
curl -L https://github.com/microsoft/onnxruntime/releases/download/v1.31.0/onnxruntime-linux-x64-1.31.0.tgz | tar xz
```

2. Build with `cargo build --release` (it puts the build in the worktree's `target/`) and track a directory of PNG frames:

```sh
target/release/track --ort-lib onnxruntime-linux-x64-1.31.0/lib/libonnxruntime.so.1.31.0 \
    --models $OMUZAKI_ASSETS/models/mediapipe-face \
    --input $OMUZAKI_ASSETS/datasets/synthface/clip01 --out out.json --mode video --landmarks
```

`out.json` has the shape of the earlier web prototype's `track_face.py` output (`names`, `frames[{t, found, bs, m}]`) plus landmarks, the ROI used, per-frame time and a latency summary. `--backend cuda` needs the CUDA build of the runtime plus cudart 12.4+, cuBLAS 12 and cuDNN 9 on `LD_LIBRARY_PATH`, and goes through `--gpu`. `model_bench` times the three networks alone.

3. Compare and benchmark:

| Tool | Does |
|---|---|
| `tools/run_parity.sh` | every accuracy number below: Rust against the Python and JS trackers on both inputs, comparison tables to a results folder |
| `tools/ref_mediapipe.py` | the Python MediaPipe reference over PNG frames (image or video mode; same settings as `track_face.py`) |
| `tools/js_tracker_bench.mjs` | the earlier web prototype's JS tracker (MediaPipe 0.10.14) in headless Chromium |
| `tools/compare.py` | per-channel blendshape, pose and landmark errors between two result files |
| `tools/bench_matrix.py` | interleaved latency and CPU of every variant, paced at 30 fps |
| `tools/bench_models.sh`, `tools/bench_tflite.py` | the three networks alone, ONNX Runtime against TFLite/XNNPACK |
| `tools/render_clip.py` | renders the coherent synthetic clip (Blender) |

`results/` holds the comparison tables and the benchmark data behind the numbers below.

## Tests

`cargo test` runs 30 unit tests (ROI maths, anchors, NMS, warp rounding, protobuf reader, Procrustes and pose recovery, smoothing, statistics). `tests/pipeline.rs` runs the real models when `WEBCAM_TRACK_ORT_LIB`, `WEBCAM_TRACK_MODELS` and optionally `WEBCAM_TRACK_FRAMES` (a directory of face PNGs) are set, and skips itself otherwise. `cargo clippy --all-targets -- -D warnings`, `cargo fmt --check` and `cargo deny check` (the repo's `deny.toml`) pass.

## Findings

### Accuracy

**Inputs.** All synthetic (ICT-FaceKit Light renders; never footage of a real person or private test material): `spike01`, 200 independent identities with extreme poses and expressions (256 x 256), and `clip01`, 90 frames at 30 fps of one identity with scripted expressions, head motion and in-frame movement (480 x 480). **References.** Python MediaPipe with `track_face.py`'s settings (on the clip it matches `track_face.py` itself to 0.0001) and the earlier web prototype's JS tracker. Every frame was found by every tracker, and frames are compared one by one. Blendshapes are scores in 0 to 1; angles are degrees from the pose matrix (`track_face.py`'s convention); translation is centimetres.

| Comparison (mean absolute error) | Frames | Blendshapes (p99 / max) | Yaw, pitch, roll | Translation x, y, z | Landmarks |
|---|---|---|---|---|---|
| **Rust vs Python**, image mode | 200 stills | **0.00024** (0.0026 / 0.0096) | 0.008, 0.009, 0.005 | 0.0005, 0.0007, 0.0016 | 0.012 px of 256 |
| **Rust vs Python**, image mode | 90 clip | **0.00017** (0.0017 / 0.0056) | 0.006, 0.007, 0.003 | 0.0004, 0.0005, 0.0015 | 0.011 px of 480 |
| **Rust vs `track_face.py`**, video mode | 90 clip | **0.00036** (0.0039 / 0.0094) | 0.014, 0.016, 0.008 | 0.0011, 0.0014, 0.0037 | n/a |
| **Rust vs the JS tracker**, video mode | 90 clip | **0.00038** (0.0040 / 0.0121) | 0.016, 0.014, 0.007 | 0.0011, 0.0016, 0.0038 | n/a |
| for scale: JS tracker vs `track_face.py` | 90 clip | 0.00036 (0.0042 / 0.0090) | 0.013, 0.015, 0.007 | 0.0009, 0.0012, 0.0036 | n/a |

In video mode the Rust tracker is as close to either reference as the references are to each other (two MediaPipe builds, 0.10.14 against 1.1.0): what remains is the noise the One-Euro filter carries along. The largest per-channel errors are the brows (MAE 0.001, worst single value 0.0096). On the 200 stills every channel has r >= 0.9993 (median 0.99998). The full 51-channel tables are in `results/`.

**What it took to match.** Each item is easy to miss and measurably wrong if missed; the stills error fell from 0.0027 to 0.00024 across the first two.

1. The landmark crop **replicates edge pixels**; only the detector pads with black (`ImageToTensor` defaults to `BORDER_REPLICATE`). Matters for close faces whose ROI passes the frame edge. Stills: 0.0019 to 0.00024.
2. OpenCV's warp **snaps positions to 1/32 pixel and rounds to whole 8-bit levels**. Interpolating in floats leaves 0.25 grey levels of noise per pixel, enough to move landmarks 0.1 px. Stills: 0.0027 to 0.0019; the clip in video mode: 0.00097 to 0.00036.
3. The blendshape network takes landmark x and y **in frame pixels**, and in video mode reads the **smoothed** landmarks (as the pose fit does); the next frame's ROI comes from the unsmoothed ones.
4. Smoothing exists only in video mode: a One-Euro filter (min cutoff 0.05, beta 80, derivative cutoff 1) on pixel coordinates with speeds scaled by face size; a first timestamp of exactly 0 never updates the measured frame rate.
5. The detector's merge threshold is the Tasks default 0.5, not BlazeFace's 0.3.
6. Head pose needs the canonical face mesh and per-landmark fit weights that ship inside the `.task` bundle (a 19 KB protobuf), a 63 degree virtual camera and two scale-recovery passes before the final fit.
7. Tracking: the detector is skipped while the previous presence score stays above the threshold; when it drops, that frame reports no face and the next one runs the detector.

### Latency and CPU

`tools/bench_matrix.py` runs every variant in turn, five rounds (A B C A B C ...), on the 90-frame clip in video mode, **paced at 30 fps** so CPU use is what a live camera would cost (process CPU time over wall time, as a percentage of one core). The machine is shared with other agents' builds and its load average swung from 3 to 25 during the run, so the table gives the median over rounds, with the spread.

| Variant (video mode, 30 fps) | Mean ms | p50 | p95 | CPU, % of one core | Round means, min to max |
|---|---|---|---|---|---|
| **Rust, `ort` CPU, 1 thread (default)** | **8.31** | 7.95 | 10.38 | **25** | 7.7 to 13.7 |
| Rust, `ort` CPU, 1 thread, workers sleep | 11.09 | 9.92 | 18.19 | 33 | 7.5 to 15.9 |
| Rust, `ort` CPU, 2 threads, spin | 6.08 | 6.02 | 6.75 | 223 | 5.8 to 11.3 |
| Rust, `ort` CPU, 4 threads, spin | 6.64 | 6.39 | 8.51 | 633 | 5.8 to 14.3 |
| Rust, `ort` CPU, 2 threads, workers sleep | 13.47 | 11.01 | 24.30 | 53 | 8.9 to 19.8 |
| Rust, `ort` CUDA (RTX 4090) | 31.71 | 33.96 | 42.95 | 87 | 22.2 to 34.8 |
| Python MediaPipe (TFLite + XNNPACK) | 7.35 | 6.48 | 12.22 | 24 | 5.8 to 12.5 |
| JS tracker (WASM, headless Chromium, CPU) | 15.25 | 13.10 | 24.50 | n/a | 11.2 to 24.3 |

A second full run in a busier hour (load 7 to 23) gave the same order: Rust default 9.2 ms, Python 8.3 ms, JS 17.6 ms, CUDA 28.6 ms.

**Where the time goes** (default configuration, frame tracked from the previous one): landmark crop 1.55 ms, landmark network 5.9 ms, blendshape network 0.82 ms, smoothing 0.01 ms, pose fit 0.02 ms. A frame where the detector runs adds about 1.4 ms (crop 0.35, network 1.06). The landmark network is 70% of the frame.

- **One thread is enough.** In a tight loop ONNX Runtime scales with spinning workers (landmark network 5.4 ms on one thread, 3.7 ms on two), but a paced 30 fps stream lets the workers sleep between frames: two spinning threads then cost 223% of a core and four cost 633% for 2 ms saved. With workers asleep, more threads make the network slower (`results/model-bench.txt`, under load: 10.7, 13.4 and 17.0 ms on 1, 2 and 4 threads). One thread with spinning also has the best p95 (10.4 against 18.2 ms), so it is `RuntimeOptions::default()`.
- **The PReLU rewrite is exact and helps when the machine is busy.** ONNX Runtime's CPU provider converts convolutions to a blocked layout and PReLU (69 in the landmark network) is not layout aware, so each costs two conversions. `convert_models.py` rewrites each as `relu(x) + slope * (x - relu(x))`. Interleaved at machine load 26 the network took 9.3 ms against 15.0 ms (-38%); on a quiet machine (load 6) both took 5.0 ms. A desktop running a game and OBS is the busy case, so it stays.
- **ONNX Runtime against TFLite/XNNPACK, per network** (`results/model-bench.txt`, medians of three rounds, under load): landmarks 12.0 ms (ORT, 1 thread) against 6.8 ms (TFLite), and 8.8 against 4.3 ms on two threads. XNNPACK is 1.8 to 2.6 times faster at equal threads and scales better; the whole-pipeline gap is small (8.3 against 7.4 ms) because MediaPipe spends its remaining time elsewhere (Python call, graph scheduling, OpenCV).
- **CUDA is not worth it for these networks.** They are small (the landmark network is 5 MB and 223 operators before the PReLU rewrite, 430 after), so each frame is hundreds of tiny kernel launches that need CPU time to be issued. On a quiet machine in a tight loop the three networks took 1.0, 3.5 and 1.2 ms; paced at 30 fps with the CPU busy the pipeline took 25 to 42 ms (plain graph 25 to 33 ms, rewritten graph 26 to 42 ms, three paired runs at load 27). The GPU also idles at 240 MHz between frames. It needs a 450 MB runtime and proprietary CUDA and cuDNN libraries. CUDA graphs or TensorRT might help; neither was tried.
- **The crop warp is the next cost** (1.55 ms for one 256 x 256 bilinear sample); an integer fast path for interior ROIs should be several times faster.

**Not measured:** DirectML (Windows) and CoreML (macOS) cannot run on this Linux host, and headless Chromium has no GPU, so the JS tracker's GPU delegate did not run.

### Recommendation for `omuzaki-track`

1. **Go Rust-native for the webcam tier.** The port is small, matches both existing trackers, costs a quarter of a core and takes the browser out of the tracking path. Decision 0004 (parity first) is met without the JS tracker.
2. **Use `ort` with `load-dynamic` and an ONNX Runtime build we choose**, never Pyke's prebuilt binaries (the spike used Microsoft's official release; for shipping, build it ourselves or ship Microsoft's with its notices, per the register). Build `ort` with `default-features = false` so nothing is downloaded. `ort` is still a release candidate (2.0.0-rc.13): pin it.
3. **Default to the CPU provider, one intra-op thread, workers spinning.** Offer DirectML (Windows) and CoreML (macOS) as opt-in once measured on those systems; do not ship CUDA.
4. **Keep the three networks as build-time assets, not code.** `convert_models.py` is the pipeline (TFLite to ONNX, batch pinned, PReLU rewritten, numerics checked against TFLite); its output (7.4 MB) goes to the asset store with `manifest.json` (hashes, tool versions, licence). TensorFlow stays a dev-time dependency. Hashes are not stable across conversion runs, so the numeric check is the gate.
5. **Lift the modules as they are** into the crate (`roi`, `detector`, `landmarks`, `blendshapes`, `metadata`, `geometry`, `smoothing`, `pipeline`). API shape: `Tracker::track(&Frame, timestamp) -> Option<FaceSample { blendshapes: [f32; 51], pose: [f32; 16], landmarks }>` plus `reset()`, one tracker per camera on its own thread, output feeding the anim retarget stage; keep `Mode::Image` and `Mode::Video`.
6. **Keep MediaPipe's behaviour rather than re-deriving it, and make the parity harness the regression test:** a stored `track_face.py` JSON of a rendered clip, with the thresholds from the table above (blendshape MAE under 0.001, pose under 0.05 degrees).
7. **Before it ships:** pre-allocate the per-frame vectors in the pose fit (about ten small allocations a frame, 0.03 ms measured, but the no-allocation rule applies), add the integer crop fast path, and decide how the camera hands frames over (the spike reads PNGs).

### Limits

- **Synthetic mannequin faces only**, by the policy that keeps real-person footage and private test material out of anything shared. The networks and code path are the same for real faces, but parity on real footage is not shown.
- **The machine was loaded**: latencies swing 2x between rounds, so use the ordering and the interleaved medians, not single numbers. The 5.0 ms landmark network on a quiet machine suggests the pipeline would be near 6 ms there.
- **CUDA ran on borrowed libraries** (cudart 12.8, cuDNN 9.10 from an existing conda env), for measurement only.
- **Open licence items are unchanged:** the GHUM terms behind the blendshape network and Google's APIs terms. The three model cards say Apache-2.0.
