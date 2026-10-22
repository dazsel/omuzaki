# Render wave 4: motion export, take and budget contracts, palette recolouring

- `io.export_animation` writes a library clip or an emote, on a character's own skeleton, as a VRMA file; three-vrm 3.5.5
  plays the files within 1e-4 of the source clip (`tests/interop/vrma/`).
- A palette change that moves a tint baked into hair vertex colours recolours those meshes in place
  (`Renderer::update_colours`) instead of creating them.
- The frame-budget scheduler takes the highest encoder load (`Scheduler::observe_output`) and steps quality down when
  an output's encoder is over 0.9 for 12 samples.
- Takes: a full State always names its project, `Mark` and `Session` are only placeholders, cause text never reaches a
  take, and live state with no linked provider is reported apart from what the writer left out.
- Shading programs register by name: `omuzaki registrations` lists `"hair"`, not the spec literal.
- io-api: a material blob serialises as base64; the passthrough finding is summarised per kind.
- Research: MJPG decode allocations measured (`docs/research/2026-10-mjpg-decode-allocation.md`); zune-jpeg stays.
