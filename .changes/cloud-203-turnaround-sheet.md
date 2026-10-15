# omuzaki-stage: turnaround sheet

- `sheet::turnaround::render_turnaround` draws the front, three-quarter, side and back views of a character in its rest pose or a built-in hero pose (`hero_pose`), settled and lit the same way, one cell per view.
- Each view carries its framing: pixels per metre, eye-line row, head centre, crown and sole rows, for the hair trace tool and the character builder.
- A cell larger than the renderer's `max_texture_size` (or `TurnaroundSpec::with_max_tile`) is rendered in tiles and stitched. Tiling is exact for `Lens::Orthographic`; the 12-degree `Lens::Telephoto` (not the default) refuses to tile, because a perspective sub-window needs an off-axis camera that the render API does not have yet.
- `Stage::settle_from_rest` resets the spring bones and part runtimes to rest before settling, `Stage::redraw` now uploads the dynamic meshes that moved (and counts them in `FrameOutput::uploads`), and `Stage::tick` refills the scene even when the frame fails after a variant swap.
- `Stage::pin_time` / `unpin_time` run every tick and redraw at one fixed time (the clock does not move) and give the last frame back afterwards; the sheet pins `SHEET_TICK`, so its four views do not depend on the stage clock or on earlier frames. `Stage::drawn_vertices` gives the world-space vertices as drawn (skinned, deformed by the runtimes), which the sheet's crown and sole rows are projected from.
- `SheetPose::custom` takes any `HumanoidPose`; `SheetPose` is no longer `Copy`, `Eq` or `Hash`.
