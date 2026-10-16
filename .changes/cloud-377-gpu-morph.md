# omuzaki-render: the GPU morph compute pass

- Morph targets are now blended by a compute pass on any device with compute; a device without it (WebGL2) keeps the CPU blend. The path is chosen when the renderer is created, `WgpuRenderer::morph_path()` says which, and it never changes under a live mesh.
- A mesh's sparse targets are uploaded once, when it is created; a frame uploads only the weights. The pass writes into the same pooled per-instance buffers as the CPU blend, so the change-skip, the stats and the release are one mechanism.
- Nothing is read back: a morphed instance's bounds are the mesh's grown by what the weights can move a vertex, and its outline directions are re-welded by a second dispatch.
- On a 20 000-vertex, 52-target head the GPU result is within 1e-4 m of the CPU's.
