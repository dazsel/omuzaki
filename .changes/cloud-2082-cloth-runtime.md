# omuzaki-char: cloth runs live on garments (`ClothRuntime`)

- `cloth::runtime::ClothRuntime` binds a garment's lofted grid to a coarse XPBD sheet pinned to the skeleton: drawn as the skinned mesh plus what the sheet simulated, with per-row hybrid skinning, and no allocation per frame (#197).
- Live and Offline quality, frame-exact offline replay in sub-steps, and a settle cache for scrubbing (`cloth::quality`, #198).
- Collisions with the body (capsules fitted inside the `BodyFit`), the hair's collider proxies and the layer below as a shell (`cloth::collide`, #333); thin colliders meet the sheet's triangles, and particles squeezed between colliders stay outside both (#129).
- Particle levels of detail by on-screen size, with hysteresis, sleep, and no pop at a switch; `StepCtx` gains `pixels_per_metre` and a constructor (#706).
- Exports bake the sheet to VRM spring chains and a skinned mesh within 80 (skirt) or 120 (robe) bones (`cloth::bake`, #386).
