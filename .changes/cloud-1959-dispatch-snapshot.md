# tools: dispatch-check reads the open-PR snapshot first

- `tools/dispatch-check.py` reads the open-PR snapshot when it is fresh (under 600 seconds old), and falls back to
  GraphQL, then REST, only when it is missing or stale. It prints which source it used (#1959).
- A task whose issue is on the snapshot's claimed list, or carries the `claimed` label, now waits as `wait #N: claimed`,
  unless `--mine` is passed (#1959).
