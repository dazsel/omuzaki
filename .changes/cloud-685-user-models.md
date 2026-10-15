# omuzaki-ai: `ModelRegistry::add` for models the person adds (part of #685)

- `add` copies each file into the cache under its hash, writes a manifest when none came (`user-<name>`, version `0.0.0`, licence unknown), checks the pins, `validate(Origin::User)`, `min_app_version`, the probe, the slot signature and `[stream]` rules, and the slot's smoke test. Every problem of the first failing stage is listed; a failed model stays listed as `Disabled` with those words, a pass is `NeedsAcknowledgement` (or `Ready` when `acknowledged_by` is set).
- `state.toml` keeps the user models and one acknowledgement per digest (person, disclaimer version, time).
- Probing and the smoke test run in this process; the sandboxed worker (#1153) takes them over. No user model is opened by `open` yet.
