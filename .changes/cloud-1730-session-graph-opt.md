# omuzaki-ai-api: `SessionOptions` is `#[non_exhaustive]`

- Build it with `SessionOptions::new(providers, threads)`; a struct literal or `..SessionOptions::default()` no longer compiles outside the crate. Fields can now be added without breaking callers (#1730).
