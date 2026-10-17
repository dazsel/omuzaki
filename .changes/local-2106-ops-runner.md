# omuzaki-ops: the ops runner's executor side

- Sessions end for real: open transactions are rolled back (or kept as "(unfinished)" when that conflicts), pending
  approvals are withdrawn, jobs are cancelled. A person can undo a whole agent session as one step, or stop it, which
  commits its open transaction as "(stopped)" (#149, #357, #1218).
- A request a person could allow waits in the approval queue (missing scope, a confirmation, a budget, a team review)
  and runs exactly once when approved; with no approver, or from a script, it is refused with a hint (#316, #1003,
  #1095, #1146, #1283).
- Sessions have rate limits, budgets in exact micro-dollars with pause at the cap, fences, and the stale-write guard
  (#244, #403, #967, #356). Client names are cleaned once, when the session opens (#208).
- A person's undo covers their agents' steps, newest first, and pauses a running agent (#245).
- Every change goes to the journal (`Executor::with_journal`), and `Executor::restore` and `replay` bring the history
  back after a restart; the undo depth and the in-memory log are capped (#251, #155, #150).
- Undo, redo and rollback run the effects of Explicit ops outside the document; live actions (`Undo::None`) never
  enter undo (#1091, #139).
- Driver jobs edit as one transaction and may join the caller's; job ends are journalled, ids continue across a reopen,
  and `jobs()` lists the last 100 ended (#1092, #1602, #1603, #1593). macOS job threads run at the utility QoS
  (#1611).
- **Changed:** a one-op undo step is labelled by the op's summary, not its name, on every surface (#1662). A
  `Denied` for a missing scope carries `ErrorDetail::MissingScope`.
