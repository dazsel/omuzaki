# omuzaki-camera-mf: COM once per thread, camera released on drop, padded 2-D buffers

- `next_frame` no longer sets up and tears down a COM apartment on every frame. The first Media Foundation call on a thread runs `CoInitializeEx`, a thread-local guard keeps the apartment until the thread exits, and every later call is a thread-local read (#1878).
- Dropping a stream now waits up to 50 ms for the camera to be released, so a drop followed by an `open` of the same camera finds it free. A driver slower than that no longer makes the tracker's stop late: the drop returns, and `open` of the same camera waits for the release (up to 5 s) and then fails with `TrackError::Busy` (#1897).
- A buffer with only `IMF2DBuffer` (no `Lock2DSize`) is locked through `Lock2D` and its pitch is honoured, so a padded surface is borrowed as it lies instead of falling back to the packed `Lock`, which may copy it (#1897).
