# 0001: Soft SD is the default character style (2026-10-10)

**Context.** Free-form 3D generation produced plastic toys, flat 2D or off-model results. Auto-rigging arbitrary meshes is unreliable.

**Decision.** Every new character starts as Soft SD: a 2–2.5-head chibi, painted anime face, mitten hands and matte toon shading. See `docs/design/style-guide-soft-sd.md`.

**Consequences.** One template rig and one shared body make outfits, emotes and animations portable across characters. Tall styles (5–7 and 7–8 heads) and non-human mascots ship as separate templates on the same skeleton contract (CB-01, CB-05); they are not a later tier.

*Amended 2026-10-11 (decision: no later tier).* Mitten hands remain the Soft SD default, but five-finger hands are available on every template (RG-09).
