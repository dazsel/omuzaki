---
title: "The assistant runtime: host loop, context, persistence, settings and the panel contract"
summary: "the in-app assistant's host: crates, the turn loop, Stop, context and compaction, conversation storage, settings, host-local tools and the chat panel contract (ADR 0040)."
area: design
---

# The assistant runtime: host loop, context, persistence, settings and the panel contract

**Status:** proposed (AG-01). **Decision record:**
`docs/decisions/0040-agent-runtime.md`.
**Spec:** v3 §11, §12 (AG-01, AG-06), §13 (TH-02, TH-08, TH-10), §14 (CN-05), §31 (SE-04).
**Builds on:** ADR 0020 and `agent-sessions.md` (sessions, approvals, budgets, the stale-write guard), the LLM seam
(`docs/design/llm-api.md`, ADR 0032: `LlmClient`, `TurnStream::try_next`, `Ledger`, `Usage::cost`), the AI performer
(`docs/design/ai-performer.md`: the same in-app session pattern), jobs (ADR 0036),
`branches.md` §8, ADR 0023 (agents read English, people read their language), `settings.md` (ADR 0026),
`evals.md` §6.3 and §13, `audio-api.md` §7.2.

ADR 0020 §10.1 makes the in-app assistant an in-process MCP client of the same server. Its host adds only what an
external client does for itself: it runs the LLM, charges the session, and waits on approvals inside a tool call.
This page says how that host works. Type sketches are the contract; their doc comments are the implementer's.

## 1. What this settles

| Asked for | Where |
|---|---|
| Crates, layering, how the UI, the eval driver and the CLI wire it | D1 |
| The turn loop: caps, parallel calls, transactions and undo, `Waiting`, `Paused`, `Limited`, `Denied`, `Conflict` | D2, D3 |
| Stop, pause, cancel, revoke | D4 |
| Context each turn, trimming, the cache layout | D5 |
| Untrusted content, secrets, camera and audio | D6 |
| Persistence, the journal's `Cause`, where "Explain this" rationale lives | D7 |
| Settings, the no-model and needs-key states, money, the budget meter's data | D8 |
| Host-local tools and MCP prompts | D9 |
| The panel contract | D10 |
| Tests | D11 |
| Worked transcript | §4 |

## 2. The shape

```
 omuzaki-ui chat panel ──Assistant (trait)──► AssistantHost (omuzaki-agent)
   │ send / stop / answer                     │  one host thread per conversation
   │ ◄── events (polled once per frame)       ├─ LlmClient (Arc<dyn>) ── TurnStream::try_next / next_timeout
   │                                          ├─ McpClientLink ── rmcp client ══ in-process duplex ══ omuzaki-mcp Session
   │                                          │                                                       (Transport::InApp,
   ├─ approval cards ── Sessions::resolve (by the person) role "assistant")
   └─ budget meter (TH-10) ─── SessionInfo { budget, spent }   ◄── Sessions::charge once per turn ──┘
```

## 3. Decisions

### D1. Crates: a small API crate, the host, and a mock

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-agent-api` | API | `Assistant` (the panel-facing trait), `UserMessage`, `ContextChip`, `AssistantEvent`, `HostError`, `Cap`, `PlanItem`, `QuestionId`, `ConversationId`, `Rationale`, `RationaleSource`, `PromptInfo`, `PromptArgInfo`, `PromptsState` (§7). Depends on `omuzaki-core`, `omuzaki-ops-api` (`Spend`, `Budget`, `ApprovalId`, `TxId`, `SessionId`) |
| `omuzaki-agent-mock` | Mock | `ScriptedAssistant`: plays an event script on `FakeClock`, records what the panel sent. For panel tests and the UI gallery |
| `omuzaki-agent` | Implementation | `AssistantHost`, `McpClientLink`, `HostConfig`, the loop, the context builder, the host-local tools, the conversation store. Depends on API crates and `rmcp`'s client only; it never links `omuzaki-mcp` or `omuzaki-ops` (lint-deps) |

```rust
pub trait Assistant: Send + Sync {                       // omuzaki-agent-api
    fn send(&self, msg: UserMessage) -> Result<(), HostError>;   // text plus context chips; refused while an answer runs
    fn stop(&self);                                              // D4
    fn answer(&self, q: QuestionId, text: String) -> Result<(), HostError>; // ask_person (D9)
    fn events(&self) -> EventReceiver;                           // the one consumer; a later call detaches the earlier
    fn transcript(&self) -> Vec<Entry>;                          // the conversation so far, for a panel opened late
    fn prompts(&self) -> PromptsState;                           // the `/` menu (D9, §7); the host's cached list
    fn refresh_prompts(&self);                                   // marks the list stale and refetches on the host thread (§7)
    fn prompt(&self, name: &str, args: BTreeMap<String, String>) -> Result<String, HostError>; // `prompts/get` text (§7)
}
impl AssistantHost {                                     // omuzaki-agent
    pub fn start(link: McpClientLink, llm: Arc<dyn LlmClient>, sessions: Arc<dyn Sessions>,
                 session: SessionId, cfg: HostConfig) -> Self;   // no I/O on the caller; spawns the host thread
}
impl Assistant for AssistantHost { /* … */ }
pub enum AssistantEvent {
    TextDelta(String), ToolCall { id: String, tool: String, summary: String }, ToolResult { id: String, ok: bool },
    Waiting { approval: ApprovalId }, Paused, Resumed, Question { id: QuestionId, text: String, choices: Vec<String> },
    Plan(Vec<PlanItem>), Limit(Cap), Budget { spent: Spend, budget: Budget }, Error(HostError), Done,
}
impl EventReceiver { pub fn try_recv(&self) -> Option<AssistantEvent>; } // never blocks; a host-owned queue (D4)
pub enum HostError { Busy, NoModel, NeedsKey, NeedsConsent, SessionEnded, ContextFull, Llm(LlmError), Mcp(String),
                     Store(String) }
```

- **Why an API crate:** the panel then depends on `omuzaki-agent-api` alone, so `omuzaki-ui` tests and the gallery run on
  `ScriptedAssistant` without an LLM, an MCP server or an executor. It also gives TH-08 a trait to read rationale
  through (D7) without the Theatre linking the host.
- **Deviations from the issue outline:** the methods are on a trait (the mock implements it too); `Resumed`,
  `Question`, `Plan` and `Limit` are new events; `answer` and `transcript` are new methods; `events` returns the host's
  own single-consumer queue rather than an mpsc `Receiver`, so Stop can withdraw output not yet read (D4). Other
  views (the Theatre, the Agent panel) read sessions and the store, not this queue. `llm` stays an `Arc`, made
  from the seam's `Box<dyn LlmClient>` with `Arc::from`, because the eval driver shares one client across attempts.
- **Wiring.** The app owns the in-process link (AGIN): it builds the duplex, connects the rmcp client, which opens a
  `Transport::InApp` session with `role = "assistant"` (as the performer uses `role = "performer"`) and the
  studio's person as principal, and hands the link, the session id and the model to `start`.
  - **UI:** `omuzaki-ui` (an App) links `omuzaki-agent` and wires the real host; the panel code sees `dyn Assistant` only.
  - **Eval driver** (`assistant`, evals.md §13): `omuzaki-test` wires the same host to its in-process server with the
    attempt's headless credential (no approver, ADR 0020 §7.6), answers `ask_person` with the task's `person_reply`,
    and takes the task's `[caps]` as `HostConfig::caps`.
  - **CLI:** `omuzaki ask "<prompt>" --project <p>` runs one message headless the same way and prints the events as text
    or `--json` lines. It needs a credential for anything past the agent defaults, as `omuzaki mcp` does.
- **Threads.** One host thread per conversation runs the loop; MCP calls go through the app's tokio runtime (the
  seam's `RuntimeHandle`) and the thread blocks on them with a cancel flag. No crate starts hidden threads.

### D2. The turn loop

A **turn** is one LLM request. An **answer** is everything the host does for one user message.

1. **Admit.** `send` is refused with `Busy` while an answer runs. If the session is `Paused { reason: Person }`, sending
   is the person's resume: the host calls `Sessions::resume` with `HostConfig::principal` first (D4).
2. **Build the request** (D5) and **bound its cost** with the seam's `Ledger`. If the bound does not fit what the
   session has left, the host calls `Sessions::request_budget(id, needed)` (new, §5): the executor pauses the session
   with a `Budget` approval and the host emits `Waiting`, then waits on it (step 6). Nothing is sent.
3. **Stream** with `try_next` on the host thread (`next_timeout(10 ms)`), forwarding text as `TextDelta`s coalesced to
   at most one per 16 ms. After the stop or an error, **charge once**: `Sessions::charge(id, Spend { usd_micros:
   usage.cost(prices), tokens: usage.tokens(), seconds: 0 })` from the final `Usage`, or `usage_so_far` after an error
   or a stop. Then emit `Budget`.
4. **Tool calls**, if the turn stopped with `ToolUse`. All the calls of one turn are run **in the order the model
   emitted them, one at a time**, and answered in one message. Order, because the executor serialises them anyway,
   because a later call may depend on an earlier one (a `Waiting` in call 1 must hold call 2), and so transcripts are
   deterministic. Read-only calls are cheap enough that concurrency would buy nothing.
5. **The transaction.** Each call is classified **before** anything is sent, by the op kind `omuzaki-mcp` puts in the
   tool's `_meta` (`"omuzaki/kind": "edit" | "query" | "job" | "driver_job" | "builtin"`, §5; `readOnlyHint` cannot tell
   a job from an edit):

   | Kind | Transaction |
   |---|---|
   | `edit`, `driver_job` | Before the answer's first one, the host opens one transaction: `tx_begin { label: "Assistant: <first line, 60 chars>", cause: "<first line, 200 chars>" }`. Later ones join it. A driver job joins and never commits it (ADR 0036 decision 10) |
   | `job` | Never in a transaction: ADR 0036 decision 1 makes a plain job with `Request.tx` `Invalid`, so `omuzaki-mcp` sends plain jobs without the session's transaction (§5). The job finishes as its own step |
   | `query`, `builtin`, host-local | Never open one. `history_undo` and `history_redo` while the answer's transaction is open are answered by the host without sending ("your changes in this answer are not a step yet; use `discard_changes`"); with none open they go through |

   The transaction commits at the end of the answer. So **one user message is one undo step**, and the person's Ctrl+Z
   (undo per principal, agent-sessions §5.4) takes back the whole answer. The exceptions are steps the executor makes
   anyway: an approved request runs as its own one-op step (ADR 0020 §4.4), and a plain job is its own step. The model
   is not offered `tx_begin`, `tx_commit` or `tx_rollback`; it gets `discard_changes` (D9) instead.
6. **What each refusal does.** Tool results go to the model exactly as `omuzaki-mcp` returns them (they already say
   what to do next); the host adds behaviour only where waiting is needed.

   | Result | Host |
   |---|---|
   | `Waiting`, detail `Queued { approval }` | Emit `Waiting { approval }`; call `approval_wait { approval }` until it resolves, each call held up to 20 s by `omuzaki-mcp`; give the model the resolution as the call's result: the applied outcome, or the rejection with its comment. No timeout: approvals never expire |
   | `Waiting`, detail `Paused` | Emit `Paused`; poll `Sessions::session` every 250 ms until `Active`; emit `Resumed`; re-send the same call (nothing ran) |
   | `Limited`, detail `RetryAfter` | `omuzaki-mcp` has already retried for 2 s. The host waits the stated time once (at most 10 s) and re-sends; a second `Limited` goes to the model |
   | `Denied` (scope, `PersonOnly`, fence) | To the model, which tells the person what it cannot do and where the person can do it |
   | `Conflict`, detail `Changed` | To the model with the heads-up `omuzaki-mcp` attached. The system prompt says: re-read what changed, keep the other actor's change unless the person said otherwise, then retry; the retry carries the newer `base` and is an informed overwrite |

7. **Caps per user message** (`HostConfig::caps`, from the `assistant.caps` setting): **30 turns, 60 tool calls,
   600 s of wall time, 400,000 tokens** (all four counters). These are the eval defaults (`evals.md` §3.2), so the
   `assistant` driver and the product stop at the same place. Wall time excludes time in `Waiting`, `Paused` and
   `ask_person`, since a person may take ten minutes to decide. At a cap the host emits `Limit(cap)` and makes one
   last turn with `ToolChoice::None` and the note "limit reached (60 tool calls): say what you did and what is left".
   That turn must pass the budget bound like any other.
8. **End.** The stop reason `EndTurn` ends the answer; `Paused` (the provider's pause) sends the turn back as is;
   `MaxOutput` continues once with "continue"; `Refused` ends the answer and shows the partial text. The host commits
   the transaction, writes the rationale (D7), and emits `Done`.

### D3. LLM errors in the loop

Retries before `Start` belong to the client (seam D5). The host retries nothing after it.
- `RateLimited`, `Overloaded` or `Network` after `Start`: charge `usage_so_far`, emit `Error`, commit the transaction,
  then `Done`. The person may send "continue".
- `Auth`: emit `Error(NeedsKey)`; the panel shows the needs-key state (D8).
- `ContextTooLong`: compact (D5), whose input is bounded, and retry the turn once; a second `ContextTooLong` is
  `Error(ContextFull)`.
- `Cancelled`: only after `stop` (D4).

### D4. Stop, pause, cancel and revoke

**Stop** is the panel's always-visible button (and `Esc` in the panel). It is one person control in the executor:
**`Sessions::stop(id, by) -> Result<Stopped, OpError>`** (new, §5; it ships with ADR 0036's contract). `by` must be the
principal, as for every control. It runs in this order, so that cancelled work is reverted before anything is
committed:
- **a. Under the executor's lock, atomically:** it pauses the session (`PauseReason::Person`, or keeps a pause already
  there) and cancels the session's jobs (`Jobs::cancel_session`). An op that holds the lock finishes first.
- **b. Cancelled jobs end** by ADR 0036's rules: at their next checkpoint, and at the latest at its 10 s deadline
  (decision 4). A **driver job joined to the answer's transaction** reverts only its own inner edits from that
  still-open transaction (decision 10). If that revert conflicts, it reverts nothing and leaves its edits in the
  transaction (`Unreverted`); Stop then keeps them, as "commit as is" is the caller's choice in decision 10. A plain
  job is in no transaction (D2.5), and its finish is discarded (`ops-api.md` §8.3, the Stop row).
- **c. Once no joined job is running, under the lock again:** it commits the open transaction as it now stands, with
  " (stopped)" added to its label, **whatever the session's state**. This is the person-authorised close that
  agent-sessions §4.3 lacks, since a paused session refuses its own `commit`. If the revert left the transaction
  empty, it is closed with no step.

`Sessions::stop` returns after a, with `Stopped { step: Pending | Committed(TxId) | None, jobs: Vec<JobId> }`. When
it returns `Pending`, the executor finishes c by itself and emits `TxCommitted` or `TxRolledBack`, and `Stopped` is
final once `SessionInfo::open_tx` is `None`.

`stop()` on the host returns in well under 1 ms and does three things under the event queue's lock:
1. sets the stopped flag, so nothing is enqueued after it except `Paused` and `Done`;
2. **withdraws** every `TextDelta`, `ToolCall`, `Question` and `Plan` still in the queue, unread. The answer's stored
   text is cut to what the consumer had read, so the panel, `transcript()` and the model's next history agree;
3. sets the turn's `Cancel` flag.

So **after `stop()` returns, `try_recv` yields no new output**: the panel shows nothing more from the click on. The host
thread then, with no waiting on the model or the call:
4. drops the stream; the seam closes the connection within 50 ms (seam D5);
5. sends MCP `notifications/cancelled` for the call in flight and stops waiting for it. The model's history answers
   that call with "stopped by the person before its result arrived". If the result arrives later, the host records
   it in the store and puts it in the next message's context block ("your call `face_set_profile` finished after the
   stop: applied"), so the model learns the truth before it acts again. An `approval_wait` or `ask_person` in flight
   is abandoned the same way; the approval stays queued for the person;
6. charges `usage_so_far`, calls `Sessions::stop(id, principal)` and emits `Paused`. When the step is `Pending`, it
   polls `Sessions::session` every 50 ms until `open_tx` is `None`, then emits `Done`.

**The numbers:**
- **No output after the click:** always, from the moment `stop()` returns.
- **`Done` within 100 ms** of the click when no driver job is joined to the answer's transaction and the executor's
  lock is free, or held by an op for at most 50 ms. If an op holds the lock longer, `Done` comes that much later,
  because the step must include that op.
- **With a joined driver job,** `Done` follows the job's end: its next checkpoint, and at most ADR 0036's 10 s
  deadline. The panel shows "Stopping: undoing the script's edits" until then. Nothing new runs in the meantime,
  because the session has been paused since step a.

D11 tests all three on `FakeClock`.

**Why the host calls `Sessions::stop` for the person.** Only the host knows when the stream and the call are down, and
the executor orders pause, cancel, revert and commit itself. `HostConfig::principal` is the studio's person.
Only in-process app code can call `stop()`, and the model has no path to it, just as it has none to the UI's buttons.

- **Pause** (Agent panel or Theatre, `Sessions::pause`): the host notices before the next turn, or through `Waiting`
  `Paused` on a call. It finishes the current stream (a pause is the soft control), sends nothing more, and keeps the
  transaction open (§4.3). **Resume** continues the answer.
- **Undo-session or a person's Ctrl+Z on the session's step** pauses it (`PauseReason::Undo`); the host treats that
  like a pause.
- **Revoke** ends the session: the executor rolls back the open transaction and withdraws pending approvals (§4.2).
  The host emits `Error(SessionEnded)` and `Done`, and refuses every later `send`. The conversation is kept, marked
  "revoked by <person> at <time>". Continuing it is an explicit "Continue in a new session", which opens a fresh session;
  the revoked credential (the assistant's "for this project" grants) is gone, as ADR 0020 intends.
- **New conversation** ends the current session (`Closed`). **One conversation is one session at a time**, so the
  budget, the Theatre's grouping and undo-session all match what the person sees as "this chat".

### D5. Context each turn

**The request** (`TurnRequest`, seam D2), in cache order:

| Part | Content | Changes | Cache |
|---|---|---|---|
| Tools | `tools/list` mapped one to one (seam D3), sorted by name, minus the `tx_*` built-ins, plus the host-local tools (D9) | on a server upgrade | prefix |
| System 1 | Identity and rules (`crates/omuzaki-agent/prompts/system.md`, English, versioned): who it works for, what it holds, "check your work with the measure and render tools the descriptions name", the untrusted-content rule (D6), the conflict rule (D2.6), "ask with `ask_person` only when a choice is the person's" | per release | prefix |
| System 2 | The server's MCP `instructions` | per release | prefix |
| System 3 | Host facts: "reply in <language name> (<BCP 47>); tool names, arguments and ids stay English" (ADR 0023, from `ui.language`), the model's id, the per-answer caps | when the language or caps change | `cache_after` here |
| Messages | The conversation, append-only (seam D2), with `CachePlan { conversation: true }` | each turn | automatic |

**Each user message** carries, before the person's text, one context block built at send time:
- `omuzaki://ui/context`: selection, workspace, focused panel, viewport camera, pending draw-to-direct notes. Its
  screenshot is a viewport render (`ImageOrigin::Render`), never a camera frame;
- the `project.describe` summary: in full on the conversation's first message, afterwards only when it changed;
- the heads-up: what others changed since the last answer. The host gets it from a `session_info` call, which carries
  the heads-up like every result (agent-sessions §5.2);
- the chips the person attached (D10).

Per-turn facts go in the user message, not the system prompt, because the prompt prefix must stay byte-stable for the
cache (seam D2).

**Size rules, applied when a block is appended, never later:** a tool result's text is cut at 32 KiB with "[cut: N
bytes; read <uri> for the rest]"; images follow the seam (≤ 768 px, ≤ 5 MB); audio is "[audio omitted]" (seam D3).

**When the window fills.** The history is append-only (thinking blocks bind to it, and edits break the cache), so the
host never trims in place. It **compacts** instead, when the next request's measured lower bound passes **75 % of
`context_window`** (W), or the provider answers `ContextTooLong`. The old history is never sent again:
1. **A bounded digest, built by the host from the store, no LLM:** the step labels, the plan, pending approvals and
   questions, the person's messages under the rule in step 3, and the model's text blocks newest first, until the
   digest reaches **40 % of W** (images and tool results left out).
2. **One summary turn:** the system prefix plus that digest, `ToolChoice::None`, `max_output` 2,000. Both sizes are
   measured with the `Ledger`'s estimate, which bounds from above. So the prompt is at most 80 % of W and fits with
   its output whenever the prefix is under 40 % of W and W ≥ 10,000, which the host checks once per model at start
   (a model that fails it is `Unsupported` for the assistant).
3. **The new conversation, sized as a whole.** It is the same system prefix, then one user message. Every size is the
   `Ledger`'s estimate (an upper bound), with 8 per block and images at `image_cost`. The request is built in this
   order, and each step checks the **whole** request: prefix, everything added so far, block overhead and
   `max_output`.
   - **Mandatory:** the summary (its measured output, at most 2,000), the latest person message, pending approvals
     and questions, and the plan (at most 20 items of 200 chars). Each goes in verbatim.
   - **Optional,** added only while the whole request stays within **60 % of W**: earlier person messages newest
     first, verbatim, then cut to their first 200 chars; then the latest render. Optional content that doesn't fit is
     left out. A line says what was left out ("N earlier messages and the last render, kept in this conversation's
     history"), so nothing goes silently.
   - The 60 % target leaves room to work before the 75 % trigger comes round again.
4. **When the mandatory part can't fit:** if the prefix, the mandatory content, the overhead and `max_output` together
   pass W, nothing is sent. The answer ends with `Error(ContextFull)`, and the panel offers "Start a new conversation
   from a summary".

   **Boundary case:** W = 10,000, prefix 3,900, summary 2,000, latest message 200, 3 blocks (24), `max_output`
   2,048. The mandatory request is 8,172, which fits W. It is already over 60 % (6,000), so the 2,000 tokens of
   earlier messages and the 361-token render stay out and are named in the line. Sending everything would have needed
   10,533 tokens, which is over W.
5. The stored conversation keeps everything. Compaction changes only what the model sees, and the transcript shows a
   "context compacted" divider.

The compaction turn is charged like any turn. Older images are the main cost, and compaction drops them all at once.

### D6. Untrusted content, secrets, camera and audio

- **Data, never instructions.** Document text, part and character names, labels written by other agents, chat
  messages, file contents, resource bodies and every tool result can carry text that looks like an order. The system
  prompt says so. The host also wraps everything it inserts that the person did not type in `<data source="…">…</data>`,
  escaping a literal `</data>`.
- **Why that is enough.** Injected text can at worst make the model edit the project within the session's grants. The
  structure does not depend on the model obeying: its ceiling is `read`, `edit:*`, `render`; it can never approve,
  grant or pair (agent-sessions §7.2); money is capped; and every edit is in one undoable step.
- **Secrets never enter the context (CN-05).** The host never resolves a `SecretRef`; only the LLM client does, per
  request (seam D6). Settings values show labels, never secrets (settings D1). The session holds no `fs` or `net`, and
  host errors carry no headers.
- **Camera frames and captured audio, never (SE-04).** The assistant has no perception tool. The seam refuses a
  `Camera` image to any non-loopback model and maps audio to text. Captured audio never reaches an agent
  (`audio-api.md` §7.2 rule 4), and no chip can attach either.

### D7. Persistence, the journal's cause, and "Explain this"

**Where conversations live: not in the document.** They are the person's words and the model's, and a project travels
(branches, merges, a second location, sharing). They live in the local data dir, which never roams (settings D3):
`<data>/assistant/<project id>/<conversation id>/` holding `log.jsonl` and `blobs/<blake3>.<ext>`. The project id is the
journal header's 128-bit id (journal.md).

- **Format:** JSON lines, appended and flushed per entry. The first line is `{"format": 1, "project", "created",
  "model"}`. Then one entry per fact: `user` (text, chips), `assistant` (the final content blocks, opaque ones byte
  for byte), `tool_call`, `tool_result` (images as blob hashes), `approval` (id, need, resolution), `question`,
  `answer`, `charge` (`Usage`, µ$), `step` (`TxId`, label, rationale), `session` (id, opened or ended and why),
  `compacted`, `stopped`. A torn last line is dropped on read, as in the journal.
- **Retention:** 90 days after the last entry (`assistant.retention_days`), purged at startup. **Delete** one
  conversation, or all of a project's, in the panel and in Settings > Privacy. Blobs go with their last reference.
  **Export** writes a Markdown transcript with its images beside it, plus the raw `log.jsonl`.
- **Resume after a restart:** live sessions never survive one (agent-sessions §3.4). The panel lists the project's
  conversations. Opening one replays `log.jsonl` into the view. The next message opens a new session and continues the
  same LLM conversation, opaque blocks included. If the provider changed, the seam drops the other provider's opaque
  blocks. An answer cut short by a crash shows "interrupted: the studio closed"; its open transaction was dropped with
  the session.

**What the journal records.** The step's label and `Cause { kind: Prompt, text }`: the first line of the person's
message, at most 200 chars. That line is what the Theatre card shows as "cause" (TH-02). `tx_begin` gains an optional
`cause` that `omuzaki-mcp` stamps on every request in that transaction (a `omuzaki-mcp` change; today it sends
`text: None`). The rest of the conversation stays out of the project.

**"Explain this" (TH-08).** The rationale for a step is the text the model wrote during that answer, before and
between its calls ("the jaw reads round because…; I'll narrow it and keep your width"). It is captured with no extra
tokens and cut to 2,000 chars. It is stored as the `step` entry of the conversation, keyed by `TxId`, and served by
`RationaleSource::rationale(tx) -> Option<Rationale>` (in `omuzaki-agent-api`), which the host's store implements and the
Theatre's step card asks. **Why not in the journal:** the rationale quotes the private conversation, and the journal
travels with the project. External agents' rationale (a `tx_commit` argument, say) is for a later design; this design
needs nothing from it.

### D8. Models, settings and money

| Key | Scope | Agent | Value (default) |
|---|---|---|---|
| `llm.providers` (seam) | Machine | **Never** | providers: kind, endpoint, key `SecretRef`, `allow_remote`, prices |
| `llm.assistant.model` (seam) | User | **Never** (the seam says `Confirm`; see below) | `Option<ModelChoice { provider, model, effort }>` (`None`) |
| `assistant.caps` | User | **Never** | `{ turns: 30, tool_calls: 60, wall_s: 600, tokens: 400_000 }` |
| `assistant.budget` | User | **Never** | the `Budget` each new session opens with: `{ usd_micros: 0, tokens: None, seconds: None }` |
| `assistant.retention_days` | User | **Never** | 90 |

- **Why `Never` on the model choice:** the provider decides where every message goes, and the model decides the price.
  Neither is taste (settings D4). A `Confirm` card that the assistant raises to change its own model is noise the
  person learns to click through. `llm-api.md` D6 says `Confirm`; this design chooses `Never`, and the change goes in
  with the provider settings keys.
- **Money defaults to 0** (ADR 0020). With a hosted model, the first answer's bound does not fit, so the session pauses
  with a `Budget` approval. The card offers "Raise budget" prefilled at **$0.50**, and the person sets it once per
  conversation, or raises the default in Settings. Local models cost 0 µ$ and never ask; their tokens still count.
- **No model configured** (`llm.assistant.model` is `None`): the panel says "No model set up. The editor works the same
  without one." with a button to Settings > Models. The app never builds the link, opens a session or touches the
  network, and the editor is unaffected (AG-01, SE-04).
- **Needs a key** (the provider has a key label and the secret store holds nothing for it), **unpriced model**, or **no
  consent yet** (seam D7): the panel shows that state with its one action. Nothing is sent until it is cleared.
- **Tiers per role**, and the reviewer model, belong to the team design (AGTM, AG-06). The assistant uses
  `llm.assistant.model`.
- **The budget meter (TH-10)** reads `SessionInfo { budget, spent }` from `Sessions` and the session events, the
  same for every session; the assistant adds nothing it can't see. The panel also shows the answer's own counters (turns,
  calls, wall time against `assistant.caps`) from the host's `Budget` and `Limit` events.

### D9. Host-local tools and MCP prompts

The host adds four tools beside the MCP tools. They run in the host, not the executor, and change nothing in the
document.

| Tool | What it does |
|---|---|
| `ask_person { question, choices? }` | A question card in the panel (`Question`); the call blocks until `answer`, or Stop. The result is the person's reply, as data. It grants nothing and is not an approval. Headless: the task's `person_reply` (evals.md §3.2) |
| `update_plan { items: [{ text, status }] }` | The answer's checklist (`Plan` event, at most 20 items). Kept through compaction. TH-03's plan board may later read it |
| `discard_changes {}` | Rolls back the answer's open transaction (`tx_rollback`). This is how "try it, then put it back" works inside one undo step |
| `read_resource { uri }` | MCP `resources/read`, for models that only call tools. Images come back as images |

**MCP prompts** ("Set up my first stream", spec §11): the panel's `/` menu lists `prompts/list`. Picking one shows a
form for its arguments, then `prompts/get` fills the **message box** with the prompt's text. The person reads it, can
edit it, and sends it as their own message. A prompt never runs by itself. How the panel reaches `prompts/list` and
`prompts/get`, and what the menu does in each state: §7.

### D10. The panel contract

**The panel sends:** `send(UserMessage { text, chips })`; `stop()`; `answer(question, text)`; and, for the `/` menu, `prompts()`, `refresh_prompts()` and `prompt(name, args)` (§7). Approval cards resolve
through `Sessions::resolve` as the person, never through the host. Pause and revoke are the Agent panel's
controls.

```rust
pub struct UserMessage { pub text: String, pub chips: Vec<ContextChip> }
pub enum ContextChip { Selection(Vec<DocPath>), Viewport, Note(NoteId), Image { png: Vec<u8> }, Resource(String) }
```

**The panel renders:** `TextDelta` into the current bubble. `ToolCall` becomes a compact row (`summary` is the tool's
title plus its main argument, "Face profile: egg-oval, 50 %"), ticked or crossed by `ToolResult`. `Waiting` shows the
approval card inline (the same component). `Paused` and `Resumed` show a banner. `Question` shows a card with
the choices and a text box. `Plan` is a checklist. `Limit` and `Budget` update the meter line. `Error` shows the error
with its one action. `Done` re-enables the box.

- **Polling:** the panel takes the receiver once and drains it with `try_recv` once per frame; it never blocks. The
  host coalesces text, so the queue holds at most about 60 text events a second, and Stop empties it of output (D4).
- **States without a host:** no model, needs key, unpriced, needs consent (D8); revoked (D4).
- **Accessibility:** the transcript is an AccessKit list. Streamed text is announced by sentence through a polite live
  region, never per delta. Stop has a label, `Esc`, and a 44 px target. Cards take focus when they appear. States are
  written in words, never shown by colour alone.

### D11. Tests

- **Every LLM call goes against `FakeLlm`** (seam D10), with `FakeClock` and no network. CI never holds a key.
- **Host unit tests** in `omuzaki-agent` use a scripted rmcp server over a duplex (tool results from JSON), not
  `omuzaki-mcp`. They cover: each row of D2.6; parallel calls kept in order; the four caps, each hit at exactly its
  number; Stop during a stream, where `try_recv` yields no output after `stop()` across 1,000 seeded interleavings
  (queued deltas included) and the stored text equals what was read; `Done` within 100 ms with a free lock and within
  100 ms plus the hold with an op holding it 200 ms; Stop during a call, with its result arriving late and reaching
  the next context block; Stop while `Waiting`; Stop after a budget pause, which still commits the step; Stop during
  a joined driver job (the job's edits are reverted before the commit, `Done` follows its end; then Ctrl+Z undoes the
  stopped step and redo restores exactly it; and with an `Unreverted` conflict, the step keeps the job's edits and
  undo and redo cover them too); classification
  (an edit opens the transaction, a plain job and `history_undo` never do); revoke mid-answer; compaction at 75 %
  and after `ContextTooLong` with a digest within 40 % of W; the whole rebuilt request sized, including the D5
  boundary case (8,172 sent, the optional 2,361 left out and named); and `ContextFull` when the mandatory part passes
  W; the context block and `<data>` escaping; and the cost
  sums, to the µ$.
- **Golden conversations** in `omuzaki-test`: the real `omuzaki-mcp`, executor and op crates, and `FakeLlm` scripts. The
  goldens are `{request, events, charges}` JSON lines diffed against blessed files. §4 is the first.
- **The store:** round trip, a torn last line, retention purge, delete, export.
- **Panel:** `ScriptedAssistant` scripts for every state in D10, with gallery screenshots.
- **Real-model acceptance (needs a maintainer decision on spend):** the `assistant` driver on the QG-03 starter set, 10 attempts per task,
  passes at **at least the reference model's baseline minus the suite tolerance** (evals.md §8.2, §13).

## 4. Worked transcript: "make her face more oval"

`FakeLlm` plays the model, with `claude-haiku-5-5` prices from the seam's §4 (µ$ per million tokens: input 100,000,
output 500,000, cache write 125,000, cache read 10,000). `max_output` is 2,048. The fixture is `humanoid-mini-a`. The
session opens with `Budget { usd_micros: 0 }`, and the person has the head selected. Each bound is the seam's `Ledger`
rule: (previous prompt + previous output + the estimate of what was appended) × 125,000, plus 2,048 × 500,000, rounded
up. Each charge is `Usage::cost`, rounded up once per turn.

| # | What happens | Events | Bound (µ$) | Usage {in, out, read, write} | Charged (µ$) |
|---|---|---|---|---|---|
| 0 | Person: "make her face more oval" (chip: head). Context block 1,100 bytes; prefix estimate 25,000 bytes; 6 blocks. Bound 26,148 tokens does not fit $0 left. `request_budget` pauses the session with approval #7 `Budget { needed: 4,293 µ$, left: 0 }` | `Waiting{7}` | 4,293 | | |
| | Person clicks "Raise budget", $0.50. The host sees #7 resolved | `Resumed` | | | |
| 1 | "Let me look at her face from the front." → `render_preview {main, front, 512}` → image | `TextDelta`×n, `ToolCall`, `ToolResult{ok}` | 4,293 | 4, 61, 0, 6,262 | **814** |
| 2 | `face_set_profile {main, egg-oval, 0.5}`. The host opens tx 31 "Assistant: make her face more oval". Meanwhile the person widened the head (version 43): `Conflict`, `Changed { /characters/main/parts/head/params/width, by: person, version: 43 }` | `ToolCall`, `ToolResult{!ok}` | 1,867 | 4, 48, 6,262, 447 | **143** |
| 3 | `read_resource {omuzaki://character/main}` | `ToolCall`, `ToolResult{ok}` | 1,910 | 4, 41, 6,709, 118 | **103** |
| 4 | "You widened the head while I worked; I'll keep your width." → `face_set_profile` again, newer base → applied (version 44) | `TextDelta`×n, `ToolCall`, `ToolResult{ok}` | 2,810 | 4, 96, 6,827, 1,891 | **354** |
| 5 | `render_preview {main, front, 512}` → image | `ToolCall`, `ToolResult{ok}` | 2,151 | 4, 40, 8,718, 141 | **126** |
| 6 | "Here she is: the jaw is narrower and…". The person clicks **Stop** after three deltas. The two unread deltas are withdrawn; the stream is dropped; `usage_so_far` is charged; `Sessions::stop` pauses the session (no jobs to cancel) and commits tx 31 as "… (stopped)" | `TextDelta`×3 read, `Paused`, `Done` | 2,189 | 4, 18, 8,859, 426 | **152** |

The worked arithmetic for turn 1: 4 × 100,000 + 61 × 500,000 + 6,262 × 125,000 = 813,650,000, so 813.65, rounded up to
**814 µ$**. Turn 2's bound: the prompt estimate is 6,266 + 61 + (29 bytes of text + 361 image tokens + 3 × 8) =
6,741 tokens; 6,741 × 125,000 + 2,048 × 500,000 = 1,866,625,000, so **1,867 µ$**. Every measured prompt is at or below
its bound's estimate.

**Totals:** **1,692 µ$** ($0.001692) and 46,988 tokens, in six `charge` calls; $0.498308 is left of the $0.50. One undo
step (tx 31, two applied ops) holds the person's Ctrl+Z, and the conflict cost one extra read, not an overwrite. In
the conversation store, `step` 31 holds the rationale from turns 1, 4 and 6. Without the Stop, turn 6 would have run
to `EndTurn` and committed tx 31 under its plain label.

## 5. Contract changes and where the sources disagree

- **`Sessions::request_budget(id, needed: Spend) -> Result<(), OpError>`** in `omuzaki-ops-api`. If the spend fits, it
  returns `Ok`. If not, it pauses the session (`PauseReason::Budget`) and queues a `Budget` approval with no request,
  returning `Waiting`, detail `Queued`. The seam's D4 ("the session pauses with a `Budget` approval; nothing is sent")
  needs a way to do that, and the trait has none: `charge` acts only after the money is spent. The performer needs it
  too. ADR 0036 decision 11 now carries it.
- **`Sessions::stop(id, by: &Actor) -> Result<Stopped, OpError>`** in `omuzaki-ops-api` (D4): the Stop control
  agent-sessions §6 lists but the trait lacks: pause and cancel jobs atomically, let cancelled driver jobs revert
  their edits (ADR 0036 decision 10), then commit what remains, in any state. The lead accepted it and folded it into
  ADR 0036's contract task, beside `request_budget`; the maintainers decide the hub change.
- **In `omuzaki-mcp`:** `tx_begin { label, cause? }` (D7); the op kind in each tool's `_meta` as `"omuzaki/kind"` (D2.5);
  plain jobs sent without the session's transaction (ADR 0036 decision 1).
- **`llm.assistant.model`'s agent rule:** `Never` here, `Confirm` in the seam (D8).
- **agent-sessions §10.3 says the assistant's session ends on project close or revoke;** this design adds New
  conversation (`Closed`). The note is in §10.1.

## 6. The task cut

These are filed after acceptance.

| # | Task |
|---|---|
| T1 | `omuzaki-agent-api` and `omuzaki-agent-mock` (D1, D10 types; `ScriptedAssistant`; with §7's `prompts` / `prompt` and types) |
| T2 | `omuzaki-agent` skeleton and the turn loop on `FakeLlm` (D2, D3, D4) |
| T3 | MCP-to-model content mapping in the host (`<data>` wrapping, size rules; D5, D6) |
| T4 | Context builder and compaction (D5) |
| T5 | Conversation store (D7) |
| T6 | Assistant settings keys (D8) |
| T7 | No-model, needs-key, unpriced and needs-consent states (D8) |
| T8 | The chat panel in `omuzaki-ui` (D10; visual; with §7's `/` menu) |
| T9 | The `assistant` driver in `omuzaki-test`, and §4 as its first golden (D11) |
| T10 | `omuzaki-mcp`: `tx_begin { cause }`, `"omuzaki/kind"` in tool `_meta`, plain jobs without the transaction (D2.5, D7) |
| T11 | `llm.assistant.model` to `Never`: folded into the provider settings keys task |
| T12 | `Sessions::request_budget` and `Sessions::stop`: folded into ADR 0036's contract task |
| T13 | Real-model acceptance run (needs a maintainer decision on spend) |

If this page must split, D7 (persistence) and D10 (the panel) move to their own page; the loop stays.

## 7. Addendum: how the panel reaches MCP prompts

D9 says the `/` menu lists the server's prompts; D1 says the panel sees `dyn Assistant` only; the trait had no way to
ask. **Decision: Option A, three methods on `Assistant`.** The host already owns the one MCP link (D1, Wiring), so
it answers; the panel gains no second object, no MCP client and no `rmcp` dependency (`omuzaki-ui` never links it).

- **Why not B** (a `PromptSource` trait the app hands the panel): a second object to wire in the UI, the eval driver
  and `omuzaki ask`, a second mock for every panel test, and a second place to say "no host" (D8). It buys nothing:
  prompts are served by the same session and catalog as tools, so only the host's link can answer. **Why not C**
  (the app talks to the server for the panel): it breaks "the panel sees one thing" and splits one link's state.
  A's cost is three more methods on the trait, an API diff on T1 (list it for the Director); T1 is unbuilt, so they are
  required methods, not defaults, and a forgotten implementation fails to compile.

```rust
// omuzaki-agent-api
pub struct PromptInfo { pub name: String, pub title: String, pub description: String, pub arguments: Vec<PromptArgInfo> }
pub struct PromptArgInfo { pub name: String, pub description: String, pub required: bool }
// on Assistant (D1):
pub enum PromptsState { Loading, Ready(Vec<PromptInfo>), Failed(HostError) }
fn prompts(&self) -> PromptsState;       // never blocks: the host's cached list (sorted by name), or why there is none
fn refresh_prompts(&self);               // never blocks: marks the cache stale; the host thread refetches; `Loading` until it answers
fn prompt(&self, name: &str, args: BTreeMap<String, String>) -> Result<String, HostError>; // one round trip, 2 s at most
```

- **`PromptInfo`** mirrors MCP's `Prompt`: `title` falls back to `name` when the server sends none; `description`
  falls back to `""`. Arguments are all text (`omuzaki-mcp` refuses anything else), in the server's order. The list
  holds what this session is offered (`omuzaki-mcp` hides a recipe whose ops the session's grants lack).
- **Host side.** `prompts()` reads a cache; the host thread fills it at `start`, refetches on
  `notifications/prompts/list_changed`, after each `Done` and on `refresh_prompts()`, and no call from the panel
  waits on I/O. A failed fetch is cached as `Failed`, so Retry is `refresh_prompts()` (it shows `Loading` at once), not a
  wait for a notification or an answer. `prompt()`
  sends `prompts/get` over the link (an in-process duplex: well under 2 s; past 2 s it returns `Mcp("timed out")`) and
  returns the first message's text. It is not a tool call, opens no transaction, charges nothing, sends nothing to the
  model, and is allowed in any state the link is up, including while an answer runs. Errors: `SessionEnded` (from `prompts()` as `Failed(SessionEnded)`) (revoked
  or closed, D4); `Mcp(text)` for the server's `invalid_params` (unknown prompt, missing argument, with its own
  hint) or a text over **16 KiB** (`Mcp("prompt text too long")`); the host builds no `NoModel` or `NeedsKey` here,
  the link needs no model.
- **Lifecycle: opening `/` is first use.** A new conversation has no host and no session until its first use,
  and first use is the first message **or** the first time the person opens the `/` menu. Opening it asks the app
  to start the host exactly as a first `send` would (a `Transport::InApp` session, `role = "assistant"`, the
  person as principal; Wiring, D1), then the menu shows `Loading` until the host's first fetch answers. Nothing is
  sent to the model, no tool is called and no transaction opens: `prompts/list` and `prompts/get` are read-only
  protocol calls. It still happens only on a person's action, so the session stays lazy. This refines ADR 0040
  decision 2 ("opened by the app at the first message"; the ADR text stays) and agent-sessions §10.1, whose sentence
  now reads "at first use: the first message, or opening the prompt menu". With no model set up (D8) there is no
  host and the menu shows that state instead; the host needs no model to list prompts, but the panel offers none
  without one.
- **The form** is built from `arguments` in order: one single-line text box per argument, labelled with the name
  (underscores shown as spaces), the description as its helper text, and "required" written in words on required
  ones. "Insert" stays disabled until every required box is non-empty. A prompt with no arguments skips the form and
  is fetched at once. Values go through `prompt` as typed, untrimmed except that empty optional boxes are left out.
- **Into the box.** The panel runs `prompt` on the app's worker (it must not block a frame, D10), shows "Getting
  prompt…" in the menu, and on `Ok(text)` puts the text in the message box: replacing it when empty, otherwise added
  after a blank line, as one undoable edit of the box. Focus moves to the box with the caret at the end, and a polite
  live region says "Prompt added to the message box". **The panel never calls `send` for a prompt.** The person reads
  it, edits it, then Enter or Send as for any message. Chips are unchanged by it.
- **Menu states.**

| State | `/` menu |
|---|---|
| An answer runs (`send` until `Done`), or between `stop()` and `Done` | Disabled, "Wait for the answer, or Stop": the box is disabled too, so there is nowhere to put the text |
| No model, needs key, unpriced, needs consent (D8), or revoked or ended (D4) | No host call is made (the panel has none, or `prompts()` gives `Failed(SessionEnded)`); the menu shows that state's one-line text and its action |
| After Stop | Enabled again at `Done`; Stop changes nothing in the list |
| `prompts()` is `Ready([])` | One row, "No prompts yet. Describe what you want in your own words." Not an error |
| `prompts()` is `Loading` | One row, "Loading prompts…" (the panel polls `prompts()` once per frame, D10) |
| `prompts()` is `Failed(Mcp(text))` | One row, "Prompts unavailable: <text>", with Retry, which calls `refresh_prompts()`. A `prompt` error stays under the form, and the panel calls `refresh_prompts()` |

- **Trust.** The text is the server's, but nothing reaches the model until the person has read it in the box and
  sent it. Then it is their `UserMessage.text`, so the host does **not** wrap it in `<data>` (D6 wraps only what the
  person did not type); `Cause::Prompt` carries its first line like any message (ADR 0040, decision 2). If the server
  changes a prompt between listing and getting, the form shows the listed description, but `prompt` returns the
  current text and the person sees exactly that in the box before sending; if it is gone or its arguments changed,
  the server's error shows and the menu is refreshed. Nothing is ever sent unseen.
- **Worked example: "Set up my first stream"** (the recipe is fixed; names below are illustrative).
  1. Opening the conversation's panel and the first `/` started the host, which listed `prompts/list` and cached `[set_up_first_stream "Set up my first stream", args:
     character (required, "Which character goes live"), platform (optional, "Where you stream")]`.
  2. The person types `/` in an empty box. The panel calls `prompts()` (no I/O) and shows the row (§7 Lifecycle: the first use started the host).
  3. They pick it. The form shows "Character (required)" and "Platform"; they type "Mira", leave Platform empty,
     and press Insert.
  4. The panel calls `prompt("set_up_first_stream", {"character": "Mira"})` on the worker. The host sends
     `prompts/get`; the server fills its template and returns one user message; the host returns its text.
  5. The box now holds that text. The caret is at its end; the live region announces it. Nothing was sent: the
     transcript is unchanged and `ScriptedAssistant` records no `send`.
  6. They change one line and press Enter. `send(UserMessage { text: <edited text>, chips: [] })` starts the answer
     as any message does (D2), and the menu disables until `Done`.
- **What changes where.**
  - **`Assistant`:** the three methods and three types above; nothing else in D1, D4 or D10.
  - **`AssistantHost` (T2):** the prompt cache, the refetch rules, `prompt()` with its 2 s and 16 KiB limits.
    Tested on the scripted rmcp server of D11: the cache gives a second `prompts()` with 0 MCP calls; with the first
    fetch failing, `refresh_prompts()` gives `Loading` and then `Ready` with no `list_changed` and no answer run; a
    `list_changed` is reflected by the next `prompts()` within one host tick; `prompt` passes the arguments
    unchanged; the 16 KiB boundary (16,384 bytes passes, 16,385 fails); a revoked session gives `SessionEnded`.
  - **`ScriptedAssistant` (T1):** `with_prompts(Vec<PromptInfo>)` and `with_prompt_text(name, text)` answer
    `prompts` and `prompt` from tables, with `fail_prompts(HostError)` (then `refresh_prompts` clears it, after one
    `Loading`) for the error rows; it counts `refresh_prompts` calls too; it records every
    `prompt` call (name, args) beside the `send` log, so a test asserts a pick sent nothing. No clock is involved.
  - **Chat panel (T8):** the `/` menu, the form, the insert step, the state rows above. Panel tests on
    `ScriptedAssistant`: in a fresh conversation, opening `/` and inserting a prompt sends no message and no tool call
    (`sent()` empty); Retry after a failure calls `refresh_prompts()` once and the menu recovers to the list;
    pick, form and insert leave the box equal to the text; Enter then sends
    exactly the edited text; one gallery screenshot per row of the state table, plus the form.
  - **`omuzaki-agent` stays free of `omuzaki-mcp`** (lint-deps); the CLI and eval driver need no change.
