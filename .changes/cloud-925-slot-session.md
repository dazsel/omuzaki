# omuzaki-ai: slot sessions

- `ModelRegistry::open` returns a session that follows its slot: a new selection, install or removal is swapped in
  between runs, and `run` keeps answering from the old session (or `Unavailable`) while the replacement loads on a
  loader thread (#925).
- A crash or a runtime error switches the model off for the app session, falls back down the slot's chain and logs it;
  `info().fallback_for` names the model it stands in for. `reset` is replayed on a swapped-in session.
