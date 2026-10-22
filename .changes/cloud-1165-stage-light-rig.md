# A "stage" light rig: coloured spots, a strong rim and a dark house

- `stage.set_light_rig` takes `rig: stage`, a design of our own (#1165). Three spots hang on the truss: magenta on the
  head, cyan on the shoulders and chest, and amber on the crown from behind. A strong cool rim comes from behind, a
  soft front light keeps the face readable, and the ambient is dim on a near-black background.
- Light-rig files take `[[spot]]` lights, each with an `id`, a colour, an intensity, a `position` and a `target` in
  metres from the feet, a `cone`, a `penumbra` and a `distance`. They reach a scene as accent spots in subject units,
  as the candle's point light does.
- Skin keeps its own hue under coloured light, so the spots tint hair and clothes and brighten skin.
