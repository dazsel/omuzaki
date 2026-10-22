# The measurement light rig and the `measure` render preset

- A new library light rig, `measure-flat` (#290): one frontal key from the camera's direction and an even white ambient, with no rim, no fill and no bloom. It is for renders that measure a face (the `measure.*` ops), not for looks. `stage.light_rigs` lists it, and `render.preview {light_rig: "measure-flat"}` takes it today.
- `library/render-presets/measure.toml` is the `measure` preset of the likeness design (D10): an orthographic front camera at the face framing, `measure-flat`, the neutral expression, post effects off, a `#1a1a1a` line of at least 1.5 px, and at least 160 px between the eyes. `render.preview {preset}` reads it with #376.
- Under `measure-flat`, the face front of the kit's `standard` and `mini` characters varies by 1.84 in CIELAB L* (dev-studio: 2.7). The same measure reads 12.4 for a 15% shade band painted onto half of it.
