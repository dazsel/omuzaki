# omuzaki-stage: the kit character performs live from a chosen camera (#2072)

- `perform::live::LiveStage` runs a live frame: it swaps in the source the input posted, services face variants, advances the performer and ticks the stage with the step's pose and channels, then feeds the renderer's report to `omuzaki-budget`'s `Scheduler`. It renders only when a step ran, so the result is deterministic in simulation time (#306, #314, #821).
- `perform::frame::PerformFrame` is the glue from the perform loop to the frame loop. A frame's `dt` covers every step since the last frame.
- `perform::input`: `InputHost` and `LiveInput` start and stop a `Tracker` on the bound camera, and hand frame sources to the perform thread through `SourceSlot`, which never frees on that thread. `input.source` and `input.camera` are Machine settings.
- The `input.*` ops are `input.select`, `input.bind_camera` (person only), `input.preview`, `input.request_camera` (person only) and `input.state`. Each edit answers its inverse call for undo (#301).
- The character's expressions are installed for the triggers when the live stage starts (#2088).
- `Performer::set_source` returns the old source, and `Performer::rest` returns the rest face.
