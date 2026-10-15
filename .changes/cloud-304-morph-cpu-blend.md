# omuzaki-render: morph targets

- `WgpuRenderer` now reports `morph_targets: true` (up to 256 targets per mesh) and draws instances with `morph` weights.
- Targets stay sparse per mesh; each morphed instance is blended on the CPU (position and renormalised normal) into a pooled per-instance vertex buffer, which is then skinned.
- An unchanged weight set, mesh and base vertices skip the blend and the upload; after warm-up a frame allocates nothing.
