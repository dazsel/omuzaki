# omuzaki-camera-mf: the Windows camera

- New crate `omuzaki-camera-mf`: `MfCamera` implements the `Camera` trait on Media Foundation, handing over raw NV12, YUY2, MJPG and RGB32 frames with the real row pitch. Mode choice and error mapping are shared tables that build on every platform (#313).
