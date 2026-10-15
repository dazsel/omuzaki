# omuzaki-track-api and omuzaki-track-mock: the tracker contract

- New contract `omuzaki-track-api` (PF-01, #302): the camera seam (`Camera`, `CameraStream`, borrowed `CameraFrame`s, `CameraFormatRequest::pick`), the `Tracker` trait with `LatencyStats`, `ClockMap` (device time onto the stage clock) and the wait-free newest-wins mailbox behind anim-api's `FrameSource`.
- New mock `omuzaki-track-mock`: `FakeCamera` (scripted or synthetic frames, busy, missing and lost devices) and `FakeTracker` (scripted `TrackFrame`s with lost gaps); no threads, no sleeping.
- Design: `docs/design/track-api.md`.
