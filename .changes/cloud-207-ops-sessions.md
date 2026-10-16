# omuzaki-ops: agent sessions

- The executor keeps a registry of agent sessions (`Sessions`), each naming the person it works for, and a source of
  what each person may do (`Principals`, `Executor::with_principals`). An agent or plugin request needs an open,
  active session: without one it is refused (`Denied`, `NoSession`), and while a person has paused the session it is
  refused with `Waiting` (`Paused`). What a session may do is its ceiling, the person's grants and the request's,
  whichever is narrowest. Every op record of a session's agent carries `on_behalf_of`.
- Pause and resume work on a session and its sub-agents, and keep the session's open transaction (#207).
- `omuzaki mcp` opens one session per connection in the executor, working for the person running the command.
