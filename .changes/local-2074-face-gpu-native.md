# The face is painted on the GPU straight into the renderer

- The live stage draws the face with the GPU rasteriser on hardware adapters, through `PaintTarget::native`: the face's commands go into the renderer's own encoder, so no pixels cross to the CPU and the renderer times the face paint under `PaintTextures`.
- A face repaint costs a fifth of the CPU it did (Vello's stages now run on buffers and bind groups kept between repaints) and allocates nothing outside wgpu.
- `PaintTarget::mark_damage` lets a GPU painter say what it changed, so the mip chain is rebuilt over the mouth, not the whole face.
