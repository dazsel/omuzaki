# omuzaki-camera-mf: COM once per thread, camera released on drop, padded 2-D buffers

- `next_frame` no longer sets up and tears down a COM apartment on every frame. The first Media Foundation call on a thread runs `CoInitializeEx`, a thread-local guard keeps the apartment until the thread exits, and every later call is a thread-local read (#1878).
- Dropping a stream now waits up to 50 ms for the camera to be released, so a drop followed by an `open` of the same camera finds it free. A driver slower than that no longer makes the tracker's stop late: the drop returns, and `open` of the same camera waits for the release (up to 5 s) and then fails with `TrackError::Busy` (#1897).
- A buffer with a 2-D interface is borrowed through its pitch when the real extent is known (`Lock2DSize`) or the rows are unpadded (`Lock2D`); a padded buffer with no extent takes the packed `Lock`, so padding bytes are never read as pixels (#1897).
- A thread that is a guest in another COM apartment is probed again on every call, so the guard takes over if that apartment is torn down (#1878).
