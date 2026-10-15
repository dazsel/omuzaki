# omuzaki-face: the painted face reads

- A raised or lowered brow no longer moves rigidly: the inner end leads (0.018 m up, 0.031 m down per unit of height)
  and the outer end hardly moves (0.004 m), and the arch grows a quarter when the brow rises and is gone when it is
  fully lowered. A brow at height 0 is the prototype's, as before.
- Puffed cheeks (`cheek_puff`) are drawn: a shaded rim along the lower outer edge of each cheek and a highlight on its
  swell, so a pout reads even though the blush it adds is faint.
- The kiss no longer jumps when the mouth opens past 0.25: over the openings 0.21 to 0.25 the slit fades out and the
  lens fades in on one width that moves from the slit's to the lens', and the lower-lip gloss moves with them. From
  0.25 up, and for a kiss below 0.21, the mouth is drawn as before.
- Eyes draw their lids and lashes: the fine lower-lid line, the smiling lids with their soft rim (when the cheek raises the
  lower lid), the crease, the lash line with the cat-eye flick, the small lower lash, the inner tick, and the closed
  eyes (the happy `^` arc and the relaxed lash curve with its three lashes). `lash.thickness` is the thick end of the
  lash line (0.0076 m in the prototype) and `lash.flick` 1 is the prototype's full flick.
- Eye effects: star eyes, gloom, shock, spiral, squeeze (the eye closes into a `>` or `<`) and the tear drop, each
  scaling smoothly from 0 to 1 and composing with the others. The heart and star catchlights now crossfade with the
  profile's own shape instead of switching at 0.5, and every highlight fades with the gloom.
- The Overlay layer draws the sweat drop at the temple and the gloom's band and hatch over the forehead.
- The mock painter draws fangs (the `fangs` channel), so a grin shows in a strip.
