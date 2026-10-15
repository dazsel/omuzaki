# omuzaki-io-ops: `prepare::arkit` synthesises the `ARKit` channels a model can already express

- `synthesise(&mut IoScene)` adds custom expressions named like `ARKit` from morph targets (exact name, the same name in another case or with a `blendShape.` prefix, or a `VRoid` `Fcl_*` name) and from VRM preset expressions (jaw, mouth shapes, smile, stretch, blink, look up and down, brow down, inner brow), and reports how each of the 52 channels was obtained (#295).
- An existing expression is never replaced. Channels that need new geometry stay `Missing`, so the export's `perfect_sync.missing_channel` rule still applies to them.
