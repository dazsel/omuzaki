# Face models

The face-tracking models in `models/mediapipe-face/` are ONNX conversions of the three networks inside Google's
MediaPipe `face_landmarker.task` bundle (float16, version 1). They are licensed under the **Apache-2.0** licence; the
licence text is in `Apache-2.0.txt` next to this file.

| File | Network | Source |
|---|---|---|
| `face_detector.onnx` | BlazeFace short range | converted from `face_detector.tflite` |
| `face_landmarks_detector.onnx` | Face Mesh V2 | converted from `face_landmarks_detector.tflite` |
| `face_blendshapes.onnx` | Blendshape V2 | converted from `face_blendshapes.tflite` |
| `geometry_pipeline_metadata_landmarks.binarypb` | face geometry metadata | copied unchanged from the bundle |

- **Original bundle:** `face_landmarker.task`, published by Google at
  `https://storage.googleapis.com/mediapipe-models/face_landmarker/face_landmarker/float16/1/face_landmarker.task`,
  SHA-256 `64184e229b263107bc2b804c6625db1341ff2bb731874b0bcc2fe6544e0bc9ff`.
- **Attribution:** Copyright Google LLC. The networks are used under the Apache-2.0 licence. This product is not
  endorsed by Google.
- **Changes made:** each network was converted from TFLite to ONNX (opset 17) with the batch dimension fixed to 1; in
  the landmark network every PReLU operation was rewritten as the equivalent `relu(x) + slope * (x - relu(x))`. The
  weights keep their values. Each conversion was checked against the TFLite interpreter on random inputs.
- **Licence of the conversions:** the same as the weights they come from, Apache-2.0.
- **Integrity:** `models/mediapipe-face/manifest.json` lists the SHA-256 of every file; the packager refuses to build a
  zip whose files do not match it.
