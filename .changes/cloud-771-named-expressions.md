# A character's own named expressions reach the exported VRM

- `convert::channels::named_expression` names a character's expression for export. An expression named like a VRM preset (`happy`) keeps that preset; any other gets `omuzaki_<id>` (`omuzaki_smug`). The VRMA exporter is meant to call the same function (#771).
- `io.export` asks the face baker for every expression of the face that isn't a preset, under that name. The baker composes each one from the expression's channel deltas, as it does for a preset: morph targets at their weights, and the atlas flips and colours of channels at 0.5 or more. `import(export(x))` finds `omuzaki_smug`.
