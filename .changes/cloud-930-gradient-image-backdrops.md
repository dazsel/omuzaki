# Scenes take a gradient or a picture as the backdrop

- `stage.set_backdrop` takes `kind: gradient` with a `top` and a `bottom` colour, and `kind: image` with an `asset` (a
  PNG or JPEG in the asset store), a `fit` (`cover` crops to fill, `contain` shows the whole picture) and the `top`
  and `bottom` of the bars `contain` leaves, which default to the rig's background colour (#930).
- Both are shown as authored: exposure and the tone curve light the character, not the picture behind it, and the
  frame is laid over the picture the way a compositor lays a transparent output over one.
- A picture is decoded off the render thread and uploaded once when the asset changes; every frame after that only
  samples it. Until it is up, the bars show.
- A `colour` backdrop is shown as authored too, so a green screen keys at exactly its colour whatever the exposure
  and tone curve. The rig's own background colour is still lit with the scene.
- Scene files without these fields read and write exactly as before.
