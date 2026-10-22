# render.contact_sheet labels its cells and renders expressions and variants

- `render.contact_sheet` takes a `character` with `expressions` (one cell each, on one `variant` if given) or `variants` (one cell each, all in one `expression` if given), rendered with one `framing`, `light_rig`, `cell` size and `quality`, as well as stored `images` (#1715).
- Each cell is labelled underneath on a paper sheet: the expression or variant id, or the first 12 characters of a stored picture's hash. `labels: false` keeps the transparent, unlabelled grid. The result lists every cell's label in `cells`.
- Limits: 1 to 64 cells, 1 to 16 columns, rendered cells 16 to 1024 pixels a side.
- The sheet layout copies a picture placed at its own size instead of filtering it, so turnaround and contact sheets no longer soften pictures that aren't scaled.
