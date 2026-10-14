# omuzaki-mcp: `omuzaki://render/<id>.png`

- A render the asset store holds is read as an `image/png` blob, through `omuzaki_mcp::RenderProvider` (#881).
- The id is the content hash; only PNG bytes are served, so the URI cannot read other assets.
- Registration is the App's job: the provider needs the store, which `omuzaki mcp` does not supply yet.
