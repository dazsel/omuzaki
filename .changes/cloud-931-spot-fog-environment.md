# Scenes take spot lights, fog and a sky or HDRI environment

- `stage.add_light` and `stage.set_light` take `kind: spot`, with a `position`, a `target`, a `cone`, a `penumbra` and
  a `range` in the anchor's units (#931, LK-08). Its intensity scales with the subject's height squared, like a point
  light, so a spot lights a face the same at any stature.
- `stage.set_lighting` takes distance `fog` (a colour, and `near` and `far` in subject units from the camera) and an
  image-based `environment`: a procedural `sky` from three colours or an `hdri` from a stored Radiance `.hdr`, with an
  `intensity`, a `rotation` and a `toon_ramp` switch. Either can be set back to `off`.
- `stage.set_backdrop` takes `kind: environment`: the environment shows behind the character, turned with it, at full
  resolution. Without an environment the rig's background colour shows, and `stage.describe_scene` lists it among the findings.
- Scene files without these fields read and write exactly as before.
