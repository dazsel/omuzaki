# CI: merge-queue test shards balanced by measured time, and the dependency cache found

- The merge queue's test jobs now find the dependency cache. `RUST_BACKTRACE` at the top of `ci.yml` changed the cache key, so none of the six test jobs ever restored the cache that `cache-warm.yml` saves; the variable is now set on the test steps only.
- `omuzaki-ui` is built once instead of in two halves, and the three slowest tests of `omuzaki-char` (`palette_independence`) run as a shard of their own.
- `tools/ci/weights.json` carries the measured numbers; `tools/ci/weights.README.md` has the table and how to refresh it.
