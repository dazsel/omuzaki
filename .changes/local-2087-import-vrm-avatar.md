# A VRM becomes an avatar you can perform as

- Drop a VRM or glTF and `io.inspect` with `as: avatar` prepares it as an avatar in memory: it maps the humanoid
  (finding bones by name or by the skeleton's shape, even with helper nodes, accessories below the feet or a second
  skeleton in the file), turns an A-posed rest onto the T-pose without touching the sculpt, welds and merges the mesh,
  and builds the face channels the model can express. It returns one plain-language report, what the model will drive,
  how long each stage took, and a confirm token (#2087).
- Face shapes a model lacks are made from its geometry: one-eye blinks and one-sided smiles split from symmetric ones,
  wide and squinting eyes, puffed cheeks, rolled and shrugged lips, and eyes that look around (#1090).
- `io.import` with `as: avatar` and the token makes it a character in one undoable step, asks you to acknowledge a
  model whose terms let only its author perform as it, and records the model's own terms. `avatar.remap`,
  `avatar.map_channel` and `avatar.describe` adjust and explain it afterwards (#1333, #868, #869).
- An imported avatar exports back to its file with every field it had, and an edit to a material changes only that
  field (#1294).
- One imported model can serve several characters, and models that share a texture under different licences each keep
  their own (#1792, #1773).
- VRM files now carry inside and plane colliders and spring joint limits both ways (#983).
