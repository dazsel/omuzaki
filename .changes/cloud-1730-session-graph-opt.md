# omuzaki-ai-api: `SessionOptions` carries a graph-optimisation level

- `SessionOptions` is now `#[non_exhaustive]`: build it with `SessionOptions::new(providers, threads)`.
- `SessionOptions::with_graph_optimisation` takes a `GraphOptimisation` (`Default`, `Basic`, `Extended`); stored options without the field read as `Default`, which loads exactly as before (#1730).
- `omuzaki-ai-ort` maps `Basic` and `Extended` onto ONNX Runtime's levels 1 and 2. No shipped model uses them yet.
