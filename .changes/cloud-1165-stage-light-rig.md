# A "stage" light rig: tight coloured spots, a cool rim and a lit floor in a dark house

- `stage.set_light_rig` takes `rig: stage`, a design of our own (#1165). Three tight spots each own one part of the
  figure so their cone edges show: magenta across the head and the near shoulder from stage left, cyan down across the
  shoulder and chest from high stage right, amber on the crown and its rim from high behind. Two cool back lights edge the silhouette,
  a dim front light keeps the face readable, and the ambient is dim on a near-black background.
- Lights behind a character now edge its silhouette in their colour when a scene asks: `Scene::back_light_rim` in the
  render API, `back_light_rim` in a scene's lighting and in `stage.set_lighting`, and in light-rig files. Toon shading
  tints both sides of a form with every light, so without it a back light only coloured the front. The rim is a crisp
  band at the silhouette, added after the skin treatment so it reads on skin; a toon material with no rim of its own (a
  floor) takes none. 0, the default, draws exactly as before.
- `stage.set_ground` takes `kind: floor`, a lit stage floor that shows the pools the spots leave and the cast's shadow,
  with an optional `colour` (a dark stage grey by default).
- Light-rig files take `[[spot]]` lights (an `id`, a colour, an intensity, a `position` and a `target` in metres from the
  feet, a `cone`, a `penumbra` and a `distance`), `back_light_rim`, and `scene_skin_floor`, the skin floor a scene lit by
  the rig sets on its cast: in a dark house skin may fall darker than a bright studio allows, or every light only makes
  it paler. The stage rig sets 0.2.
- Skin keeps its own hue under coloured light, so the spots' colours land on the hair, the cone edges, the floor pools
  and the rim, and brighten skin in its own hue.
- `stage.contact_sheet` takes `titles` for stored images, one each, so a sheet can label its columns (`design target`,
  `studio`, `stage`).
