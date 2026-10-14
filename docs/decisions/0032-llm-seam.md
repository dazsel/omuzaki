# 0032: One LLM seam: a blocking pull stream, a provider-neutral append-only history, integer micro-dollars, no fallback (2026-10-13)

**Status:** proposed. **Context:** AG-01,
CN-09, CN-05; spec v3 §12, §14 (CN-01, CN-04, CN-05, CN-09), §23 (AP-03), §31 (SE-04),
§39; ADR 0016 (an LLM is a connector capability, not a model slot), ADR 0020 (budgets, `Sessions::charge`), ADR 0026
(settings, `SecretRef`), ADR 0022 and `docs/design/evals.md` §6.3 (the reference driver). Design:
`docs/design/llm-api.md`.

## Context

Five features will call an LLM (the assistant, the AI performer, the vision and LLM connector capability, the eval
reference driver, the rubric judge) and the score drafter will too. None has a contract, and the eval driver was about
to write its own raw `reqwest` loop. Without one seam each would invent its own message model, price rule, retry
policy, key handling and test double.

## Decision

1. **Three crates:** `omuzaki-llm-api` (the `LlmClient` trait, messages, events, usage, prices, errors, `Cancel`,
   `Ledger`, `register_llm_provider!`; no network stack, no async runtime), `omuzaki-llm-mock` (`FakeLlm` and the
   conformance suite), `omuzaki-llm` (the Anthropic Messages and OpenAI-compatible chat clients).
2. **Blocking, pull-based streaming.** `stream()` does no I/O and returns a `TurnStream` with `try_next` and
   `next_timeout`. The host owns one tokio runtime and passes it in; no crate starts hidden threads. Dropping a stream
   cancels it.
3. **A provider-neutral, append-only history.** Text, images (at most 768 px, with an origin), tool use, tool results
   and opaque provider blocks returned untouched to their own provider. MCP `tools/list` and tool results map one to
   one.
4. **Money is integer micro-dollars** from a dated, tiered price table: one round-up per turn. The caller of the model
   charges `Sessions::charge` once per turn; the client knows no sessions. A pre-request bound computed offline must
   fit the remaining cap or nothing is sent. A hosted model without a price table cannot be used.
5. **Retries live in the client and only before the first event:** 429 with `retry-after`, 5xx, 529; at most 4,
   jittered. Refusal is a stop reason, not an error, because it still costs money. Cancellation closes the connection
   within 50 ms. A client never falls back to another model.
6. **Keys only through `SecretRef` and the secret store;** provider settings are `Machine` scope and agents can never
   change them; OpenAI-compatible endpoints are loopback unless the person allows otherwise. Camera images go only to a
   loopback model; captured audio never goes to any model. A hosted provider needs a one-time consent card a person
   accepts.
7. **Providers register themselves** with a manifest that CN-01 wraps as a connector; `FakeLlm` is CN-01's mock
   connector for the LLM capability.
8. **One HTTP and TLS stack:** the workspace's `reqwest` with `rustls` over `aws-lc-rs` and `system-proxy`, as
   `omuzaki-http` sets them, with the same user agent and proxy behaviour. All requests pass through one module,
   `omuzaki_llm::http`, which becomes the implementation of an `HttpTransport` if the connectors design puts one
   in an API crate.

## Consequences

- New API crate `omuzaki-llm-api` Tasks: the API and mock crates, the Anthropic provider, the
  OpenAI-compatible provider, the price table and `Usage::cost`, provider settings keys, the conformance suite, and
  re-cut onto the shared client.
- New dependencies: `sse-stream` (in the tree via rmcp, needs a register row); the TLS stack and `reqwest`'s shipped
  row come with it.
- Rejected:
  - **an async trait:** every caller is a plain thread, and it would put a runtime into the API crate;
  - **an LLM as a model-registry slot:** ADR 0016 keeps hosted and local-server models in connectors;
  - **`LlmError::Refused`:** an error would drop the partial text and the usage that must be charged;
  - **`count_tokens` before each turn:** a round trip per turn, and the budget check must hold offline;
  - **automatic model fallback:** it substitutes the model under the caller, which breaks pinning and cost previews;
  - **`ring` as a second crypto provider:** two TLS stacks in one binary to audit and patch; a mingw problem with
    `aws-lc-rs` would be a workspace-wide change on its own issue.
