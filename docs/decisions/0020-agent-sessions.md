# 0020: Agent sessions live in the executor; agents never exceed their person, never overwrite blind, and only a person approves (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** AG-02; spec v3 §6, §11, §12, §13, VC-10, VC-11; ADR 0013, ADR 0016, ADR 0018 (in flight). Design:
`docs/design/agent-sessions.md`.

## Context

Agents reach Omuzaki over MCP (stdio and HTTP), and the in-app assistant (AG-01) and sub-agents (AG-03) are coming.
AG-02 promises that every session is listed with its actor, scopes, fence and spend; that `broadcast`, `publish` and
`spend` need a per-session grant; and that one click pauses, revokes or undoes a whole session. Today:
- `omuzaki-mcp` keeps its own per-connection state: actor, one transaction, rollback on drop.
- The runner trusts `Request.grants`, and reads, subscriptions and the log take no grants.
- `ErrorCode::Denied` promises an approval queue that doesn't exist.
- Nothing stops an agent from overwriting a change the person made a moment ago without knowing.

## Decision

1. **The executor owns the session registry** (a `Sessions` trait next to `Runner`, both implemented by
   `omuzaki-ops`). Session checks run under the executor's single lock, so controls are atomic with ops.
2. **A session is one agent actor working for one person.** The executor builds the actor id
   (`<sanitised client>#<session id>`). The person (`on_behalf_of`) is named once by the transport when it opens the
   session, kept in the registry, and stamped on every op record. `Actor` itself does not change, which amends the
   contract line of `docs/design/ux/README.md` decision 1. Agent and plugin requests without a session are refused.
3. **Effective grants = the session's ceiling ∩ the person's grants ∩ the request's grants.** The person's grants come
   from a `Principals` source the app registers (VC-10 roles later). Only the person can widen a ceiling. "For this
   project" grants attach to a credential (an HTTP token, the assistant's settings, a role file), never to a client
   name.
4. **Transactions stay owned by the actor that began them.** Ending a session (disconnect, idle timeout, revoke,
   project close) rolls its open transactions back, newest first; one that conflicts is kept as an "(unfinished)"
   step and reported. There is no public `Runner::abandon`; this is implemented inside the executor.
5. **Concurrency without locks:**
   - The person is never refused, held or rate-limited for an agent's sake.
   - Agents are guarded against stale writes: a request carries `base`, the newest document version its sender has
     seen. An edit to a path another actor changed after it is a `Conflict` naming who, and `omuzaki-mcp` fills `base`
     from the heads-up cursor automatically.
   - AG-04's fences are leases for the session's lifetime, binding agents only. A person editing a fenced part
     sees a dismissible inline note, never a modal, and still wins.
6. **Controls are the person's:** pause (the soft option: the open transaction stays open, calls are refused with
   `Waiting`), resume, stop, revoke, and undo-session. Revoke also revokes the session's credential by default, so the
   agent can't come back unless the person grants it again. Undo-session reverts the
   session's steps newest first, skips and reports conflicts, and records the result as one step in the person's
   history. A person's Ctrl+Z covers their agents' steps and pauses a running one first.
7. **One approval queue** for a missing scope, an op that calls `ctx.require_person`, a budget, and (VC-11) a
   reviewer:
   - It is used only for requests whose dry-run succeeds.
   - The agent gets `ErrorCode::Waiting`.
   - A person resolves it exactly once, in Omuzaki's own window. MCP elicitation, the CLI and agents can never
     approve.
   - The approved request runs as its own transaction, with the approver recorded.
   - Nothing times out.
   - With no approver present (headless `omuzaki mcp`), the request is refused with a hint instead. Headless use
     (scripts, CI) runs from grants the person set in advance on a credential.

   Model switches (ADR 0016) and audio egress (ADR 0018) use `require_person`. Device binding is
   `OpError::person_only`, never queued.
8. **Budgets and limits on the session:**
   - **Money** in integer micro-dollars, 0 unless the person sets a cap.
   - **Tokens** charged by whoever pays for the LLM.
   - **Active time.**
   - **Token-bucket rate limits** on the runner's clock (`ErrorCode::Limited`), never applied to persons.

   Passing a cap pauses the session and asks the person.
9. **The runner's events are the audit log,** journalled by VC-08:
   - every document event names its actor and resulting version;
   - session and approval events record who, for whom, what and why.

   The Build Theatre reviews a session from them and re-runs it as a `Replay` session, which is subject to the same
   grants and approvals.
10. **One path for every agent:**
    - The in-app assistant is an in-process MCP client of the same server.
    - `omuzaki mcp` over stdio bridges to the studio when the studio holds the project. The first connection of a new
      credential needs one click in Omuzaki's window (pairing, remembered per credential); agents can't pair
      themselves.
    - Otherwise `omuzaki mcp` runs headless with person-typed grants.

## Consequences

- One contract change in `omuzaki-ops-api` (design section 11) closes the missing grants on reads, subscriptions and the log, and carries the types for abandon, approvals and the grant check. The rest is executor work. Ten new tasks cover it (design
  section 12): the guard, limits, budgets, undo-session, undo per principal,
  the MCP wiring and tools, the stdio bridge and a CLI listing.
- `omuzaki-mcp` stops keeping its own actor serial and drop-rollback, and asks the registry instead.
- Ops gain a declared "needs a person" rule (`OpCtx::require_person`), which ADR 0018 was waiting for.
- `ErrorCode` grows `Waiting` and `Limited`. Agents and the CLI's exit codes must handle both.
- Rejected:
  - a wrapper service in front of the runner, which protects only the surfaces that use it;
  - `Actor::Agent { on_behalf_of }`, which can be forged;
  - path locks, which make the person wait on an agent;
  - approval through elicitation or the CLI, where the agent can answer for itself;
  - floating-point money.
- The open questions were decided on 2026-10-12 (design section 14):
  - revoke revokes the credential by default;
  - new bridge credentials pair with one click;
  - the rate-limit defaults stand until measurement says otherwise;
  - a fenced part gets a dismissible inline note.
