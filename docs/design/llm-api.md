---
title: "The LLM seam: client trait, streaming, usage and price, errors, fakes"
summary: "the LLM seam (ADR 0032): the `LlmClient` trait, blocking streams, the provider-neutral message model and MCP mapping, micro-dollar pricing and the pre-request bound, retries and cancellation, providers, privacy, `FakeLlm` and the conformance suite."
area: design
---

# The LLM seam: client trait, streaming, usage and price, errors, fakes

**Status:** proposed, awaiting acceptance. **Feature:** AG-01. **Decision record:**
`docs/decisions/0032-llm-seam.md`.
**Spec:** v3 §12 (AG-01), §14 (CN-01, CN-04, CN-05, CN-09), §23 (AP-03), §31 (SE-04), §39 rules 1, 2 and 6.
**Builds on:** ADR 0016 (an LLM is a connector capability, not a model slot), ADR 0020 and
`docs/design/agent-sessions.md` §8 (budgets in micro-dollars, `Sessions::charge`), ADR 0026 and
`docs/design/settings.md` (typed keys, `SecretRef`), `docs/design/evals.md` §6.3 and §12 (the reference driver), the
secret store (`SecretStore`, `SecretLabel`, `SecretValue`).

Five features call an LLM: the assistant (AG-01), the AI performer (AP-03), the connector capability (CN-09),
the eval reference driver and the rubric judge; the score drafter (ML-10) follows. This page gives them
one contract, one fake, one price rule and one place for keys and privacy. Types are shown as the contract they will
have; their doc comments are the implementer's.

## 1. What this covers

| Asked for | Where |
|---|---|
| Blocking or async; stream shape; who owns the runtime | D1 |
| Provider-neutral messages; MCP mapping | D2, D3 |
| Usage, money, who charges, the pre-request check, counting without the network | D4 |
| Errors, retries, refusal, context overflow, cancellation | D5 |
| Providers, model listing and probing, settings and keys, loopback | D6 |
| Privacy, consent and cost previews | D7 |
| Connectors (CN-01), `register_llm_provider!` | D8 |
| What the eval reference driver and the rubric judge share | D9 |
| FakeLlm, golden transcripts, the conformance suite | D10 |
| Dependencies, licences, the one HTTP stack | D11 |
| Worked example | §4 |

## 2. The crates

```
assistant (AG-01) ─┐                      ┌─ omuzaki-llm-api   trait, messages, events, usage, prices, errors,
AI performer       ├─ Box<dyn LlmClient> ─┤                  Cancel, Ledger, register_llm_provider!
connectors (CN-09) │                      ├─ omuzaki-llm-mock  FakeLlm, scripts, the conformance suite
eval driver, judge ┘                      └─ omuzaki-llm       Anthropic Messages + OpenAI-compatible chat,
                                                             HTTP, SSE, retries, price data, MCP mapping
```

`omuzaki-llm-api` depends on `omuzaki-core` (registration, `Value`), `omuzaki-secrets-api` (`SecretStore`),
`omuzaki-settings-api` (`SecretRef`), serde, serde_json (through `Value`, which schemars needs anyway), schemars and
inventory. It has no network stack and no async runtime.

## 3. Decisions

### D1. A blocking, pull-based stream; the host owns one tokio runtime

```rust
pub trait LlmClient: Send + Sync {
    fn info(&self) -> &ModelInfo;
    /// Validates and starts a turn. Does no I/O on the calling thread and returns in well under 1 ms.
    fn stream(&self, req: TurnRequest, cancel: &Cancel) -> Result<TurnStream, LlmError>;
}
pub struct TurnStream { /* Box<dyn EventSource>, the last Usage seen */ }
impl TurnStream {
    pub fn try_next(&mut self) -> Result<Next, LlmError>;                    // never blocks
    pub fn next_timeout(&mut self, wait: Duration) -> Result<Next, LlmError>; // blocks up to `wait`
    pub fn usage_so_far(&self) -> Option<Usage>;
    pub fn collect(&mut self, idle: Duration) -> Result<Turn, LlmError>;    // folds all events (refused once try_next took one); borrows, so usage survives
}
pub enum Next { Event(LlmEvent), Wait, End }
pub struct Turn { pub model: String, pub message: Message, pub usage: Usage, pub stop: StopReason, pub retries: u8 }
#[derive(Clone, Default)] pub struct Cancel(Arc<AtomicBool>); // cancel(), is_cancelled()
```

**Why blocking pull, not `async fn`/`Stream`:** every caller is a plain thread. Ops are synchronous; the assistant
panel polls once per UI frame (`try_next`, no blocking on the UI thread); the performer's conversation loop is its own
thread with a 200 ms barge-in deadline; the eval driver and judge are sequential loops. An async trait would put a
runtime and `futures` into the API crate and colour every caller. An async caller (an MCP handler) wraps
`next_timeout` in `spawn_blocking`.

**Who owns the runtime:** the host. `omuzaki-llm` takes a `RuntimeHandle` (an opaque handle in the API crate that
`omuzaki-llm` downcasts to its tokio handle) when a provider is built; the app passes its one runtime, and
`RuntimeHandle::dedicated()` starts a one-worker runtime for the CLI, tests and the eval driver. No crate starts hidden
threads. Each turn is one task that reads the HTTP body and pushes events into a bounded channel of 64: when the
consumer stops reading, back-pressure stops reading the socket, so nothing buffers without bound. Dropping a
`TurnStream` cancels its turn.

**Stream rules:** events arrive in the order of D3; exactly one `Stop` ends a turn, or an `Err` does, and either is
followed only by `End`. `TurnStream` checks `cancel` on every call, so after `cancel()` returns the next call yields
`Err(Cancelled)` and nothing else (D5).

### D2. The message model: provider-neutral, append-only

```rust
pub struct TurnRequest {
    pub system: Vec<SystemBlock>, pub messages: Vec<Message>, pub tools: Vec<ToolDef>,
    pub tool_choice: ToolChoice, pub max_output: u32, pub effort: Effort, pub cache: CachePlan,
}
pub struct SystemBlock { pub text: String, pub cache_after: bool }  // a cache breakpoint after this block
pub struct Message { pub role: Role, pub content: Vec<ContentBlock> } // Role: User | Assistant
pub enum ContentBlock {
    Text(String),
    Image { mime: ImageMime, bytes: Vec<u8>, origin: ImageOrigin },   // Png | Jpeg | Webp | Gif
    ToolUse { id: String, name: String, input: Value },
    ToolResult { id: String, blocks: Vec<ContentBlock>, is_error: bool }, // Text and Image only
    Opaque { provider: String, block: Value },                          // returned untouched
}
pub enum ImageOrigin { Render, File, Screen, Camera }
pub struct ToolDef { pub name: String, pub description: String, pub input_schema: Value }
pub enum ToolChoice { Auto, Any, Tool(String), None }
pub enum Effort { Default, Low, Medium, High, Max }
pub struct CachePlan { pub conversation: bool } // also cache the growing conversation (a breakpoint on the last message)
```

- **History is append-only.** Callers never edit earlier messages: current models bind thinking blocks to the
  conversation (evals.md §6.3), and editing breaks the cache prefix.
- **Opaque blocks** carry a provider's thinking and redacted-thinking blocks (and their signatures) byte for byte, in
  their original position. A client sends back only blocks whose `provider` is its own and drops the rest, so a
  conversation can move between providers without sending one provider's data to another.
- **System blocks and caching:** `cache_after` puts a breakpoint after that block, which covers the tools too (they
  precede the system prompt in Anthropic's cache order). Tools are sorted by name so the prefix is stable.
  `CachePlan::conversation` adds Anthropic's automatic caching of the growing conversation. Only the 5-minute cache is
  used, so `Usage` has one write counter (a 1-hour plan would add a field and a rate). OpenAI-compatible servers cache
  prefixes themselves (llama.cpp, vLLM) or not at all; breakpoints are ignored there and `ModelInfo::caching` says so.
- **Images** are at most 768 px on the long side (evals.md §6.3) and 5 MB; larger ones are `Unsupported` at
  `stream()`, never silently re-encoded. `origin` exists for D7's rule on camera frames.
- **`ToolChoice::Tool`** forces one tool, which is how a caller gets structured output on both APIs (the judge's
  `{answer, reason}`).
- **Effort** maps to Anthropic's effort setting and to OpenAI-compatible `reasoning_effort`; `Default` sends nothing,
  and a model whose `ModelInfo::effort` is false ignores it. There is no temperature: determinism comes from FakeLlm,
  not sampling.

### D3. MCP maps one to one; events are small and ordered

**`tools/list` → `ToolDef`:** `name` unchanged (our tool names are already API-safe, `^[a-zA-Z0-9_-]{1,64}$`, the
stricter OpenAI limit; a server tool outside it is skipped with a warning rather than renamed, since renaming breaks
the call path); `description` is `title`, a blank line and `description`; `input_schema` is `inputSchema` unchanged.
`outputSchema` and `annotations` are not sent.

**`tools/call` result → `ToolResult`:** `isError` → `is_error`; each `content` item in order: `text` → `Text`;
`image` → `Image` (base64 decoded, `origin: Render` for `omuzaki://render/...`, otherwise `File`); a `resource_link` →
`Text` of its URI; an embedded text resource → `Text`; an embedded image blob → `Image`; `audio` and other blobs →
`Text("[audio omitted]")` (SE-04: audio never leaves through a model). When `content` is empty and
`structuredContent` is present, it is sent as one compact JSON `Text`. The mapping is
`omuzaki_llm::mcp::{tool_defs, tool_result}` over the JSON shapes, with no `rmcp` dependency.

```rust
pub enum LlmEvent {
    Retrying { attempt: u8, wait: Duration, cause: String }, // only before Start
    Start { model: String },                                   // the served model id
    TextDelta(String),
    ToolUseStart { id: String, name: String }, ToolUseInput(String), ToolUseEnd { id: String },
    Opaque { provider: String, block: Value },                 // one complete block
    Usage(Usage),                                              // cumulative; the last one is final
    Stop(StopReason),
}
pub enum StopReason { EndTurn, ToolUse, MaxOutput, Refused, Paused }
// Every struct-like variant above (of LlmEvent, ContentBlock and ImageCost) is #[non_exhaustive]: callers build them
// with a constructor (LlmEvent::start(model), ContentBlock::tool_use(id, name, input), ImageCost::tiles(px), ...)
// and match with `..`, so a field added later breaks no provider.
```

Consecutive `TextDelta`s between other content events form one `Text` block. `ToolUseInput` carries raw JSON
fragments; `collect` parses them at `ToolUseEnd` (invalid JSON is `Malformed`). `Start` lets the eval driver check the
served model (evals.md §6.3); `Retrying` lets it record retries. `Paused` is Anthropic's `pause_turn`: the caller sends
the turn back to continue.

### D4. Usage and money: integer micro-dollars from a dated table

```rust
pub struct Usage { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 }
pub struct MicroUsd(pub u64);
pub struct PriceTable { pub as_of: String /* YYYY-MM-DD */, pub source: String, pub tiers: Vec<PriceTier> }
pub struct PriceTier { pub prompt_up_to: Option<u64>, pub rates: Rates } // first tier that holds the prompt
pub struct Rates { pub input: u64, pub output: u64, pub cache_read: u64, pub cache_write: u64 } // µ$ per million tokens
impl Usage {
    pub fn prompt(&self) -> u64;                              // input + cache_read + cache_write
    pub fn tokens(&self) -> u64;                              // prompt() + output
    pub fn cost(&self, prices: &PriceTable) -> MicroUsd;
}
```

- **The rule:** pick the first tier whose `prompt_up_to` holds `prompt()` (Haiku 5.5 is dearer above 100k prompt
  tokens), sum `tokens × rate` over the four counters in `u128`, divide by 1,000,000 and round **up**, once per
  turn. Exact and deterministic; it never under-charges, and over-charges by under 1 µ$ a turn.
- **`input` excludes cache counters.** Anthropic reports it that way; the OpenAI-compatible client subtracts
  `prompt_tokens_details.cached_tokens` from `prompt_tokens` and reports it as `cache_read`.
- **Prices** are data in `omuzaki-llm` (`prices.toml`: model id, `as_of`, source URL, tiers), changed by reviewed PR.
  A provider entry may override them (a reseller or proxy). Local providers use a zero table with `as_of = "local"`.
  A hosted model with no table cannot be built (`Unsupported`): an unpriced model would make every cap meaningless.
  Settings warns when a table is older than 180 days.
- **Who charges:** whoever calls the model, once per turn, after the final `Usage` or after an error that followed a
  `Usage` (`usage_so_far`: the provider billed it; `collect` borrows the stream, so it stays readable after an error).
  In the app that is the host of the assistant, performer or connector job:
  `Sessions::charge(id, Spend { usd_micros: cost, tokens: usage.tokens(), seconds: 0 })`. The client never
  charges: it knows no sessions. `Budget::tokens` counts all four counters, because a cache read still fills the
  context. The eval driver and judge run headless outside sessions and apply their own caps (evals.md §12.3) from the
  same `cost`.
- **The pre-request check** (`Ledger`, in the API crate) bounds a turn's cost before it is sent, with no network:
  - the prompt is the previous turn's measured `prompt()` plus its `output` (now history), plus an estimate of what
    was appended; on a first turn, the measured prompt of an earlier conversation with the same model, tools and
    system text (the ledger remembers it by hash), or the estimate of everything;
  - the estimate is 1 token per byte of text and JSON (byte-level BPE never makes a token shorter than a byte, so this
    bounds from above; Claude's tokenizer is not published, so this is our assumption, not a documented fact), plus 8
    per block, plus images at `ModelInfo::image_cost` (Anthropic: ⌈w/28⌉ × ⌈h/28⌉, retrieved 2026-10-13 from
    docs.claude.com/en/docs/build-with-claude/vision);
  - the bound prices the whole prompt at the maximum of the `input`, `cache_read` and `cache_write` rates, each taken
    as the maximum over every tier a prompt of up to the estimated size could fall in (`PriceTable::rates_up_to`; a
    table need not rise), and adds `max_output × output`. The arithmetic saturates, so an overflow is `MicroUsd(u64::MAX)`.

  The host sends only if the bound fits what is left (the session's money cap, and for the eval, the attempt, run,
  night and month). If not, the session pauses with a `Budget` approval (agent-sessions §8.1); nothing is sent.
  `count_tokens` endpoints are not used: they cost a round trip per turn, and the check must hold offline.

### D5. Errors, retries, cancellation

```rust
pub enum LlmError {
    RateLimited { retry_after: Option<Duration> }, Overloaded, Auth, ContextTooLong { max: u64 },
    Unsupported(String), Network(String), Malformed(String), Cancelled,
}
```

- **Retried by the client** only before the first `Start` (so no text is ever duplicated): 429 with `retry-after`,
  500, 502, 503, 504, 529 (`overloaded_error`), an SSE `error` event of type `overloaded_error` before `Start`, and
  connect or reset errors. At most **4** retries; the wait is `retry-after` when given (capped at 60 s), otherwise full
  jitter: uniform in `[0, min(16 s, 1 s × 2^attempt)]` from a seeded RNG (deterministic in tests). Each retry emits
  `Retrying`. `RetryPolicy { max_retries }` is in the provider config; 0 is allowed.
- **Surfaced at once:** 401/403 → `Auth`; 400 that names the context (Anthropic `prompt is too long`, OpenAI
  `context_length_exceeded`) and 413 → `ContextTooLong { max: info.context_window }`; other 400 → `Malformed`; a 429
  without `retry-after` → `RateLimited { retry_after: None }` (Anthropic sends that for a spend cap, which keeps failing
  until access resumes; retrieved 2026-10-13 from docs.claude.com/en/docs/api/errors); any failure after `Start` →
  `Network` or `Overloaded`, never retried. Exhausted retries surface the last cause.
- **Refusal is a stop reason, not an error** (`StopReason::Refused`; the OpenAI-compatible `refusal` field or
  `content_filter` finish maps to it). The turn may carry partial text and its usage costs money; an error would lose
  both. The issue's outline had `LlmError::Refused`; this design moves it, for that reason.
- **Context overflow:** `stream()` refuses a request whose `Ledger` lower bound (measured history only) already exceeds
  `context_window`; otherwise the provider's answer decides. The assistant then summarises or drops old tool results;
  that policy is AG-01's.
- **Cancellation, with numbers:** after `cancel()` returns, the next `TurnStream` call yields `Err(Cancelled)` and no
  event follows (checked on every call). The turn task checks the flag on every body chunk and on a 10 ms tick, so the
  connection is closed within **50 ms** of `cancel()`; cancelling during a backoff wait ends it at once. AP-03's
  200 ms barge-in keeps 150 ms for TTS and the planner.
- **Timeouts:** connect 10 s; first byte 120 s; 60 s idle between SSE events (pings count) → `Network`.
- **No fallback, anywhere:** a client serves exactly the model it was built for and never substitutes another
  (evals.md §6.3); falling back is a caller's decision, made visibly.
- No error, `Debug` or `Display` output ever contains a key or a request header.

### D6. Providers and configuration

| | Anthropic Messages | OpenAI-compatible chat |
|---|---|---|
| Who | Claude (the default, AG-01) | Ollama, llama.cpp `llama-server`, LM Studio, vLLM, any compatible host |
| Endpoint | `POST /v1/messages`, `stream: true` | `POST /v1/chat/completions`, `stream: true`, `stream_options.include_usage` |
| Key | `x-api-key` from `SecretRef` | optional `Authorization: Bearer` from `SecretRef` |
| Listing | `GET /v1/models` | `GET /v1/models` |
| Capabilities | from `prices.toml`'s model table (context, output limit, vision, tools, effort); an unlisted model is listed but not usable until priced | probed (below) |
| Opaque blocks | `thinking`, `redacted_thinking` | none (`reasoning_content` is dropped) |

**Probing** an OpenAI-compatible model runs once per model and is cached per endpoint and model id: the server's own
metadata first (Ollama `/api/show` capabilities and context length, vLLM `max_model_len` in `/v1/models`, llama.cpp
`/props`), then two free local probes: a 1-token request with one trivial tool (rejected → no tools) and one with a
1×1 PNG (rejected → no vision). Unknown context falls back to the provider entry's override, else 8,192.

**Settings** (registered with `register_setting!`, ADR 0026):

| Key | Scope | Agent rule | Value |
|---|---|---|---|
| `llm.providers` | Machine | Never | `Vec<ProviderConfig>`: `id`, `kind`, `endpoint`, `key: Option<SecretRef>`, `allow_remote`, `prices` override, `context_window` override, `retry` |
| `llm.assistant.model` | User | Confirm | `ModelChoice { provider, model, effort }` |
| `llm.performer.model` | User | Confirm | `ModelChoice` |

`Machine` because endpoints are local paths and ports; `Never` because the key reference and the endpoint decide
where data goes (ADR 0026 rule 2). The key itself lives only in the secret store (label
`connector/llm/<provider id>`); the client resolves it per request and holds the `SecretValue` only while the request
headers are built. **OpenAI-compatible endpoints must be loopback** (`127.0.0.0/8`, `::1`, `localhost` resolved to
those) unless the person sets `allow_remote`; plain `http` is refused for any non-loopback host.

### D7. Privacy, consent and cost previews (SE-04, CN-04)

| Provider | What leaves the machine |
|---|---|
| Anthropic, or OpenAI-compatible with `allow_remote` | system text, every message, tool definitions, tool results including render images, and the key |
| OpenAI-compatible on loopback | nothing |

- **Never camera frames, never captured audio.** There is no audio block. An `Image` with `origin: Camera` is refused
  by every client whose endpoint is not loopback (`Unsupported`), so AP-03's opt-in camera perception works only with a
  local model; screen images may go to a hosted model after the consent below names them.
- **One-time consent card** per hosted provider and endpoint host, before its first request: what is sent (the table
  row, plus "screen images" if the performer's perception is on), the provider's retention terms link, the price table
  and its date. Only a person accepts it (ADR 0020: agents never approve); the acceptance is app state, like ADR 0016's
  model acknowledgements, and a changed endpoint host asks again.
- **Cost preview (CN-04):** a connector job shows the `Ledger` bound before it runs ("sends 3 images to Anthropic,
  at most $0.004"). The chat panel and the performer show the session meter and cap instead of a card per turn.

### D8. Connectors (CN-01)

```rust
register_llm_provider!(LlmProvider {
    kind: "anthropic",
    manifest: ProviderManifest { capabilities, privacy, cost: CostModel::Metered | CostModel::Free, output_terms },
    build: fn(&ProviderConfig, &ProviderEnv) -> Result<Box<dyn LlmClient>, LlmError>,
    list_models: fn(&ProviderConfig, &ProviderEnv) -> Result<Vec<ModelInfo>, LlmError>,
});
```

`ProviderEnv` carries the `SecretStore` and the `RuntimeHandle`. Providers register themselves (§39 rule 2); CN-01's
framework wraps each registered provider as a connector with the LLM or vision capability, reading cost,
licence and privacy from the manifest, and FakeLlm registers as `kind: "fake"`, which is CN-01's mock connector for
the LLM capability. `list_models` is a blocking call for the Settings page only, never on a hot path.

A kind is registered once. Two registrations of one kind are a configuration error, never resolved by link order:
`provider(kind)` returns `RegistryError::DuplicateKind` naming both (package and `file:line`), and Apps call
`check_providers()` once at startup to fail early with the same error.

### D9. What the eval driver and judge share

**Shared** (they use `omuzaki-llm`): the Anthropic client (HTTP, TLS, SSE, retries, `Retrying` records), `Usage`,
`PriceTable` and `cost`, the `Ledger` bound, the MCP mapping, FakeLlm and the fake HTTP server.
**Theirs:** `reference.toml` pinning (model, effort, system prompt) and the served-model check on `Start`; no fallback
(the seam has none anyway); their loop (parallel tool calls, `person_reply`); their four caps and the month total from
`eval-runs.jsonl`; the key from `OMUZAKI_EVAL_API_KEY`, through a one-label `SecretStore` in `omuzaki-test`; transcripts
stored without headers; the judge's forced tool, random image order and agreement records. The eval reference driver drops its own
`reqwest` loop.

### D10. Tests: no network in CI

- **FakeLlm script** (JSON, serde types in `omuzaki-llm-mock`): a list of turns; each turn has an optional `expect`
  (the last user content contains a string, or answers tool ids), the events to send, a `chunk` size in bytes for
  `TextDelta` and `ToolUseInput` splitting, an optional `error` injected before event `n`, `retries` to emit, and a
  per-event delay on `FakeClock`. A request with no turn left, or one that fails `expect`, is `Malformed` naming the
  turn. Same script, same events, always. A script that does not load strictly is an error: unknown fields (a
  misspelled `expect`, say) are rejected at every level, including inside events and their usage.
- **Golden transcripts:** each provider test stores `{request, events}` as JSON lines (requests without headers) and
  diffs against a blessed file; SSE fixtures are hand-written from the providers' documented examples, never captured
  with a key.
- **Conformance suite** in `omuzaki-llm-mock::conformance` (as `omuzaki-render-mock` does): FakeLlm passes it directly,
  each real provider passes it against a loopback fake server (`hyper`, already in the tree). Cases: text only; one
  tool call; two parallel calls; opaque round trip; cumulative usage; refusal; `MaxOutput`; 429 with `retry-after` then
  success (1 retry); 429 without it (no retry); 529 five times (`Overloaded` after 4); 401 (`Auth`, no retry); context
  error; a drop after `Start` (`Network`, no retry); `Start → Usage → Network` leaves that usage in `usage_so_far`
  after `collect` fails; malformed SSE; cancel mid-stream (closed within 50 ms, measured at the server, worst of 100);
  cancel during backoff; the same stream split at every byte gives identical events (property test); a key never
  appears in any `Debug`, error or golden file.

### D11. Dependencies, licences and the one HTTP stack

**One TLS stack.** `omuzaki-llm` uses the workspace's `reqwest` with the features that `omuzaki-http` sets: `rustls`,
whose single crypto provider is `aws-lc-rs`, and `system-proxy`; it adds only the non-TLS features `stream` and
`http2` for async SSE. No second provider (`ring` was proposed and rejected on review: two crypto providers in one
binary are two TLS stacks to audit and patch). If `aws-lc-rs` ever breaks the mingw build, that is a
workspace-wide change on its own issue, never a per-crate choice.

**The same client as `omuzaki-http`, without linking it** (implementation crates don't link each other): the same
`[workspace.dependencies]` entry and TLS and proxy features (one `reqwest` build), the user agent `Omuzaki/<version>`,
a 10 s connect timeout, the OS proxy for hosted endpoints, and https only. Loopback endpoints differ on purpose: plain
http is allowed (D6) and the proxy is always bypassed (`no_proxy`), so a local model's traffic never leaves the
machine (D7).

**The one place that moves.** Every request goes through `omuzaki_llm::http`: one function that builds the client
(above) and one that sends a POST and returns the status, headers and body as a byte stream. Providers, retries and
SSE parsing sit above it. If the connectors design (decision 13, "one HTTP stack") puts an `HttpTransport` in
an API crate, that module becomes its implementation and nothing else changes.

| Crate | Use | Licence | Note |
|---|---|---|---|
| `reqwest` 0.13 (in tree) | HTTP client; the workspace's `rustls` and `system-proxy`, plus `stream`, `http2` | MIT OR Apache-2.0 | register row and TLS rows come with `omuzaki-http` |
| `rustls` 0.23, `aws-lc-rs`, `rustls-platform-verifier` 0.7 | TLS, crypto, OS trust store | Apache-2.0 OR ISC OR MIT; ISC AND (Apache-2.0 OR ISC); MIT OR Apache-2.0 | already registered by `omuzaki-http` |
| `sse-stream` 0.2 (in tree via rmcp) | SSE framing | MIT OR Apache-2.0 | needs its own register row |
| `base64` 0.23, `futures-util` 0.3 (in tree) | images, body stream | MIT OR Apache-2.0 | |

Licences read from crates.io on 2026-10-13. Every row goes in with the task that adds the dependency.

## 4. Worked example: one tool call, one cache hit, priced to the micro-dollar

FakeLlm script (`claude-haiku-5-5`; the prefix is the system prompt plus one tool, 4,800 tokens, `cache_after: true`):

```json
{"model": "claude-haiku-5-5", "turns": [
 {"expect": {"user_contains": "front"}, "chunk": 8, "events": [
   {"Start": {"model": "claude-haiku-5-5"}},
   {"ToolUseStart": {"id": "tu_1", "name": "render_preview"}},
   {"ToolUseInput": "{\"character\":\"main\",\"camera\":\"front\",\"size\":512}"},
   {"ToolUseEnd": {"id": "tu_1"}},
   {"Usage": {"input": 37, "output": 58, "cache_read": 0, "cache_write": 4800}},
   {"Stop": "ToolUse"}]},
 {"expect": {"answers": ["tu_1"]}, "chunk": 8, "events": [
   {"Start": {"model": "claude-haiku-5-5"}},
   {"TextDelta": "Here she is, from the front."},
   {"Usage": {"input": 413, "output": 23, "cache_read": 4800, "cache_write": 0}},
   {"Stop": "EndTurn"}]}]}
```

Turn 2's request appends the tool result: the text "Rendered main, front, 512 px." (29 bytes) and a 512 × 512 PNG
(⌈512/28⌉² = 361 tokens). Prices: Haiku 5.5 up to 100k prompt tokens, retrieved 2026-10-13 from
docs.claude.com/en/docs/about-claude/pricing: $0.10 input, $0.50 output, $0.125 5-minute write, $0.01 hit per million
tokens, so `Rates { input: 100_000, output: 500_000, cache_write: 125_000, cache_read: 10_000 }`.

| | Prompt | Sum of tokens × rate (µ$ per MTok) | Cost | `Spend` charged |
|---|---|---|---|---|
| Turn 1 | 4,837 | 37×100,000 + 58×500,000 + 4,800×125,000 = 632,700,000 | 632.7 → **633 µ$** | `{633, 4,895, 0}` |
| Bound before turn 2 | 4,837 + 58 + 29 + 361 + 3×8 = 5,309 | 5,309×125,000 + 1,024×500,000 = 1,175,625,000 | **1,176 µ$** (`max_output` 1,024) | must fit the cap |
| Turn 2 | 5,213 | 413×100,000 + 23×500,000 + 4,800×10,000 = 100,800,000 | 100.8 → **101 µ$** | `{101, 5,236, 0}` |

Conversation: **734 µ$** ($0.000734) and 10,131 tokens. The cache hit saved 4,800 × (100,000 − 10,000) / 10⁶ =
432 µ$ on turn 2. The conformance suite runs this script and asserts these numbers.

## 5. Where the sources disagree

- **AP-03 vs SE-04:** AP-03 lets the performer "see a chosen screen or camera"; SE-04 says camera frames never leave
  the device. Both hold under D7: camera images reach only a loopback model.
- **Refusal:** the issue's outline has `LlmError::Refused`; evals.md §6.3 calls refusal "its own outcome". D5 makes it
  a stop reason, because a refused turn still costs money.
- **Names changed from the outline:** `LlmEvent` gains `Start`, `Retrying` and `Opaque`; `ContentBlock::Opaque`
  names its provider; `TurnRequest` gains `tool_choice`; `LlmError` gains `Unsupported` and loses `Refused`;
  `StopReason`, `Next`, `Turn`, `Ledger`, `RuntimeHandle` are new.

## 6. The task cut

Seven tasks, filed after acceptance: the API and mock crates, the two providers, the price table, the settings
keys, the conformance suite, and the re-cut of the eval reference driver onto the shared client.

If this page must split, D6 and D7 (providers, privacy) go to their own page and the rest stays.
