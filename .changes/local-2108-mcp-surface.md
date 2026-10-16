# omuzaki-mcp: the full agent surface over MCP; omuzaki-ops-api: a finished job carries its outcome

- `JobState::Done` keeps the op's `Outcome` in memory (`JobState::done`, `with_outcome`, `outcome`), so polling a job renders the same result as waiting for it. The variant is `#[non_exhaustive]`: build it with `JobState::done`.
