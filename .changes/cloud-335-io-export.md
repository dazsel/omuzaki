# io.export: write a character to a VRM or glTF file

- `io.export {character, format, path, purpose, perfect_sync, variant, target_height}` writes a kit character or an imported model to `vrm1`, `vrm0` or `glb` in one step (#335). It runs `io.check_export`'s check and refuses on any error it finds (the licence gate for `publish`), bakes the painted face into the 52 perfect-sync channels with the registered face baker, and writes the file atomically: a temporary file renamed over the old one. A dry run returns the check's findings and says whether the face will be baked, and writes nothing.
- Without a face baker, or for a character with no painted face, the file carries the VRM presets only, and a `perfect_sync.not_baked` finding says why. An imported model keeps perfect sync only when it already has all 52 channels.
- Exports carry the character's look (#713). The look applies to every material, including the hair's shading program, which exports as its toon approximation.
- `JobCtx::services()` gives a job the app's services, so its work can run a check its plan also runs.
