# Face paint timing reads like the face probe

- `GpuTimings` gains `painters`: the painters' share of `Pass::PaintTextures` (the face rasteriser's raster and resolve), so the renderer's own premultiply and mip rebuild can be told apart from the paint. Additive; every constructor in the workspace sets it.
- `live_times` prints that split under `gpu.face_paint`, and `--steady MS` keeps the GPU busy for about `MS` milliseconds in front of each frame, the steady-clock reading spec §10 uses for `gpu.face_paint` (the `gpu_face_timing` probe's 2 ms load). Without it the readings stay the paced ones, at whatever clock the GPU idles at.
- `WgpuRenderer::device_and_queue` lends a probe the renderer's device and queue.
