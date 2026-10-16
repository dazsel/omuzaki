# omuzaki-ai: slot sessions

- `ModelRegistry::open` returns a session that follows its slot (#925). On an ordinary change (another selection, a
  new default, an install) the replacement loads on the registry's one loader thread while `run` keeps answering from
  the old session, and the first `run` after it is ready hands over. A replacement that fails to load, or whose
  runtime panics while loading or resetting it, leaves the old session answering, and `slots` says why.
- When the running model can no longer be used (removed, disabled, its acknowledgement withdrawn), `run` answers
  `Unavailable` at once, until its replacement is applied.
- A crash or a runtime error switches the model off for the app session, falls back down the slot's chain and logs
  it; `run` answers `Unavailable` meanwhile, and `info().fallback_for` names the model it stands in for.
- `reset` resets the session that answers, and a replacement is applied only once it was reset as often.
- `slots` and `models` report as active only the model that answers; `slots` gives the reason whenever that isn't the
  selected one (loading, a load failure, a crash, removed or disabled).
