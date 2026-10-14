# Provenance of every model file and runtime the spike uses

Nothing in this table is committed: model files live in `$OMUZAKI_ASSETS/models/mediapipe-face`, runtimes are downloaded on demand. This page records where each came from, what licence covers it, and how to rebuild it.

## The model bundle

| Item | Value |
|---|---|
| Source | `https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task` (the URL the JS prototype's `tracker.js` uses as its CDN model) |
| Downloaded | 2026-10-12 04:40 UTC; object generation `1683136941916318`, `Last-Modified: Wed, 03 May 2023 18:02:21 GMT`, ETag `b0e7274907a1644404fef66b28dd6d85` |
| SHA-256 | `64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff` (3,758,596 bytes); identical to the copy the earlier web prototype uses |
| Licence | Apache-2.0 for all three networks, per the Google model cards archived with hashes in `$OMUZAKI_ASSETS/licences/mediapipe/` (`SHA256SUMS`). Open items, unchanged by this spike: the GHUM licence for the blendshape training mesh and the Google APIs Terms of Service. Register row: "MediaPipe face_landmarker.task" (`docs/licensing/register.md`, audit A1) |
| Cards | BlazeFace short range → `face_detector`; Face Mesh V2 → `face_landmarks_detector`; Blendshape V2 → `face_blendshapes` |

The `.task` file is a zip. Its four members, as extracted by `tools/convert_models.py`:

| File | SHA-256 | Bytes |
|---|---|---|
| `face_detector.tflite` | `b4578f35940bf5a1a655214a1cce5cab13eba73c1297cd78e1a04c2380b0152f` | 229,746 |
| `face_landmarks_detector.tflite` | `c7d54204ce0448474c7f3fa9af494787c0965cbdd6f20fc72867e43046bd43d5` | 2,553,590 |
| `face_blendshapes.tflite` | `4f36dded049db18d76048567439b2a7f58f1daabc00d78bfe8f3ad396a2d2082` | 955,312 |
| `geometry_pipeline_metadata_landmarks.binarypb` | `bdbcda96dfcb7da883da124aaa2c55dee49770d934f0fcc71747f8c21bdc75b4` | 19,376 |

## The ONNX conversions

Made with `tools/convert_models.py`: TFLite to ONNX (opset 17), batch dimension pinned to 1, every PReLU rewritten as `relu(x) + slope * (x - relu(x))` (the same function; it speeds up ONNX Runtime's CPU provider when the machine is busy, see the Findings in `README.md`), then each model is checked against the TFLite interpreter on five random inputs before it is accepted.

| File | SHA-256 | Bytes | PReLU rewritten | Max \|ONNX − TFLite\| |
|---|---|---|---|---|
| `face_detector.onnx` | `0a39f5661f965c6b8c3927bdef1b68c014246322175e6a0ec0bb8c07a42c1710` | 418,449 | 0 | 5.5e-05 |
| `face_landmarks_detector.onnx` | `8306a5e054ce1d0462af5e6bd114afd123c64c573d0773b2d49de4b033304a0d` | 5,081,861 | 69 | 3.8e-04 (on landmark values in input pixels, 0..256) |
| `face_blendshapes.onnx` | `91f07a202f9b2487fd42d54af83bc6a198a6977844f004083f8b046ef6ce14e1` | 1,884,267 | 0 | 1.8e-06 |

Before the PReLU rewrite the landmark model's tf2onnx output hashed `39584ef2d39841527044bbbf201fe5c6d49bb1ef433b4077ff5b5954a99603b1` (`manifest.json` keeps it as `tf2onnx_output_sha256`).

Conversion tools (a throwaway venv, dev time only, never shipped): TensorFlow 2.16.2 (Apache-2.0), tf2onnx 1.16.1 (Apache-2.0), onnx 1.16.2 (Apache-2.0), onnxruntime 1.26.0 from PyPI (MIT), numpy < 2 (BSD-3-Clause). The conversion keeps the weights' values (the TFLite file stores float16 weights; the ONNX files hold them as float32). **The hashes are not stable across runs or tool versions** (two runs of the same script produced different bytes for the same graphs), so the acceptance check is the numeric comparison above, and `manifest.json` next to the files records the tool versions and the hashes of the files actually used. The licence of a derivative is the licence of the weights it came from (Apache-2.0).

## ONNX Runtime

| Item | Value |
|---|---|
| Rust crate | `ort` 2.0.0-rc.13 (MIT OR Apache-2.0), features `std`, `load-dynamic`, `cuda`, **default features off**. Without `download-binaries` the build fetches nothing: no Pyke prebuilt binaries are involved (register row "ort") |
| Runtime library, CPU runs | Microsoft's official release, `onnxruntime-linux-x64-1.31.0.tgz` from `https://github.com/microsoft/onnxruntime/releases/tag/v1.31.0`, SHA-256 `cc5c72baf5ae5c8238a6841f0897227be2d02826b9cf98eaf02fdefaeeb03a57` (MIT; the tarball carries `LICENSE` and `ThirdPartyNotices.txt`) |
| Runtime library, CUDA runs | `onnxruntime-linux-x64-gpu_cuda12-1.31.0.tgz` from the same release, SHA-256 `62d3af5269af2143b5d37494337573f1ffb8885f3ad7477ae0cb1cc79c8dedfd` |
| NVIDIA libraries, CUDA runs only | cudart 12.8.90, cuBLAS 12.8.4.1, cuFFT 11.3.3.83, cuRAND 10.3.9.90, cuDNN 9.10.2.21 from the `nvidia-*-cu12` PyPI wheels already installed in an existing conda env (`LD_LIBRARY_PATH`). Proprietary runtime files with a redistribution grant (policy: "case by case"); used locally for the measurement, not copied, not shipped. The system CUDA 12.1 toolkit is too old (the provider needs `cudaLibraryGetKernel`, CUDA 12.4+) |

## Reference trackers used for comparison (dev only, nothing shipped)

- `mediapipe` (Python) 1.1.0 in a throwaway virtual environment, used by the earlier web prototype. Register row "mediapipe (Python) 1.1.0".
- `@mediapipe/tasks-vision` 0.10.14, the earlier web prototype's self-hosted copy, run in headless Chromium by `tools/js_tracker_bench.mjs`.

## Test inputs

Synthetic faces from the earlier web prototype's synthface pipeline: ICT-FaceKit Light meshes (MIT; the register keeps the row `unverified` for the scan-consent question, so these renders are private test inputs and nothing trained on them ships), Poly Haven HDRIs (CC0), our own shaders. Two sets, both on `$OMUZAKI_ASSETS/datasets/synthface/`: `spike01` (200 independent identities, 256 x 256, existing) and `clip01` (one identity, 90 frames at 30 fps, 480 x 480, made by `tools/render_clip.py`). **No frame is committed.** No footage of a real person and no private test material is used anywhere in this spike.
