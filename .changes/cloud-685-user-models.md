# omuzaki-ai: models the person adds, end to end (#685, #687, #857, #928, #1153, #795)

- `add` copies each file into the cache under its hash (through the cache's own store), writes a manifest when none
  came (`user-<name>`, version `0.0.0`, licence unknown), and checks the pins, `validate(Origin::User)`,
  `min_app_version`, the slot signature, the `[stream]` rules and the slot's smoke test. A model that fails stays
  listed as `Disabled` with every problem of the first failing stage.
- User models are probed, smoke-tested and run in a sandboxed worker process (`omuzaki_ai::sandbox`,
  `omuzaki-model-worker`): a memory cap, a lower priority, killed with the app; a crash or a run past the slot's
  deadline kills the worker, and the slot falls back and says why. The worker gets native libraries from the registry,
  never from the network.
- `acknowledge` records the person's acceptance of the notice once per digest (person, notice version, time). An agent
  can't switch to a model the person hasn't accepted (`NeedsPerson`), and nobody can select one that isn't `Ready`.
  `remove` forgets a user model and deletes its cached files unless another model uses them.
- The consent hook (SE-05): a model that imitates a real person is `NeedsConsent` until the app's `ConsentLookup`
  finds the record (`Registry::set_consent`), and `Disabled` in a slot whose models can't imitate anyone. Every list
  shows `IMPERSONATION_WARNING`.
- The `model.list`, `model.status`, `model.add`, `model.select`, `model.remove` and `model.acknowledge` ops, with
  did-you-mean on names; `model.status` reports a slot's download percent.
- `omuzaki-ai-api`: `ModelEntry::imitates` and `ModelRegistry::downloading` (default `None`).
- The `VerifiedCache` store steps that differ on Windows have tests that run on every platform.
