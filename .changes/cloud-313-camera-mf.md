# omuzaki-camera-mf: the Windows camera

- New crate `omuzaki-camera-mf`: `MfCamera` implements the `Camera` trait on Media Foundation with an asynchronous source reader, so `next_frame` honours its timeout. It hands over raw NV12, YUY2, MJPG and RGB32 frames with the real row pitch, checked against the buffer's real allocation, and `seq` follows the capture times so skipped frames show as gaps. Mode choice, frame bounds, the sequence and the timed hand-off are shared code that builds and tests on every platform (#313).
