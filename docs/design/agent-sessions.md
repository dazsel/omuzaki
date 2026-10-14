# Agent sessions: who an agent is, what it may do, and how a person stays in charge

**Status:** accepted (2026-10-12; section 14 records the answers to the open questions). **Feature:** AG-02. **Decision:** ADR 0020.
**Spec:** v3 §6 (ops, transactions, scopes), §11 (MCP), §12 (agents), §13 (Build Theatre), §29 (VC-10, VC-11).
**Builds on:** `crates/omuzaki-ops-api` (`Runner`, `Actor`, `Grants`, `Event`), `crates/omuzaki-ops` (the executor),
`crates/omuzaki-mcp` (sessions per connection, HTTP tokens and limits, the "others changed" heads-up),
ADR 0013 (file roots), ADR 0016 (user models), ADR 0018 (audio, in flight), `docs/design/ux/shell.md` §9 to §11.
**Settles together:** abandoning a session's transactions, scope checks on reads, approvals and `on_behalf_of`.

This page explains the decisions so an implementer or reviewer can disagree with a reason. The types will be
documented in the code; section 11 lists them exactly once, for the contract task.

## 1. What this settles

| Question | Answer in one line | Section |
|---|---|---|
| What is a session? | A record in the executor: an agent identity, the person it works for, grants no wider than that person's, a budget, limits and a state | 3 |
| Who owns a transaction? | The actor that began it, as today. A session that ends rolls its open ones back; one that can't be rolled back is kept and reported | 4 |
| Is `Runner::abandon(actor)` needed? | No. Ending a session abandons internally; that becomes executor work | 4.5 |
| Several agents and a person at once? | Persons first and never refused for an agent's sake; agents never overwrite blind (a stale-write guard); no locks; fences (AG-04) are leases with the session's lifetime | 5 |
| Pause, revoke, undo a whole session? | Executor controls, person-only, atomic under the executor's lock | 6 |
| How does an agent ask and a person confirm? | One approval queue for missing scopes, "needs a person" ops, budgets and (later) reviews; resolved only in Omuzaki's own UI | 7 |
| Rate and cost limits? | Token buckets and a spend counter on the session, enforced by the executor on the runner's clock | 8 |
| Audit and replay? | The runner's events, journalled (VC-08), are the audit log; the Theatre replays a session from them | 9 |
| MCP and in-app agents? | One path: the in-app assistant is an in-process MCP client of the same server | 10 |

## 2. The shape

```
 Claude Code (stdio) ──► omuzaki mcp ──bridge──┐
 Codex (HTTP token) ───────────────────────┐ │
 in-app assistant (in-process MCP) ──────┐ │ │
                                         ▼ ▼ ▼
                                omuzaki-mcp Session ── opens/ends ──► Sessions ┐
                                   │ Request{actor, base}                    │ one executor,
 the person at the UI ── Request ──┼──────────────────────────────► Runner ──┤ one lock
 (and the CLI)                     │                                         │ (omuzaki-ops)
                                   ◄──────────── Event stream ───────────────┘
                                   │
            Agent panel, approval cards, History, Build Theatre, journal (VC-08), MCP heads-up
```

- **The executor owns the registry.** `omuzaki-ops::Executor` implements a new trait, `Sessions`, next to `Runner`.
  Session checks (paused? ended? grants? budget? rate?) then happen under the same lock as plan-and-apply, so a
  revoke in the middle of a call either lands before it or after it, never half-way. A separate `Sessions` service
  in front of the runner was rejected: the UI and CLI call the runner directly, so a wrapper would protect only the
  surfaces that remember to use it, which is the problem again.
- **Surfaces present; the executor decides.** `omuzaki-mcp` turns a session's state into words an agent can act on,
  holds a call briefly while a session is paused, and retries a rate-limited call. It never decides what is allowed.

## 3. What a session is

### 3.1 The record

`SessionInfo`, listed by the Agent panel, `session_list` over MCP and `omuzaki session list`:

| Field | Meaning |
|---|---|
| `id` | `SessionId`, allocated by the executor, unique within the project's history (continues after the journal's highest, once the journal persists it) |
| `actor` | `Actor::Agent { id: "<client>#<id>", model }`, built by the executor from the sanitised client name and the id. Transports use this actor and never make their own |
| `on_behalf_of` | The person (an `Actor::Person`) the session works for: the person who issued the HTTP token, the person running the studio for stdio and in-app sessions |
| `transport` | `Stdio`, `Http { token }` (the token's label, never its secret), `InApp`, `Plugin`, `Automation`, `Replay` |
| `client`, `model` | Sanitised client name; the model when the client says (the in-app host always knows it) |
| `parent`, `role` | For sub-agents (AG-03): the spawning session and the role file's name; empty otherwise |
| `grants` | The effective grants (3.3) |
| `fence` | Document paths the session may edit. Empty means the whole project until AG-04 defines fences |
| `budget`, `spent`, `limits` | Section 8 |
| `state` | `Active`, `Paused { by, reason }` or `Ended { reason, at }`. "Working", "idle" and "waiting for you" are derived: from `last_active_at` (idle after 60 s without a call) and `pending` |
| `open_tx` | The session's open transaction and its label, if any |
| `pending`, `steps` | Pending approvals; committed transactions so far |
| `started_at`, `last_active_at` | On the runner's clock |

### 3.2 Identity and the principal

- **One session, one agent actor.** `Actor` equality is how transactions and histories are owned today, so the
  session's actor is the identity everything hangs on. Two connections from the same client are two sessions and two
  actors (as now: `claude-code#1`, `claude-code#2`).
- **The principal lives in the registry, not in `Actor`.** `docs/design/ux/README.md` decision 1 proposed
  `Actor::Agent { on_behalf_of }`. Rejected: any code that builds an `Actor` could then name any principal (the trust
  hole describes), and every actor comparison would compare principals too. Instead the transport names the
  principal once, when it opens the session, from what it has authenticated (the token's issuer, the studio's
  person), and the executor stamps `on_behalf_of` onto every `OpRecord` it writes. History and replay stay
  self-contained without trusting requests.
- **Agents and plugins must have a session.** The executor refuses a request from an `Actor::Agent` or
  `Actor::Plugin` that has no open session (`Denied`, detail `NoSession`). Persons need none. `Actor::System` stays
  Omuzaki's own (migrations) and needs none. Automations (BR-11) and scripts (PL-02) may open sessions
  (`Automation` with the person who configured them as principal; scripts run as the person today); BR-11 and PL-02
  decide.

### 3.3 Grants: never wider than the person (VC-10)

```
 effective = session ceiling  ∩  principal's grants  ∩  Request.grants
             (token / launch      (Principals source,    (a transport may narrow,
              grants + approvals)  VC-10 roles later)     never widen)
```

- **Persons** have no session: their effective grants are `Request.grants ∩` their principal's grants. The
  transport still builds `Request.grants` server-side (the CLI adds the roots of paths the person typed, ADR 0013),
  and the `Runner` docs say it is trusted transport input for persons only.
- **Ceiling.** What the session was given when it opened (`Grants::agent_default()` plus whatever the token or launch
  named, plus the project folder and roots the person delegated, ADR 0013), widened only by a person: an approval
  "for this session" or "for this project" (7.1) or the grants popover (`Sessions::set_grants`).
- **Principal's grants** come from a `Principals` source the app registers with the executor (contract, section 11).
  Phase 1 has one local person holding `Grants::person()`. VC-10 roles narrow it later without touching sessions.
  Scopes intersect. File roots are not intersected (a person holds `fs` with no roots, ADR 0013); instead only a
  person can add a root to a session, and only roots ADR 0013 allows.
- **Who may change a session's grants:** its principal (later, a VC-10 admin), only up to their own grants. Never an
  agent, including the session itself.
- **Per-session grants** (approved "for this session") end with the session. **Project grants** ("for this project")
  attach to a **credential**, never to a self-declared name: an HTTP token's stored record, the in-app
  assistant's settings, a role file (AG-03), a paired bridge credential (10.2). A headless stdio session started
  without a credential has none, so "for this project" is not offered for it. Client names are chosen by the client, so a grant keyed on one would go to anyone who claims it.

### 3.4 Lifetime and states

```
            open                pause (person, budget, undo, step)
 (none) ─────────► Active ◄──────────────────────────────► Paused
                     │            resume (person) / budget raised │
                     └──────────────┬──────────────────────────────┘
                                    ▼ end: disconnected, idle timeout, closed, revoked, project closed
                                  Ended   (kept in the registry for History, Theatre and per-principal undo)
```

- **Opens** when a transport admits a client: MCP `initialize`; the in-app assistant's first message; a plugin load.
  A bridge connection with a credential the studio has not paired yet opens `Paused { reason: Pairing }` (10.2).
- **Ends** on: stdio EOF or HTTP `DELETE` (`Disconnected`), the HTTP idle timeout (`IdleTimeout`, 30 min), the
  client closing it (`Closed`), a person's revoke (`Revoked { by }`), or the project closing (`ProjectClosed`).
- **On end, atomically:** the session's open transactions are abandoned (4.2), its pending approvals are withdrawn
  (they do not run after the agent is gone), its per-session grants are dropped, and `SessionEnded` reports all of it.
- **Ended records stay** for the life of the executor. The journal (VC-08) keeps them across restarts; live sessions
  never survive a restart, because their connections don't.

### 3.5 Room left for later features

- **Sub-agents (AG-03):** `SessionSpec.parent`. A child's ceiling must be within the parent's effective grants, its
  spend counts toward the parent's budget, and pausing, revoking or ending the parent does the same to its children.
- **Fences (AG-04):** `SessionInfo.fence`, see 5.3.
- **Pause modes (TH-06):** `PauseReason::Step` is set by the executor when a session's pause mode says so (after each
  transaction, plan item or gate). TH-06 adds the mode; the state machine already has the reason.
- **Replays (TH-12):** `Transport::Replay`, see 9.2.

## 4. Transactions and ownership

### 4.1 Commit and rollback: as today

`Runner::commit(tx, actor)` and `Runner::rollback(tx, actor)` take the actor and refuse anyone but the owner
(already landed). Nothing changes, except that a paused session's calls are refused (4.3) and events now name the
actor (section 11).

### 4.2 Ending a session abandons its open transactions

When a session ends, the executor, under its lock:
1. Rolls back each open transaction the session's actor owns, **newest first**.
2. If a rollback conflicts (someone changed the same fields since), that transaction is **committed as it is**, with
   " (unfinished)" added to its label, into the agent's history, and reported in `Ended.kept` with the conflict
   (path, who changed it). Leaving it open was rejected: nobody could ever close it. Reverting part of it was
   rejected: a half-reverted step is harder to understand than a kept one, and undo-session (6) can still try later.
3. A conflict on one never stops the others; other actors' transactions are untouched.
4. Emits `TxRolledBack` or `TxCommitted` for each, then `SessionEnded`.

The MCP `Session`'s own `Drop` rollback is replaced by `Sessions::end(id, Disconnected)`.

### 4.3 Pause and the open transaction

A paused session's transaction **stays open**. While paused, everything the session would run is refused with
`Waiting` (detail `Paused`): `run` (queries too, since `render.*` queries cost GPU time and run under the executor's
lock), `begin`, `commit`, `rollback`, `undo` and `redo`. Reading the document, the log and its own session record
still works. The person can change things during the pause; the agent finds out through the heads-up (5.2) and the
stale-write guard (5.1) when it resumes.

### 4.4 An approved request runs as its own transaction

A request that waited for approval (7.1) runs, once approved, as a one-op transaction of the agent's own, even if
it was sent inside an open transaction. By then the transaction may be committed or gone, and an approved effect
(a publish, a model switch) should not disappear because the agent later rolls back something else.

### 4.5 No public `Runner::abandon(actor)`

asked whether every surface needs one call to drop what an actor holds. With the registry in the executor, the
callers it listed are covered: MCP and the in-app assistant (sessions, through `Sessions::end`), the AG-02 controls
(`end`, `undo_session`), plugins and automations (sessions). Scripts run as the person: an `abandon(person)` would also
throw away the person's own open transaction in the UI, so it would be wrong there anyway. Abandoning is therefore
internal to the executor, and the abandon work is re-cut to implement 4.2.

## 5. Concurrency: several agents and a person on one project

### 5.1 The policy

1. **The person first.** A person's edits are never refused, held or rate-limited because of an agent. The executor's
   single lock serialises everything; agents' rate limits (8.2) keep them from starving the UI.
2. **No blind overwrites by agents: the stale-write guard.** A request may carry `base: Option<DocVersion>`, the newest
   document version its sender has been told about. If the planned patch touches a path (or an ancestor or
   descendant of it) that **another actor** changed after `base`, the executor refuses with `Conflict`, detail
   `Changed { path, by, version }`, hint "re-read it; your next call carries the newer base". `omuzaki-mcp` sets `base`
   from the session's heads-up cursor automatically, so agents get this protection without doing anything.
   Retrying after reading is an informed overwrite and goes through. The executor keeps a path index
   (`DocPath` → last version and actor), updated on every applied patch, undo, redo, rollback and session undo.
   Persons' requests carry no `base`. A request without `base` is not guarded (scripts, tests, older transports).
3. **Same-path conflicts on undo** keep today's rule: the step is dropped from that actor's history and the `Conflict`
   names who changed the path (`Runner::undo`).
4. **Two agents** follow rule 2 between themselves: whoever writes second with a stale view is refused, with the
   other's name in the message.

Locks were rejected for phase 1: a person would have to wait on an agent (or break its lock), and agents would hold
locks across LLM round trips of tens of seconds. Last-writer-wins without a guard was rejected: §12 promises "never
silent overwrites".

### 5.2 The "others changed" feed

already tells an agent, on every result, what others changed since its last call (grouped, capped, own edits
filtered out, positioned by the log's `n`). This design keeps it and makes three changes:
- events now carry the actor and the resulting `DocVersion` (section 11), so the digest no longer infers who undid
  or rolled back what, and the cursor maps to a `base` exactly;
- the in-app assistant gets the same feed, because it is an MCP client (10.1);
- the agent's own approval outcomes are listed too ("your request to export the VRM was approved by Sam and
  applied", or "rejected: 'not yet'"), since the agent did not apply them itself.

### 5.3 Locks and leases: none now; fences later

AG-04's fences are the only exclusive claims, and they bind agents only:
- a fence is a set of document paths a session claims; two agents can't claim overlapping paths;
- the claim is a **lease with the session's lifetime**, not a wall-clock timer: it ends when the session ends (idle
  timeout included), is revoked, or the fence is released;
- an agent's op outside its fence is refused naming the holder;
- a person is never stopped by a fence. An agent whose fenced path the person changed gets the stale-write
  `Conflict` from 5.1, which is the review item §12 asks for;
- the person sees a **non-blocking inline note** where they edit a fenced part ("an agent is working on this part;
  your edit takes precedence and it will be told"), never a modal. It can be dismissed for the rest of that session.

### 5.4 Undo across actors

- **Agents undo their own steps only** (`history_undo` over MCP), as now.
- **A person's Ctrl+Z undoes their own latest step or their agents' latest, whichever is newer** (undo per
  principal, `docs/design/ux/README.md` decision 1). If that step belongs to an active session, the session is paused
  first (`PauseReason::Undo`), so the agent can't keep committing on top while the person steps back. A new commit by
  the person or any of their agents clears the principal's redo.
- **Undo a whole session** is section 6.

## 6. Controls: pause, resume, stop, revoke, undo session

All are `Sessions` methods. `by` must be a person, the session's principal (VC-10 adds admins). Each is atomic under
the executor's lock and emits an event; the panel and the Theatre update from events only.

| Control | Effect | Event |
|---|---|---|
| **Pause** | State `Paused { by, Person }`; open transaction kept; calls refused or held (4.3) | `SessionChanged` |
| **Resume** | Back to `Active`. A session paused for its budget resumes only once the budget allows (8.1) | `SessionChanged` |
| **Stop** | Pause, plus cancel the session's running jobs once the job manager exists. The always-visible button in the panel | `SessionChanged` |
| **Revoke** | Ends the session (`Revoked { by, credential }`): abandon (4.2), withdraw approvals, drop grants. **By default it also revokes the session's credential** (HTTP token or paired bridge credential), so the agent can't come back unless the person grants it again; the confirm dialog names the other sessions on that credential, which end too, and the box can be unticked. Pause is the soft option | `SessionEnded` |
| **Undo session** | Pauses the session if active (`PauseReason::Undo`), abandons its open transactions, then reverts each of its committed steps still applied, **newest first**. A step whose revert conflicts is skipped and reported (path, who changed it), like `Runner::undo`. The reverted steps become **one step in the person's history**, labelled "Undo Designer's session (12 steps)", so Ctrl+Z brings the agent's work back. The agent's own undo and redo stacks are cleared. Steps with effects outside the document (`Undo::Explicit`) are listed in `outside` until can undo them | `SessionUndone`, then `TxCommitted` for the new step |

## 7. Approvals and confirmation flows

### 7.1 One queue for every "ask a person"

A request from a session is **queued** instead of refused when a person could allow it:

| Need | When | Decisions offered |
|---|---|---|
| `Scope { scope }` | The op's scope is missing from the session's grants, but the principal holds it | Approve (this request), Approve for session, Approve for project (credential sessions only, 3.3), Reject with comment |
| `Person { reason }` | The op called `ctx.require_person(reason)` and no person is acting or confirming (7.3, 7.4) | Approve, Reject with comment |
| `Budget { needed, left }` | Running it, or the session's spend or time, would pass its budget (8.1). The session is paused too | Approve once, Raise budget, Reject (the session stays paused) |
| `Pairing { client }` | A bridge connection presents a credential the studio has not paired (10.2). No request; the session waits paused | Approve = Pair (remembered for that credential), Reject = Refuse (the session ends, revoked) |
| `Review { policy }` | VC-11 (phase 5): a team policy needs a reviewer for `publish` or a `broadcast` grant. Reserved now | Approve, Reject (by a person with the reviewer role) |

How it runs:
1. Before queueing, the executor **dry-runs** the request (that needs only `read`). If the dry-run fails
   (validation, the licence gate VF-08), that error is returned and nothing is queued: a person is only asked about
   requests that would otherwise succeed. The dry-run's plan (patch, output, cost) is the card's preview.
2. The request is queued as a `PendingApproval` (id, session, actor, op, args, cause, need, preview, batch key,
   time) and `ApprovalRequested` is emitted. The agent gets `ErrorCode::Waiting`, detail
   `Queued { approval, need }`: "nothing ran; a person must decide; don't retry".
3. **Resolve** (`Sessions::resolve(id, decision, by)`) takes the approval out of the queue under the lock, so it
   resolves **exactly once**. On an approval it re-plans and runs the request against the current document as the
   agent (4.4), with the need satisfied for that one run: the scope, the confirmation (`OpCtx::confirmed_by`), or
   the budget. "For session" and "for project" also widen the ceiling; "raise budget" sets the new budget and
   resumes a budget-paused session. The result (applied outcome, failure, rejection with its comment, or
   withdrawal) is in `ApprovalResolved` and reaches the agent (5.2, `approval_wait`).
4. Needs are resolved one at a time: if the approved run then hits a second need (a scope was approved, then the op
   asks for a person), that becomes a new approval and the resolution says so.
5. **No timeouts.** An approval never resolves on its own. It is withdrawn only when its session ends.
6. **Batching:** the batch key is (session, op, need). Identical keys collapse into one card ("6 similar steps:
   Approve all"), and the UI resolves each id exactly once.
7. **Bounded:** at most `limits.max_pending` (16) per session; past that, `Limited`.

### 7.2 Who may resolve, and through what

- **A person, in Omuzaki's own window.** Resolving needs `Actor::Person`, the session's principal (VC-10: or an
  admin), holding the needed scope. Agents can never resolve, including for other sessions.
- **Never through the agent's own channel.** MCP elicitation is answered by the client, which the agent controls, so
  it is not a way to approve or grant; Omuzaki may use it only to say "waiting for you in Omuzaki". There is no CLI or
  MCP command that approves or grants: Claude Code and Codex have a shell, so such a command would let an agent
  approve itself.
- **The honest boundary:** an agent that has the person's shell can run the `omuzaki` CLI as the person. Sessions guard
  what agents do through MCP and the in-app assistant; they don't make a shell-holding agent less than the person.
  One writer per project means a headless CLI can't edit a project the studio holds.

### 7.3 Model switches (ADR 0016)

`model.select` by an agent, to a model the person has not acknowledged, calls `ctx.require_person(..)` with the
ADR 0016 disclaimer and the model's declared licence as the reason. The card shows it; approving runs the op with
`confirmed_by` = that person, and the op records the acknowledgement (per model hash) as theirs. A switch to an
acknowledged or shipped model needs no one. "`needs_person` result" is this `Queued { need: Person }`.

### 7.4 Audio routing (ADR 0018)

ADR 0018 keeps its checks in the ops "until ops-api grows a declared person-only rule". This is that rule:
- an edit that would grow the set of (captured source → egress) pairs calls `ctx.require_person("this would send
  <mic> to <program_out>")`: queued, and approved by the person, it runs;
- device binding and unbinding are **person-only**, not confirmable: they return `OpError::person_only(..)` (`Denied`,
  detail `PersonOnly`), never queued, hint "only a person can do this, in Settings > Audio";
- making things quieter or more private never asks.

### 7.5 Publish and broadcast (VC-11, UX decision 5)

- Phase 1: an agent without `publish` or `broadcast` gets a `Scope` approval. Granting either, for a request or a
  session, uses the two-step confirm (`shell.md` §10). "Go live" by an agent needs a per-session `broadcast` grant
  from a person, always.
- Phase 5 (VC-11): a team policy adds the `Review` need for `publish` ops and `broadcast` grants, for agents and
  people alike. The licence gate runs first (in the dry-run, step 1 of 7.1). VC-11's "approvals are ops with a log"
  is met by the journalled approval events plus `OpRecord.approval`, which links the applied op to its approval and
  approver (9.1).

### 7.6 No approver present (headless)

`omuzaki mcp` run without the studio (10.2) has no window to approve in. The executor is built with no approver, and a
request that would queue is refused instead: `Denied` with the need as detail and a hint ("use a credential that
grants `publish`, or open the project in Omuzaki Studio").

**Headless works from grants set in advance.** The person issues a credential in the studio (a token) with
the grants and budget a script or CI job needs; its record (a keyed hash, never the secret) is stored with the
project. Headless `omuzaki mcp` given that secret (an environment variable or a file, never a flag that lands in shell
history) opens its session with that credential's grants and budget as the ceiling, and runs without an approver:
anything beyond them is refused, never queued. Without a credential it gets `Grants::agent_default()` plus the
person-typed `--grant` and `--spend-cap`, like ADR 0013's typed paths.

## 8. Budgets and rate limits

### 8.1 Budget (TH-10, AG-06)

- `Budget { usd_micros, tokens, seconds }`: money (integer micro-dollars, so sums are exact and deterministic),
  LLM tokens, active wall time. Money defaults to **0**: a session that holds `spend` must be given a cap (the
  popover's "$0 of [2.00]"). Tokens and time default to no cap.
- `Spend` counts the same three. **Money** is charged at apply from the plan's `CostHint.usd`, rounded up to the
  micro-dollar; jobs correct it with the actual cost once the job manager reports it. **Tokens** are charged by
  whoever pays for the LLM through `Sessions::charge`: the in-app host does (it calls the model); Omuzaki can't see an
  external agent's tokens, so the meter shows "—" for them. **Time** is the runner clock since `started_at`, minus
  time paused.
- **Enforcement:** before applying, if `spent + cost` would pass the cap, or time or tokens already have, the session
  is paused (`PauseReason::Budget`) and a `Budget` approval is queued (with the request, if one triggered it). A
  `charge` that crosses a cap does the same with no request. AG-06's per-role caps reuse this counter.
  *Amended by ADR 0040:* a host that calls an LLM bounds each turn before sending it; a bound that does not fit
  pauses the session with a `Budget` approval through `Sessions::request_budget`, so nothing is sent (`agent-runtime.md` D2).

### 8.2 Rate limits

Per session, enforced by the executor on the runner's clock (`FakeClock` in tests, so deterministic), never on
persons:

| Limit | Default | Counts |
|---|---|---|
| `edits_per_minute`, `edit_burst` | 600, 100 | applied edits and jobs, `begin`, `commit`, `rollback`, `undo`, `redo` |
| `runs_per_minute`, `run_burst` | 3000, 300 | queries and dry-runs (they plan under the executor's lock) |
| `max_pending` | 16 | queued approvals |
| open transactions | 1 | as MCP has today |

Over a limit: `ErrorCode::Limited`, detail `RetryAfter { seconds, limit }` naming the limit and its value, like
HTTP refusals. `omuzaki-mcp` waits and retries transparently for up to 2 s, so a well-behaved agent never sees
it; a runaway loop is slowed, then told. The defaults are accepted (2026-10-12). A measurement run checks them against
a realistic agent session, records the numbers, and changes them there if the data says so. The HTTP limits stay where they are: they protect the port, these protect the document and the
person's frame time.

## 9. The audit log and the Build Theatre

### 9.1 What is recorded

The **runner's event stream is the audit log.** The VC-08 journal writer already persists it, in order, next
to the op log. After this change it holds:
- every document change: `OpApplied` (the record now with `on_behalf_of` and `approval`), `TxCommitted`,
  `TxRolledBack`, `Undone`, `Redone`, all naming the acting actor and the resulting `DocVersion`;
- every session fact: `SessionOpened` (who, for whom, how, with what grants and budget), `SessionChanged` (grants and
  budget changes with who made them, pauses, resumes, spending), `SessionEnded` (why, what was abandoned or kept,
  which approvals were withdrawn), `SessionUndone`;
- every approval: `ApprovalRequested` (with its preview) and `ApprovalResolved` (who, which decision, the comment,
  the outcome).

That covers VC-10's "every grant and change goes in an audit log" and VC-11's approval log. Queries are not logged
(position): the log is the history of changes and decisions.

### 9.2 Replaying a session

- **Review (TH-02, TH-07):** the Theatre's cards for a session are the journalled events with that session's actor
  or id, grouped by transaction: label, cause, actor, duration (first op to commit), approvals inline. Before and
  after thumbnails are renders of the document at the step's first version minus one and its last version, rebuilt
  from VC-08 snapshots plus patches. Rewinding uses the same versions.
- **Re-run (TH-12):** a new session with `Transport::Replay`, on behalf of the person replaying, re-issues the
  original session's still-applied transactions **as requests** (op and arguments, `CauseKind::Replay`), in order,
  each in a transaction with its original label. Undone and rolled-back transactions are skipped, which the new
  `Undone` and `TxRolledBack` fields make exact. It goes through the same executor, so a replayed `publish`
  needs a grant or approval again, and its spend counts against the replay's budget. Re-running "with different
  choices" substitutes arguments (seeds, references) before each request. Replaying requests rather than patches is
  what makes different choices possible; exact reconstruction (recovery) folds patches instead.

## 10. One path for MCP and in-app agents

### 10.1 The in-app assistant is an MCP client

AG-01 already says the assistant "sees the selection, viewport and recent ops through the same MCP resources an
external agent gets". So it **connects to the same `omuzaki-mcp` server in-process** (an rmcp duplex transport), opening
a session with `Transport::InApp`. It gets the same tools, results, heads-up, stale-write guard, approvals, pause and
limits, with no second implementation. Its host adds only what an external client does itself: running the LLM,
charging tokens and money to the session (`Sessions::charge`), and waiting on an approval inside a tool call, since it
controls its own loop. Sub-agents (AG-03) are more in-process sessions with `parent` set.

*Amended by ADR 0040:* the session's role is `assistant`, and one conversation is one session: the app opens it at the
first message, and New conversation ends it (`Closed`). Stop (section 6) becomes `Sessions::stop`: it pauses and
cancels jobs atomically, lets cancelled driver jobs revert their edits, then commits the open transaction in any
state, a paused one included (`agent-runtime.md` D4).

### 10.2 stdio while the studio is open

`omuzaki mcp` is a separate process started by the client. If it opened the project itself while the studio has it
open, there would be two writers and two documents. So:
- **Studio running with the project:** `omuzaki mcp` becomes a **bridge**. It relays stdio to the studio's loopback
  port, and the studio opens a `Stdio` session for it on behalf of the studio's person, with
  `Grants::agent_default()` plus the project root.
- **Pairing.** The first connection of a new credential needs one click in Omuzaki's window. The bridge connects with
  its own credential if it has one (stored per project and client in the user's config folder, owner-only), or asks
  for a new one. A new or unknown credential opens its session `Paused { reason: Pairing }` and queues a
  `Pairing { client }` approval. The card ("Claude Code wants to work on this project") offers Pair or Refuse.
  - Pair resumes the session and marks the credential paired, so later connections with it need no click.
  - Refuse ends the session as revoked.
  - Agents can't pair themselves: pairing is resolved only in the studio's window (7.2).
  - Until paired, the agent's calls get `Waiting` (held up to 20 s like any pause), and only `session_info` and
    `approval_wait` answer.
  - Revoking the session revokes the credential (6), so the next connection must pair again.
- **No studio:** `omuzaki mcp` runs headless with its own executor and no approver, from a credential's grants set in
  advance, or from agent defaults plus `--grant` and `--spend-cap` (7.6).

### 10.3 What each transport does

| | stdio (bridge) | stdio (headless) | HTTP | in-app |
|---|---|---|---|---|
| Principal | studio's person | the OS user | the token's issuer | studio's person |
| First connection | one click to pair the credential | none | the person issued the token | none |
| Ceiling | agent default + project root, widened on the credential | the credential's grants, else agent default + typed `--grant` | the token's grants | `read`, `edit:*`, `render` (AG-01) |
| Approvals | yes | no: refused with a hint | yes | yes, waited for inside the tool call |
| "For this project" grants | yes (the paired credential) | only as set in advance on a credential | yes (the token) | yes (assistant settings) |
| Ends on | EOF, credential revoked | EOF | `DELETE`, idle 30 min, token revoked | project closed, person revokes |

## 11. The contract change (for the contract task)

All in `omuzaki-ops-api`, one change, reviewed by a maintainer. Names are binding; doc comments are the implementer's.

**New `session` module.**
- `SessionId(pub u64)`; `Transport { Stdio, Http { token: String }, InApp, Plugin, Automation, Replay }`.
- `SessionSpec { transport, client: String, model: Option<String>, on_behalf_of: Actor, grants: Grants, budget:
  Budget, limits: Limits, parent: Option<SessionId>, role: Option<String> }`.
- `SessionInfo` with the fields of 3.1; `SessionState { Active, Paused { by: Actor, reason: PauseReason }, Ended {
  reason: EndReason, at: Seconds } }`; `PauseReason { Person, Budget, Undo, Step, Pairing }`; `EndReason {
  Disconnected, IdleTimeout, Closed, Revoked { by: Actor, credential: bool }, ProjectClosed }` (`credential`: the
  transport also revokes the session's credential).
- `Budget { usd_micros: u64, tokens: Option<u64>, seconds: Option<u64> }`, `Spend { usd_micros: u64, tokens: u64,
  seconds: u64 }`, `Limits { edits_per_minute: u32, edit_burst: u32, runs_per_minute: u32, run_burst: u32,
  max_pending: u32 }` with `Default` as in 8.2.
- `Ended { abandoned: Vec<TxId>, kept: Vec<KeptTx>, withdrawn: Vec<ApprovalId> }`, `KeptTx { tx, label, error:
  OpError }`; `SessionUndo { step: Option<TxId>, reverted: Vec<TxId>, skipped: Vec<KeptTx>, abandoned: Vec<TxId>,
  outside: Vec<TxId> }`.
- `trait Principals: Send + Sync { fn grants(&self, person: &Actor) -> Grants; }`.
- `trait Sessions: Send + Sync`:
  `open(SessionSpec) -> Result<SessionInfo, OpError>`, `sessions() -> Vec<SessionInfo>`,
  `session(SessionId) -> Option<SessionInfo>`, `session_of(&Actor) -> Option<SessionInfo>`,
  `pause(SessionId, by: &Actor) -> Result<SessionInfo, OpError>`, `resume(..)`,
  `set_grants(SessionId, Grants, by: &Actor) -> Result<SessionInfo, OpError>`, `set_budget(SessionId, Budget, by)`,
  `charge(SessionId, Spend) -> Result<SessionInfo, OpError>`, `end(SessionId, EndReason) -> Result<Ended, OpError>`,
  `undo_session(SessionId, by: &Actor) -> Result<SessionUndo, OpError>`, `approvals() -> Vec<PendingApproval>`,
  `resolve(ApprovalId, Decision, by: &Actor) -> Result<Resolution, OpError>`.

**New `approval` module.**
- `ApprovalId(pub u64)`; `Need { Scope { scope }, Person { reason: String }, Budget { needed: Spend, left: Spend },
  Pairing { client: String }, Review { policy: String } }`.
- `PendingApproval { id, session: SessionId, actor: Actor, request: Option<ApprovalRequest>, need: Need, batch:
  String, requested_at: Seconds }`, `ApprovalRequest { op: String, args: Value, cause: Option<Cause>, preview: Plan }`.
- `Decision { Approve, ApproveForSession, ApproveForProject, RaiseBudget { budget: Budget }, Reject { comment:
  Option<String> } }`.
- `Resolution { id, by: Option<Actor>, result: Resolved, at: Seconds }`, `Resolved { Applied { outcome: Outcome },
  Failed { error: OpError }, Rejected { comment: Option<String> }, Withdrawn { reason: EndReason } }`.
- `ApprovalRef { id: ApprovalId, by: Actor }`.

**Errors.**
- `ErrorCode::Waiting` (nothing ran; a person must act first; don't retry) and `ErrorCode::Limited` (a session
  limit; retry after the stated time).
- `OpError.detail: Option<ErrorDetail>` (serde default, skipped when empty), `ErrorDetail { MissingScope { scope },
  NeedsPerson { reason }, PersonOnly { reason }, Queued { approval: ApprovalId, need: Need }, Paused { by: Actor,
  reason: PauseReason }, Ended { reason: EndReason }, NoSession, RetryAfter { seconds: f32, limit: String },
  Changed { path: DocPath, by: Actor, version: DocVersion } }`.
- Constructors `OpError::needs_person(reason)`, `OpError::person_only(reason)`, `OpError::with_detail(detail)`.

**Runner, records and events.**
- `Request.base: Option<DocVersion>` (serde default).
- `OpRecord.on_behalf_of: Option<Actor>`, `OpRecord.approval: Option<ApprovalRef>` (both stamped by the executor).
- `Event::TxCommitted { tx, label, actor }`, `TxRolledBack { tx, actor, version }`, `Undone { tx, label, actor,
  owner, version }`, `Redone { tx, label, actor, owner, version }` (`actor` did it; `owner` owns the step).
- New events: `SessionOpened { session: SessionInfo }`, `SessionChanged { session: SessionInfo, change:
  SessionChange }` with `SessionChange { Grants { by }, Budget { by }, Paused, Resumed { by }, Spent }`,
  `SessionEnded { session: SessionInfo, ended: Ended }`, `SessionUndone { session: SessionId, by: Actor, result:
  SessionUndo }`, `ApprovalRequested { approval: PendingApproval }`, `ApprovalResolved { resolution: Resolution }`.
- `Runner::get(&self, path, grants: &Grants) -> Result<Option<Value>, OpError>`, `Runner::subscribe(&self,
  grants: &Grants) -> Result<Receiver<Event>, OpError>` and a new `Runner::log(&self, after_seq: u64, limit: usize,
  grants: &Grants) -> Result<Vec<OpRecord>, OpError>`, each `Denied` without `read`.

**Grants and ops.**
- `Grants::intersect(&self, other: &Grants) -> Grants` (3.3), `Grants::scopes()`, `Grants::fs_roots()` (for the
  popover), `Grants::without(scope)`.
- `OpCtx::confirmed_by() -> Option<&Actor>`, `OpCtx::require_person(reason) -> Result<(), OpError>` (passes when the
  actor is a person or a person confirmed this run; the answer is the same in a dry-run, like `fs_access`), and the
  executor-side `OpCtx::with_confirmation(&Actor)`.

Not in the contract: the executor's builder options (`with_principals`, `with_approver(bool)`, the first session id),
which are `omuzaki-ops` API.

## 12. Tasks

All belong to AG-02 in Phase 1. Each has a fence, a contract and acceptance checks.

| Task | Pri |
|---|---|
| ops-api: the sessions and approvals contract (section 11) | P0 |
| omuzaki-ops: session registry, principals and effective grants; pause and resume | P0 |
| omuzaki-ops: ending a session abandons its open transactions (4.2); revoke | P0 |
| omuzaki-ops: the approval queue (7.1, 7.6) | P0 |
| omuzaki-ops: the stale-write guard (`Request.base`, 5.1) | P0 |
| omuzaki-ops: session rate limits (8.2) | P0 |
| omuzaki-ops: session budgets and `charge` (8.1) | P0 |
| omuzaki-ops: undo a whole session (6) | P0 |
| omuzaki-ops: undo per principal (5.4) | P0 |
| omuzaki-mcp: sessions through the registry | P0 |
| omuzaki-mcp: `session_info` and `session_list` tools | P0 |
| omuzaki-mcp: `approval_wait` and approval outcomes in the heads-up | P0 |
| `omuzaki mcp` bridges to the studio (with pairing); headless runs from a credential's grants | P0 |
| omuzaki-cli: `omuzaki session list` (read-only) | P2 |
| omuzaki-ops: sanitise the client name when the registry builds the actor id | P1 |
| omuzaki-ui: Agent panel (re-pointed at the controls above) | P0 |
| omuzaki-ui: approval cards (decisions per need, 7.1) | P0 |
| omuzaki-mcp: project root plus delegated roots, through `SessionSpec` | P1 |

Three tests are required: pause and resume, revoke in the middle of a transaction, and
undo-session with a person's edit interleaved.

## 13. Alternatives rejected

- **A `Sessions` wrapper in front of the runner** (2): only surfaces that use it would be protected.
- **`Actor::Agent { on_behalf_of }`** (3.2): forgeable, and it changes actor identity.
- **Grants trusted from `Request`**: a transport bug or a hostile client message could widen them.
- **Locks or leases on paths for everyone** (5.1): persons would wait on agents across LLM round trips.
- **Holding a paused agent's calls indefinitely:** MCP clients time out. Calls are held up to 20 s by `omuzaki-mcp`,
  then refused with `Waiting`.
- **Approving through MCP elicitation or the CLI** (7.2): the agent controls both.
- **Approvals that resolve on a timer:** never.
- **Grants keyed on client names** (3.3): the client chooses its name.
- **A public `Runner::abandon(actor)`** (4.5): unnecessary with sessions and wrong for persons.
- **Floating-point money:** sums drift and differ between machines; micro-dollars are exact.

## 14. Decisions on the open questions (2026-10-12)

1. **Revoke also revokes the credential by default.** Revoke means the agent can't come back without the person
   granting again; pause is the soft option (section 6).
2. **New bridge credentials pair with one click** in Omuzaki's window, remembered per credential. Agents can't pair
   themselves (10.2, `Need::Pairing`).
3. **Rate-limit defaults are accepted.** A measurement run checks them against a realistic agent session, records the numbers
   and changes them if the data says so (8.2).
4. **Editing an agent's fenced part:** a dismissible, non-blocking inline note, never a modal. The person always
   wins, and the agent gets the `Conflict` that names them (5.3).

Also decided: headless `omuzaki mcp` runs from grants the person set in advance on a credential, so scripted and CI use
needs no approver (7.6).
