# omuzaki-io-fbx: FBX import (binary 3000+ and ASCII)

- New crate `omuzaki-io-fbx` registers an importer for format `fbx` (import only). It reads the node hierarchy into an `IoScene` in canonical space: right-handed, +Y up, the model's front toward +Z, metres. Each node's local transform is expressed in those axes, so a turn about the file's up axis comes out as a turn about +Y (#977).
- `fbx.units_converted` and `fbx.axes_converted` (info) say when the file was not already in metres or right-handed Y-up.
- Limits: the input size, the node count, and ufbx's memory (1 GiB), allocation count and hierarchy depth (1024). A truncated file is `Malformed`; an OBJ or other non-FBX file is `Unsupported`.
- Meshes, materials, skins and textures are not read yet; what a file holds of them is counted and reported as `fbx.content_dropped` (warning). Vertex arrays and embedded images are skipped while loading. What ufbx repaired is reported as `fbx.repaired` (warning).
- The node cap also sets ufbx's memory and allocation ceilings (up to the fixed ones). `omuzaki-cli` links the importer, so `io.import` and `io.inspect` accept FBX.
