# omuzaki-io-ops: `fit_decimate`, `fit_meshes` and platform limit tables

- `fit_decimate::decimate` and `decimate_scene` cut triangles to a budget with a quadric edge-collapse, keeping skin weights and morph targets consistent (#1075).
- `fit_meshes::merge_meshes` joins mesh nodes that no expression binds, the `fit.meshes` stage (#1137).
- The `limits` module holds platform limit tables as data (`data/platforms/vrchat-pc.toml`, `vrchat-quest.toml`); `vrchat_rank` now reads them instead of its own table (#1076).
- A limit table is the `[limits]` section of `data/platforms/<id>.toml`, registered by its platform with `register_limit_table!` (no central list). It has a `beyond` label for an avatar past the last tier and an optional `advisory` list of recommendation tiers (a stat past one but within the target is a warning). `AvatarStats` gains `vertices`, `morph_targets`, `textures`, `max_texture_side`, `meshes` and `attachments`.
- `decimate_scene` gives a share a small primitive cannot use to the others, and `decimate` refuses an infinite `max_error`. `merge_meshes` carries each node's mesh and skin in its plan, and `benches/fit.rs` measures it against its 200 ms budget.
