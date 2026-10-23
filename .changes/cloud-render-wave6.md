# Edge lines at depth jumps, framing that holds the hair, and smaller render fixes

- `Scene::edges` (render-api `EdgeLines`): a line wherever the depth jumps, on the near side, `width_px` output pixels
  wide. A jump is a step of more than `step` times the depth beyond what the gentler of the two sides' slopes explains,
  so a steep chin over a flat neck draws and a surface seen edge-on doesn't. `EdgeLines::coverage` is the one
  definition; the mock calls it and the wgpu renderer runs the same test in the outlines pass, as a fullscreen draw
  reading the frame's depth. A conformance check holds both to it.
- The `measure` render preset draws them (`outline.edge_step = 0.002`), so its line closes under the chin, where the
  hull line hid behind the neck (#2319).
- Framing aims at the head as it is seen: the head's box grows by the hair and hats that sit on it, so face and bust
  shots stop cropping them (#2244). A face shot is wider for it: the `measure` preset's documented size is now 1024 px.
- A bench for the UV transform's cost (`cargo bench -p omuzaki-render --bench uv_transform`; `OMUZAKI_BENCH_GPU=1`
  for a hardware adapter).
