# 0040: The in-app assistant is a host thread over the shared MCP server: one message is one undo step, Stop is instant, conversations stay out of the project (2026-10-13)

**Status:** proposed. **Context:** AG-01, TH-08, TH-10,
CN-05, SE-04; ADR 0020 §10.1 (the assistant is an in-process MCP client), ADR 0023 (agents read English), ADR 0026
(settings), the LLM seam (ADR 0032, `docs/design/llm-api.md`), jobs (ADR 0036). Design: `docs/design/agent-runtime.md`.

## Context

ADR 0020 says the in-app assistant reaches the document only through the same MCP server as any agent, and that its
host adds three things: running the LLM, charging the session, and waiting on approvals inside a tool call. It does not
say how that host loops, what the model sees, what Stop does, where conversations are kept, or what the panel can
rely on. The LLM seam gives the client, the stream, prices and the pre-request bound; this record decides the
rest.

## Decision

1. **Crates:** `omuzaki-agent-api` (the `Assistant` trait the panel uses, events, messages, `RationaleSource`),
   `omuzaki-agent-mock` (`ScriptedAssistant`) and `omuzaki-agent` (`AssistantHost`, the loop, the context builder, the
   conversation store). `omuzaki-agent` depends on API crates and rmcp's client only. The UI, the eval driver
   (`assistant`) and `omuzaki ask` all wire the same host over an in-process link.
2. **One conversation is one session** (`Transport::InApp`, role `assistant`), opened by the app at the first message.
   **One user message is one transaction and one undo step**, opened lazily before the first edit or driver job,
   labelled "Assistant: <first line>", with `Cause::Prompt` carrying that line. Calls are classified first by the op
   kind `omuzaki-mcp` puts in each tool's `_meta`: queries, built-ins and plain jobs never open or join it (a plain job
   in a transaction is `Invalid`, ADR 0036). The model gets `discard_changes`, not the `tx_*` tools. Approved requests
   and plain jobs stay their own steps (ADR 0020 §4.4, ADR 0036).
3. **The loop:** tool calls run in the order the model emitted them, one at a time. `Waiting` is waited out through
   `approval_wait`, with no timeout. `Paused` is waited out and the call re-sent. `Limited` is retried once, then given
   to the model. `Denied` and `Conflict` go to the model with the server's own hint. Caps per message: 30 turns, 60
   tool calls, 600 s of active wall time, 400,000 tokens (the eval defaults). At a cap there is one last summarising turn.
4. **Money:** each turn's cost is bounded before it is sent (the seam's `Ledger`) and charged once after it ends. A
   bound that does not fit pauses the session with a `Budget` approval through a new
   `Sessions::request_budget`. The session's money cap defaults to $0, so the first hosted answer asks for a cap;
   the card is prefilled at $0.50.
5. **Stop** is instant: `stop()` withdraws unread output from the host's own event queue, so nothing new is read
   after it returns. The host then drops the stream and the call without waiting for them (a late result reaches the
   model in the next message) and calls a new person-only control, `Sessions::stop`. That control pauses the session
   and cancels its jobs atomically; lets a cancelled driver job revert its edits from the open transaction (ADR 0036
   decision 10); then commits what remains, in any session state (so Ctrl+Z works, even after a budget pause).
   `Done` comes within 100 ms when no driver job is joined and no op holds the executor's lock longer than 50 ms;
   with a joined driver job, it comes when that job ends (≤ 10 s, ADR 0036 decision 4).
6. **Context:** a byte-stable prefix (sorted tools, rules, server instructions, then language and caps with a cache
   breakpoint), and per-message facts (`omuzaki://ui/context`, `project.describe`, the heads-up, chips) in the user
   message. History is append-only. At 75 % of the window, or on `ContextTooLong`, the host compacts. It summarises a
   bounded digest (≤ 40 % of the window), never the old history. It restarts with the whole request sized. The
   mandatory part goes in verbatim: the summary, the latest person message, pending approvals and questions, and the
   plan. Earlier messages and the last render are added only within 60 % of the window, and anything left out is
   named. If the mandatory part with `max_output` passes the window, the answer ends with `ContextFull`.
7. **Untrusted content is data:** everything the person did not type is wrapped and labelled. Grants, approvals and
   caps make the worst case an undoable edit. No secrets, camera frames or captured audio ever enter the context.
8. **Conversations live in the local data dir, never in the project:** JSON lines per conversation, kept 90 days,
   deletable and exportable, and resumable in a new session. The journal records only the step label and the cause
   line. **"Explain this" rationale** is the model's own text from that answer, stored in the conversation and served
   to the Theatre through `RationaleSource`.
9. **Settings** `llm.assistant.model`, `assistant.caps`, `assistant.budget` and `assistant.retention_days` are all
   `AgentWrite::Never`, like `llm.providers`. With no model set, nothing is built, no session opens, and nothing
   touches the network.
10. **Host-local tools:** `ask_person` (a question card, not an approval), `update_plan`, `discard_changes`,
    `read_resource`. MCP prompts fill the message box; the person sends them.

## Consequences

- New crates `omuzaki-agent-api`, `omuzaki-agent-mock`, `omuzaki-agent`. No new external dependency (rmcp is in the tree).
- `omuzaki-ops-api` gains `Sessions::request_budget` (ADR 0036 decision 11) and `Sessions::stop`, both in ADR 0036's
  contract task (the maintainers decide `stop`).
  `omuzaki-mcp`'s `tx_begin` gains an optional `cause`, each tool's `_meta` carries `"omuzaki/kind"`, and plain jobs are
  sent without the session's transaction.
- `docs/design/agent-sessions.md` §10.1 notes the session-per-conversation rule, and §8.1 notes the pre-request bound.
- The seam's `llm.assistant.model` (`llm-api.md` D6, ADR 0032) moves from `Confirm` to `Never`, in its own small task.
- **Rejected:** parallel tool execution (no gain under one executor lock, and it makes transcripts non-deterministic);
  one step per tool call (Ctrl+Z would take back a fragment of an answer); letting the model drive `tx_*` (it would
  leave transactions open across pauses and stops); trimming history in place (it breaks thinking blocks and the
  cache); conversations or rationale in the journal (private text would travel with the project); a Stop that leaves
  the step open (Ctrl+Z would find nothing to undo); `ask_person` as an approval (an answer grants nothing).
