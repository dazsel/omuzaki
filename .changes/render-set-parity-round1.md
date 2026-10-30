# Sets from the reference loop: a grand library, new props and soft daylight shadows

- `stage.set_stage_preset {preset: "grand-library"}`: a day-lit library with painted bookcases, cabinets, a window, a
  gramophone, a framed picture, an urn, a chest, an armchair and a carpet. It pairs with the new `library-day` light rig.
- New prop kinds: `gramophone`, `picture`, `urn`, `chest` and `armchair` (an upholstered chair with a tufted oval back
  and a studded rim).
- `shelf` takes `trim` (gilded strips, lips and a crown), `doors`, `pilasters`, `back` (the panel behind the books) and
  `arch` (round arches over the top bay).
- `rug` takes a woven `pattern` (`plain`, `medallion`, `foliage`, `lattice`) in a `motif` colour.
- `window` takes `glazing: diamond` (leaded diamonds) and `arch` (a round head).
- `room` takes `ceiling` (with a moulded border) and `mottle` (patchy plaster on the walls).
- Soft shadows (#2666): `Scene::shadow_softness` in the render API (0 to 1). It is also `shadow_softness` in a scene's
  lighting, in `stage.set_lighting` and in light-rig files. Above 0, shadow edges widen into a penumbra of up to about
  8 cm, sampled on a turned 16-tap disk. 0, the default, draws exactly as before. `library-day` uses 0.8.
- `cargo run -p omuzaki-stage --features wgpu --example scene_still -- RECIPE.json OUT.png` renders a scene built from
  registered ops to a PNG, headless.
