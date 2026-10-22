# Agents can see what an edit did: render.compare, render.contact_sheet and render.turnaround

- `render.compare {a, b, mode, threshold}` reads two stored renders and stores a heatmap (the default), a difference or the two side by side, and reports whether they are identical, how many pixels changed, where (the box around them) and by how much (#336).
- `render.contact_sheet {images, columns, gutter}` lays up to 64 stored renders out in a grid as one PNG.
- `render.turnaround {character, framing, size, ...}` renders a character, built from the kit or imported, from the front, three-quarter, side and back in one row.
- All three are jobs that change nothing in the document; their pictures go to the content store as `omuzaki://render/<hash>.png`, and each has a dry-run cost hint.
