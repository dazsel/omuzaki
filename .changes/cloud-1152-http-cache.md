# omuzaki-http, omuzaki-stage: one HTTPS fetcher and one shared cache folder

- The fetcher waits out `Retry-After` on 429 and 503, with a cap on the wait.
- `app_cache_dir` gives the per-OS cache folder for models and fonts; `OMUZAKI_CACHE_DIR` overrides it.
- The sheet fonts download through the fetcher into the verified cache, and a second run reads the cache with no request.
