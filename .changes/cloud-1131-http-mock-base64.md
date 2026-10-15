# omuzaki-http-mock: cassette base64 uses the workspace `base64` crate

- Cassette `body_base64` and `chunks[].base64` values decode with `base64` (the workspace crate) instead of a hand-written decoder.
- Accepted inputs are unchanged: canonical padding is required, and non-zero bits after the last character are still accepted (`"Zh=="` is `f`).
- A bad value is refused with the same `"body_base64" is not valid base64: <why>` shape; the `<why>` wording now comes from the crate.
