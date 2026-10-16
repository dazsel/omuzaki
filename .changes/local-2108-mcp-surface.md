# omuzaki-mcp: the full agent surface over MCP; omuzaki-ops-api: a finished job carries its outcome

- Every MCP connection is a registered session: opened at the handshake with the transport, the person and the grants as its ceiling, ended through the registry (`Disconnected`, `IdleTimeout`, `Revoked`), and closed when the registry ends it first (a revoked credential also revokes the HTTP token). Writes carry `base`, so a person's edit between an agent's read and write is a conflict.
- New tools with a registry: `session_info`, `session_list` and `approval_wait`. A `waiting` result says what the person is asked, a paused session's calls are held for 20 s, a short rate limit slows the call, and the agent's own approval outcomes arrive once in the heads-up.
- Fence refusals name the holder and the fence; the guide tells a session its own fence.
- Long jobs are MCP tasks (progress, status, cancel), and a finished task renders what the blocking call returns. `JobState::Done` keeps the op's `Outcome` in memory (`JobState::done`, `with_outcome`, `outcome`; the variant is `#[non_exhaustive]`).
- `McpServer::connect_in_app` serves the built-in assistant over an in-memory stream; `McpOptions::with_roots` gives agents the project folder and the folders the person shared.
- New resources `omuzaki://metrics/live` and `omuzaki://library/<item>`, and four built-in recipes.
- Numbers an `f32` widened to `f64` reach agents as the `f32` spells them (0.6, not 0.6000000238418579); a result's heads-up includes every change committed before the call; a client whose `initialize` fails is never named.
