# Every shadow-casting key light casts its own shadow

- Each directional light with `casts_shadow` now gets its own shadow map, in light order, up to four at `High`
  quality (#675). `Low`, the live default, keeps one map for the first casting light, so live frames cost what they
  did. `ShadowQuality::max_maps` and `limits::MAX_SHADOW_MAPS` say how many.
- Each map is fitted and texel-snapped to its own light, and keeps its own live redraw interval; a light that turns or
  is added redraws only its map.
- The shadow-catcher ground multiplies the shadows of every shadowed light, as three.js's `ShadowMaterial` does.
- One-key scenes render exactly as before.
