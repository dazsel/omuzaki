# Imported avatars: the drop-to-perform timing test, and one joint run per skin

- A generated 40 MB VRM 1.0 (twenty 2048-pixel textures, a 20 000-vertex face with the 52 ARKit shapes, 150 bones)
  goes from `io.inspect` through `io.import … as: avatar` to its first frame on the stage, and every stage of the
  design's machine-time table is reported against its budget; `OMUZAKI_BUDGET_TIER=Dev` fails on an overrun (#1368).
- The avatar stage shares one joint run between meshes on the same skin. An ordinary model with many primitives on a
  150-joint skin no longer passes the scene's 2048 joint matrices and fails its first frame.
