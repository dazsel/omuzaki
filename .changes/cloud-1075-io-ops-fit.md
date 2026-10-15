# omuzaki-io-ops: `fit_decimate`, `fit_meshes` and platform limit tables

- `fit_decimate::decimate` and `decimate_scene` cut triangles to a budget with a quadric edge-collapse, keeping skin weights and morph targets consistent (#1075).
- `fit_meshes::merge_meshes` joins mesh nodes that no expression binds, the `fit.meshes` stage (#1137).
- The `limits` module holds platform limit tables as data (`data/limits/vrchat-pc.toml`, `vrchat-quest.toml`); `vrchat_rank` now reads them instead of its own table (#1076).
