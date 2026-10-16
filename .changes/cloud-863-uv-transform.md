# Render: material UV transform

- A material's UV transform (scale, then offset) now moves the base texture and the UV-mapped overlays on the GPU
  renderer, as it already did on the mock (#863).
- Hard-texel atlases are the only case the conformance check pins down; linear sampling is unchanged.
