# Imported avatars: the contract they perform through

- An imported avatar has a contract of its own (`omuzaki-avatar-api`): its meshes, materials, expressions, channel
  drivers and gaze route, what it drives (`driven`), validation, and what export keeps exactly as the file had it. Its
  document node, `characters/<id>/avatar`, records only what a person chose: humanoid and channel overrides, material
  edits, the rights acknowledgement and attachments (#1214). A mesh the file draws at several nodes becomes one runtime
  mesh per node, so each copy deforms on its own, and the file's spring colliders, groups and chains are kept as the
  file has them (shared colliders, colliders in no group, repeated names), so export writes them back exactly (#2208).
- The stage, export and the tracking doctor see built characters and imported avatars through one `BodyView`, so
  nothing downstream branches on where a body came from (#864).
- `omuzaki-avatar` evaluates an imported avatar's expressions each frame with VRM 1.0's override and `isBinary` rules,
  matching `three-vrm`, allocation-free (#865).
- Procedural character ops refuse an imported character with a sentence saying it is an imported avatar, instead of a
  "missing field" error.
