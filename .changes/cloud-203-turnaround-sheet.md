# omuzaki-stage: turnaround sheet

- `sheet::turnaround::render_turnaround` draws the front, three-quarter, side and back views of a character in its rest pose or a built-in hero pose (`hero_pose`), settled and lit the same way, one cell per view.
- Each view carries its framing: pixels per metre, eye-line row, head centre, crown and sole rows, for the hair trace tool and the character builder.
- A cell larger than the renderer's `max_texture_size` (or `TurnaroundSpec::with_max_tile`) is rendered in tiles and stitched. Tiling is exact for `Lens::Orthographic`; the 12-degree `Lens::Telephoto` (not the default) refuses to tile, because a perspective sub-window needs an off-axis camera that the render API does not have yet.
