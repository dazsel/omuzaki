# omuzaki-render: dynamic meshes and depth of field cost less per frame

- A dynamic mesh's outline refresh costs 0.14 ms for 20 000 vertices on the CI rasteriser, down from 0.55 ms (#1541). Where the adapter has compute, the outline directions are welded by a dispatch from the vertices on the GPU, and a dynamic mesh without morph targets keeps no CPU copy of its vertices: `update_vertices` interleaves straight into the upload and fits the bounds as it goes.
- The shadow bounds of a dynamic mesh are refitted only when a frame reads them (a shadow-casting light fitted to a map, or a blended material), and a dynamic morphed mesh that every instance draws morphed does no base refresh at all (#1843). What a frame skips stays stale, so a later frame that needs it brings it up to date first.
- Depth of field is 23% cheaper on the CI rasteriser at 1080p (#656): the gather reads a precomputed spiral and, on an opaque scene, one packed image a tap instead of two. The picture is unchanged within the golden tolerance.
- `update_vertices` checks that its vectors are finite about four times faster.
- The mip chain (#87) keeps one pass per level: rebuilding a full 1536 px chain records in 37 µs of CPU, and it already redraws only the repainted rectangle.
