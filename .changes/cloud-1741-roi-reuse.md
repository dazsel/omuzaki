# omuzaki-track: ROI reuse and frame skipping

- Video mode skips the landmark, blendshape and pose pass on a frame whose ROI barely changed since the last full pass (a luma signature of the ROI, a mean and a block-peak threshold), repeating the last sample, for at most 2 frames in a row, and only after a confident pass.
- The detector re-runs every 60 frames; a re-detection that finds nothing keeps the tracked face. Low presence still drops the face and re-detects, as in MediaPipe.
- `FaceTrackerConfig::reuse` (`pipeline::ReuseConfig`) holds the knobs; `ReuseConfig::OFF` is MediaPipe's video mode. Image mode and `track_roi` are unchanged.
- Design and prior art: `docs/design/track-roi-reuse.md` (#1741).
