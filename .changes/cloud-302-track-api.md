# omuzaki-track-api and omuzaki-track-mock: the tracker contract

- New contract `omuzaki-track-api` (PF-01, #302): the camera seam (`Camera`, `CameraStream`, borrowed `CameraFrame`s, `CameraFormatRequest::pick`), the `Tracker` trait with `LatencyStats`, `ClockMap` (device time onto the stage clock) and the wait-free newest-wins mailbox behind anim-api's `FrameSource`.
- New mock `omuzaki-track-mock`: `FakeCamera` (scripted or synthetic frames, busy, missing and lost devices) and `FakeTracker` (scripted `TrackFrame`s with lost gaps); no threads, no sleeping.
- Design: `docs/design/track-api.md`.
- Contract types other crates build (`CameraFormat`, `CameraInfo`, `CameraFormatRequest`, `CameraFrame`, `TrackerConfig`, `LatencyStats`) are `#[non_exhaustive]` with constructors (ADR 0054 §4); `TrackerConfig` saves `lost_after` as whole milliseconds (`lost_after_ms`); `ClockMap::map` output is strictly increasing (at least `MIN_STEP`, 1 µs, apart).
- Benchmarks (`omuzaki-track-api/benches/hot_path.rs`): clock mapping with the fit full, and mailbox publish and poll, alone and overlapped on two threads.
