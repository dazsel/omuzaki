---
title: "Connectors: the framework, rights, the tray, cost and privacy, keys and capabilities"
summary: "the framework, rights, the tray, cost and privacy, keys and capabilities"
area: design
---

# Connectors: the framework, rights, the tray, cost and privacy, keys and capabilities

**Status:** Accepted. **Decision record:**
`docs/decisions/0030-connectors.md`. **Features:** CN-01 to CN-12, spec v3 §14, §6, §7, §39 rules 2
and 6.

Artists bring their own generators, services and programs; Omuzaki calls them, shows what it will send and spend
before it does, keeps the results out of the project until a person accepts them, and records where every byte came
from. This page fixes that contract once, so the thirty-odd connectors build against one
design. It reuses what exists and adds no parallel system:

| Reused | For |
|---|---|
| `omuzaki-store-api`: `Store::put`, `Provenance`, `Source::Generated`, `Commercial`, `Consent`, `licence_gate` | where outputs go and what the publish gate reads (D6) |
| `omuzaki-ops-api`: `OpKind::Job`, `Plan.cost`, `Need`, `ctx.require_person`, `Services`, `Sessions::charge`, `Scope::{Net, Spend}` | the ops, grants and money (D4, D5) |
| `omuzaki-ai-api`: `Progress`, `Requester`, `ConsentLookup`, `Likeness`, `Version`, `ModelRegistry`, the TOML manifest plus `register_*!` pattern (ADR 0017) | registration, consent, the slot bridge (D1, D3, D6) |
| `docs/design/library.md` D9 | the accept and reject vocabulary (D7) |
| The LLM seam (`omuzaki-llm-api`) | the LLM and vision capability wraps it (D2) |
| `SecretStore`, settings, `omuzaki-http`, the consent hook, ADR 0016 | keys, configuration, HTTP, consent, user models |

## 1. What this covers

| Question | Section |
|---|---|
| Which crates; where the `Connectors` service lives; what the spec's `omuzaki-ai` row becomes | [D1](#d1-layering-and-crates) |
| The capability set, one typed request each, the adapter knobs; how LLM and vision use the seam | [D2](#d2-capabilities-and-requests) |
| The manifest file, its validation, registration, the MCP tool map | [D3](#d3-the-manifest-and-registration) |
| The `connect.*` ops and what they need from the job path | [D4](#d4-the-ops-and-the-job-path) |
| `spend`, budgets, what agents may never name, confirmation for assets that leave | [D5](#d5-grants-and-money) |
| The provenance the host builds and how commercial status only narrows | [D6](#d6-rights-and-provenance) |
| The candidates tray: records, states, who decides, consumers, garbage | [D7](#d7-the-candidates-tray) |
| Keys: where they live, who attaches them, scrubbing | [D8](#d8-keys) |
| Cancel, progress, partial results, LLM streaming | [D9](#d9-cancellation-progress-partial-results-streaming) |
| Replay (TH-12) and crash recovery | [D10](#d10-replay-and-recovery) |
| Fakes, cassettes, conformance; no paid call in any test | [D11](#d11-testing) |
| Settings, project state, secrets | [D12](#d12-configuration) |
| One HTTP stack | [D13](#d13-one-http-stack) |
| The phase cut, and what CN-07, 08, 10, 11 and 12 still need | [D14](#d14-the-phase-cut) |

## 2. The shape of it

```text
 caller op (connect.generate, concept.sheet.generate, ...)          person / agent
     │ plan: tray::input_rights (doc + store), then Connectors::preview (pure: no connector call)
     │ job:  Connectors::run      (blocking, on a job worker)
     ▼
 omuzaki-connect  Host ── ConnectorSet (every register_connector! in the binary, parsed once)
     │  preview: choose → check request and params → status → leaves-machine → inputs (private_test, consent)
     │           → estimate → rights join → needs_person
     │  run:     preview again → cap → seed → StoreInputs + AuthedHttp + RunCx → Connector::run
     │           → provenance per output → Store::put → Candidate (pending) ; Text/Json → answers
     ▼
 adapter (omuzaki-connect-comfyui, -mcp, -llm, -models, ...)   sees: RunCx only (no store, no key, no document)
     │ HTTP: cx.http (AuthedHttp → omuzaki-http's HttpTransport)   MCP: a SessionOpener service   LLM: the seam
     ▼
 caller op: tray::land → one patch, one undo step; the executor charges actual Spend (D5)
```

## 3. Decisions

### D1. Layering and crates

**Decision.** Connectors are their own subsystem, in the §39 shape:

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-connect-api` | API | ids, `Capability`, `ConnectorManifest`, `Request`, outputs, `Estimate`, `Preview`, `ConnectError`, `Connector`, `RunCx`, `register_connector!`, `Candidate` and the `tray` functions, `Call`, `RunResult`, the `Connectors` service trait |
| `omuzaki-connect-mock` | Mock | `FakeConnector` per capability, `FakePace`, `MemoryInputs`, `MemoryConfig`, the conformance suite |
| `omuzaki-connect` | Implementation | the host (`ConnectorSet`, preview, rights, `AuthedHttp`, run), the `connect.*` ops, the settings key |
| `omuzaki-http-api` | API | `HttpTransport`, `HttpRequest`, `HttpResponse`, `HttpStream`, `HttpError`, `Cancel` (D13) |
| `omuzaki-http-mock` | Mock | `FakeHttp` (routes, cassettes) (D13) |
| `omuzaki-http` | Implementation | `HttpFetcher` and `ConnectHttp: HttpTransport` |
| `omuzaki-connect-<family>` | Implementation | adapters, one crate per family: `comfyui`, `mcp`, `llm`, `models`, and one per named vendor that needs code; each self-registers |

- **The `Connectors` service trait lives in `omuzaki-connect-api`,** so any crate's op reaches the host with
  `ctx.service::<dyn Connectors>()` and lands its candidates with `tray::land` (D7) without depending on
  `omuzaki-connect`. `concept.sheet.generate` (CO-04) and emote art (SH-08) are such callers.
- **The App wires it.** `omuzaki-stage` is the shared wiring of every App (`crates/README.md`), so it owns building
  `Services` with the `Host` as `dyn Connectors`, plus `Store`, `SecretStore`, `Settings`, `ModelRegistry` and the
  MCP `SessionOpener`. No crate today builds an App's `Services` (only tests do); this is new work (Unlocks, N2).
- **The spec's `omuzaki-ai` row** ("model registry, provider connectors (MCP client + HTTP), the agent runtime for the
  built-in assistant, cost tracking") is split, as ADR 0018 did for audio: `omuzaki-ai` keeps the model registry;
  provider connectors move to the `omuzaki-connect*` crates; the assistant's model client is `omuzaki-llm*`
  crates; cost tracking is the session's (ADR 0020, `Sessions::charge`). The §9 table is amended in this PR.
- **The reverse bridge is kept:** a registry slot can be offered as a connector (`omuzaki-connect-models`, the local
  TTS slot as a voice connector), so callers see one voice capability whether the engine is local or hosted.

**Rejected.** *Connectors as registry runtimes* (a `Runtime` or `Slot` per vendor): a slot is a pinned-file tensor
session with a fixed signature, run in-process or in a sandboxed worker; a connector has no files to pin, costs
money, leaves the machine and returns files, not tensors. Forcing it into a slot would put prices, keys and privacy
notes into the model card and the registry's hot path. *Connectors inside `omuzaki-ai`*: one crate holding the
registry, every vendor and the assistant would be the merge hotspot §39 forbids and drag `reqwest` into everything
that needs a slot.

### D2. Capabilities and requests

**Decision.** A closed set, `Capability { Image, Texture, Model3d, Voice, Music, Motion, Llm, Vision, Dcc }`, with
one typed request each (`Request`, tagged by `capability`, `deny_unknown_fields`), as the connect-api draft proposes: `ImageRequest`,
`TextureRequest`, `Model3dRequest`, `VoiceRequest` (`VoiceChoice::{Preset, Clone}`), `MusicRequest`, `MotionRequest`,
`LlmRequest`, `VisionRequest`, `DccRequest`, with `capability()`, `input_assets()`, `count()`, `prompt_text()`,
`seed()`, `with_seed()` and `check()` and the limits listed there (`MAX_COUNT = 8`, `MAX_SIDE = 4096`, ...).

- **Closed, not open.** The capability decides the request type, the output kinds and how a caller uses the result;
  a new capability is an API change by design. Adapter-specific knobs go in each request's
  `params: BTreeMap<String, Value>`, and **the manifest declares them** (D3, `ParamDecl`): the host checks `params`
  against the declaration in `preview`, so an agent learns the knobs from `connect.describe` and a typo is `Invalid`
  naming `params/<name>` before anything runs.
- **Bytes become candidates; text and JSON go back to the caller.** `Output::Asset` is stored and enters the tray;
  `Output::Text` and `Output::Json` (LLM, vision, DCC answers) are returned in `RunResult::answers` and never reach
  the store or the tray: they are not assets, and a caller (sheet reading, critique) uses them at once.

**The LLM seam (`docs/design/llm-api.md`, ADR 0032).** `Request::Llm` and `Request::Vision` stay, thin: one turn, no caller tools, no streaming. They
exist so that a caller that wants "describe this sheet" goes through the same preview, cap, privacy and provenance
path as any other connector, and a person sees one Connectors page. Everything else stays in the seam: the client
trait, streaming, tool use and the assistant's loop (AG-01 and the AI performer call the seam directly).

| Question | Settled |
|---|---|
| The mapping | `system` to a system block; each `ChatMessage` to a text message; each vision image (read through `cx.inputs`) to an image block; `max_tokens` to `max_output`; no cache plan. Without `json_schema`: `tools: []`, `tool_choice: None`. With it: the seam's structured-output mechanism (llm-api D2, `ToolChoice::Tool`): one internal tool `answer` whose `input_schema` is the schema, `tools: [answer]`, `tool_choice: Tool("answer")`; the wrapper reads the forced `ToolUse` input as `Output::Json` (missing or not matching the schema: `BadResponse`) and never runs a tool. No other tool is ever sent |
| Who attaches the key | **The seam's provider.** It is also used without the host (AG-01), so it resolves its own key: the `SecretRef` in the `llm.providers` setting, label `connector/llm/<provider id>` (llm-api D6). The manifest says `deliver = "provider"` (D3); the host builds no `AuthedHttp` for `transport = "llm"` and hands the adapter a transport that refuses every request, so the wrapper cannot duplicate or bypass the seam's HTTP |
| Who checks the cap | The host refuses before the run if the estimate passes `max_usd_micros`; the wrapper refuses before the turn if the seam's pre-request bound (known input tokens plus `max_output` at the output price) passes `cx.cap_usd_micros` |
| One price source | **The seam's dated price table is the only source.** The manifest says `cost = { model = "provider" }` and carries no price; `Connector::estimate` of the wrapper calls the seam's bound. No copy, so no test pinning two copies (pin test is dropped) |
| What `Usage` says | `input_tokens = input + cache_read + cache_write`, `output_tokens = output`, `usd_micros = Usage::cost(prices)` (the seam's exact integer rule). Money goes through `JobCtx::spend` as the turn completes and tokens through one `Sessions::charge` when `work` returns (D4.6), both by the `connect.generate` job, never by the wrapper; AG-01, which calls the seam directly, charges its own |

### D3. The manifest and registration

**Decision.** Each connector ships `connectors/<id>.connector.toml` next to its code, embedded with `include_str!` and
registered with `register_connector!("<id>", include_str!("../connectors/<id>.connector.toml"), Type)`, the
`register_model!` pattern (ADR 0017 decision 1): the repo's licence gate reads every manifest without building
(`cargo xtask connectors`), and the binary carries exactly what it links (ADR 0015, kind `connector`).

The fields are the draft's (`id`, `name`, `version`, `transport`, `[[capabilities]]`, `auth`, `cost`, `[privacy]`,
`[licence]`, `[mcp]`, `endpoint`), with three changes:

```rust
pub enum Transport { Http, Mcp, Llm, Local, Fake }            // + Llm: reaches its service through the seam
pub enum Deliver { Header { name: String, scheme: String }, Env { name: String }, Provider }   // + Provider (D2)
pub enum CostModel { Free, PerCall { .. }, PerUnit { .. }, PerToken { .. }, Provider }          // + Provider (D2)

/// An adapter knob a request may set in `params` (D2). Declared per capability, shown by `connect.describe`.
pub struct ParamDecl { pub name: String, pub kind: ParamKind, pub default: Option<Value>, pub summary: String }
pub enum ParamKind { Int { min: i64, max: i64 }, Float { min: f64, max: f64 }, Bool, Text { max_bytes: u32 },
                     Choice { options: Vec<String> } }
pub struct CapabilityDecl { /* capability, inputs, outputs, max_count */ pub params: Vec<ParamDecl> }
```

Validation (`ConnectorManifest::validate`, every problem with its field path) is the draft's list, amended:

- `transport = "llm"` needs `cost` `provider` or `free`, `auth` `none` or `api_key` with `deliver = "provider"`,
  capabilities only `llm` and `vision`, and no `endpoint` (the seam's settings hold it); `deliver = "provider"` and
  `cost = "provider"` are refused on any other transport.
- `endpoint` is `https://`, or `http://` with a loopback host (a shipped default is always one of the two; a person
  may point it at a LAN host in settings, D12).
- `params`: unique names, a slug each, a default inside its kind's range; `min <= max`.
- `auth.secret` is exactly `connector/<id>` with `deliver = "header"` or `"env"`; with `deliver = "provider"` it is
  absent, because the key belongs to whichever `llm.providers` entry the person selects (its `SecretRef`). Either
  way the host never reads `auth.secret` directly: it uses the effective label `Target::secret` (D4).
- **The auth enum:** `AuthSpec::ApiKey { secret: Option<String>, deliver: Deliver }` carries the field as
  `Option`: `secret` is `Some("connector/<id>")` with `deliver = "header"` or `"env"`, and `None` with
  `deliver = "provider"`; a `secret` present with `provider` is a validation error. The shipped LLM manifests
  (`local-llm`, `anthropic`) use `api_key` with `deliver = "provider"`, so a selected provider that has a key reaches
  status and scrubbing through `Target::secret`, and a keyless provider entry gives `secret: None`, which is never
  `NeedsKey`. `auth = none` stays valid for an LLM manifest; then `Target::secret` is `None` even if the selected
  provider has a key, because the seam's client still resolves its own key. The shipped manifests do not use it.
- Unchanged: `leaves_machine` agrees with `destinations`; paid models carry a `priced` date;
  `outputs = user_model | reported` needs `commercial = unknown`; `transport = mcp` if and only if `[mcp]` exists,
  with a tool for each capability; output kinds match the capability.

**The MCP tool map** (`[mcp]`'s `McpTool { capability, tool, args, url_field }` with `ArgSource::{Field,
Const}`) makes one struct, `McpConnector`, serve every MCP vendor: a new MCP connector is a manifest and a
registration line. Asset fields (`references`, `images`, `reference`, `asset`) go as base64 read through `cx.inputs`,
never as a hash or a path.

**DCC requests (`DccRequest { tool, action, asset, args }`; D2).** `tool` is the DCC program and equals the
connector id (for example `blender`). `action` is the name of the mapped MCP tool to call. `[mcp].tools` may hold
several `dcc` entries, one per tool, each `(capability, tool)` unique. An `action` that no entry names, or a `tool`
that differs from the connector id, is refused before any call. `asset` is an optional input, read through
`cx.inputs` like the fields above. `args` is the request's params map (`Request::params()` is `Some` for `dcc`),
checked against the capability's `ParamDecl`s by the host in `preview` and again by the connector, so an undeclared
or ill-typed value never reaches the program (for the request type).

**Rejected.** Manifests loaded from the person's disk at run time (a downloaded manifest could claim `commercial =
ok` for anything; shipped manifests pass the register gate, and user models are declared in settings, D12). A
manifest per capability (a vendor's terms, key and privacy are per service).

### D4. The ops and the job path

| Op | Kind, scope, undo | Does |
|---|---|---|
| `connect.list {}` | query, `read`, none | every connector: id, name, capabilities, paid, leaves_machine, status |
| `connect.describe {connector}` | query, `read`, none | the manifest (with `params`); never a local path, command line or key |
| `connect.key_status {connector}` | query, `read`, none | `{present, needs_key}` from `SecretStore::contains`; never the value |
| `connect.preview {request, connector?}` | query, `read`, none | the `Preview` (D5) plus its `english()` sentence; `Plan.cost` from the estimate |
| `connect.generate {request, connector?, expect_usd_micros?, max_usd_micros?, reuse?}` | **job**, `net`, `Auto` | plan = the preview; job = `Connectors::run`, then `tray::land` as one transaction |
| `connect.test {connector}` | job, `net`, none | `Connectors::ping`: free, no outputs |
| `connect.tray_list {state?, capability?}` | query, `read`, none | tray rows sorted by id |
| `connect.accept {candidate}`, `connect.reject {candidate, reason?}` | edit, `edit:library`, `Auto` | `tray::decide` (D7) |
| `connect.regenerate {candidate, seed: same \| new, changes?}` | job, `net`, `Auto` | re-issues the stored request (D7) |

**A dry-run of `generate` is the preview:** an empty patch, `output` the `Preview` with its sentence, `cost` the estimate; it calls no
connector, spends nothing and changes nothing. `regenerate` is the same. Op arguments have no field for a URL, a
command line or a key (D5).

**The job path is the job system's (ADR 0036, in review): `plan` under the lock, `work` on a job worker off the lock, `finish`
under the lock against the document as it is then.** Connectors use it like this, and block on it:

1. **`plan`** (also the dry-run): `tray::input_rights(ctx.doc, store, req)`, then `Connectors::preview`; an empty
   patch, the preview as output, the estimate as `Plan.cost`. Nothing is sent.
2. **`work`** runs what the plan priced, never a re-choice: it takes the connector and the cap from the plan
   through `JobCtx` (below), and **re-resolves the rights from the live document** before anything leaves: `job.read(|doc|
   tray::input_rights(doc, store, req))`, then `Connectors::run` with that map in `Call::inputs`. The rights that
   gate sending are the ones read at work start, not the plan's: a record added between plan and work (a queued job,
   an intervening run that marks the same bytes `private_test`) is seen, and the call fails with `PrivateInput` or
   `PreviewChanged` before any request. `work` returns the `RunResult` as JSON, including the map it used.
3. **`finish`** re-reads the rights against the document as it is then and joins them with the map `work` used,
   strictest wins, into each candidate's `provenance` before `tray::land`: a stricter record that arrived while the
   call ran is never lost from the outputs, though their bytes were already sent. One patch, one undo step, labelled
   "Generate 4 images with comfyui"; a cancelled or partly failed job lands its complete outputs.
4. **Progress and cancel** come from `JobCtx`: `progress` and `is_cancelled` for the borrowed `Progress`, and an
   owned `CancelToken` (below) for anything that outlives a call, such as an `HttpStream`.
5. **The run id is the `JobId`**, recorded in the journal (`JobStarted`); seeds (D10) and candidate ids
   (`r<job>-<index>`) derive from it.
6. **Money** is the plan's `CostHint.usd`, reserved at start as the job's hard cap. The cap is
   `min(max_usd_micros, reserved)` and goes to the adapter as `cx.cap_usd_micros`. **Every billable request is
   reported the moment it completes,** before anything is stored, through the `Meter` the host passes down
   (`RunCx::meter`), which the job wires to `JobCtx::spend`; a store failure or a cancel afterwards cannot lose a
   charge. `Usage.usd_micros` must equal the sum metered (conformance). **Tokens** (LLM and vision) are charged by
   the job with `Sessions::charge` (tokens only, money 0), once, when `work` returns.
7. **Recovery** never re-runs a job that was running at a crash (D10).

**The handoff, in contracts.** ADR 0036 gives `work` only the op's arguments and `JobCtx`. Connectors use three
`JobCtx` accessors added by the jobs work, additive to ADR 0036:

```rust
// omuzaki-ops-api: JobCtx accessors added by the jobs work
impl JobCtx<'_> {
    /// The output of the plan this job runs (for connect.generate, the Preview: the chosen connector and estimate).
    pub fn plan_output(&self) -> Option<&Value>;
    /// The money reserved for this job at start, micro-dollars: the hard cap `spend` enforces.
    pub fn reserved_usd_micros(&self) -> u64;
    /// An owned, cheap-to-clone cancellation flag for work that outlives a borrow (a streaming body).
    pub fn cancel_token(&self) -> CancelToken;
}
/// Set once when the job is cancelled; `Clone + Send + Sync + 'static`.
pub struct CancelToken(/* Arc<AtomicBool> */);
impl CancelToken { pub fn is_cancelled(&self) -> bool; }
```

```rust
// omuzaki-connect-api
/// Where a billable request is reported as it completes. `Err(CapReached)` once the next charge would pass the cap.
pub trait Meter: Send + Sync { fn charge(&self, usd_micros: u64) -> Result<(), ConnectError>; }
/// What a run gets from its job, besides the call.
pub struct RunEnv<'a> { pub progress: &'a dyn Progress, pub cancel: Arc<dyn Cancel>, pub meter: &'a dyn Meter }
pub trait Connectors: Send + Sync { fn run(&self, call: &Call, env: &RunEnv<'_>) -> Result<RunResult, RunError>; /* rest as D6 */ }
// RunCx gains `cancel: Arc<dyn Cancel>` (for HttpTransport::open) and `meter: &dyn Meter`.
```

`work` reads the planned `Preview` from `plan_output`, sets `Call::connector` to its connector (never choosing
again) and puts the whole planned preview in `Call::planned: Option<Preview>`. `Connectors::run` then:

- **resolves the connector's target once** and uses that one value for the whole run: the preview, the price, the
  allowed hosts of `AuthedHttp`, the `RunCx` and the client that sends. A settings change during the run affects the
  next run only. The target covers every place a connector's configuration lives, the LLM providers' included:

  ```rust
  // omuzaki-connect-api
  /// Where and how a call goes, resolved once from settings. Built by the connector, read by the host.
  pub struct Target {
      pub config: ConnectorConfig,        // the `connectors.entries` snapshot
      pub endpoint: Option<String>,       // the effective endpoint
      pub leaves_machine: bool,           // decided on the effective endpoint (D12) or the provider's remoteness
      pub destinations: Vec<String>,
      /// The one key label this run uses: `auth.secret` by default, the selected provider's `SecretRef` for an LLM
      /// connector, `None` without auth. Status, `key_status`, `AuthedHttp` and scrubbing all read this field.
      pub secret: Option<SecretLabel>,
      pub detail: Value,                  // adapter-specific resolved settings, opaque to the host
  }
  pub trait Connector: Send + Sync {
      /// Default: from the manifest and `config` (HTTP, MCP, local). Reads settings through `services` only here.
      fn target(&self, m: &ConnectorManifest, config: &ConnectorConfig, services: &Services) -> Result<Target, ConnectError>;
      fn estimate(&self, m: &ConnectorManifest, t: &Target, req: &Request) -> Result<Estimate, ConnectError>;
      fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError>; // cx.target
      // ping, as drafted
  }
  ```

  **The LLM wrapper overrides `target`:** it reads the `llm.providers` entry its config names
  (`ConnectorConfig::provider`, a new optional field) through the `Settings` service, copies that `ProviderConfig`
  into `detail`, and derives `endpoint`, `destinations` and `leaves_machine` from it (loopback unless
  `allow_remote`, llm-api D6). `estimate` prices from `detail`'s price table (with its override), and `run` builds the
  seam client from `detail` (`build(&ProviderConfig, &ProviderEnv)`), never from live settings. So the one
  `ProviderConfig` the preview judged is the one that sends. An unset `provider`, or one that names no `llm.providers`
  entry, fails `target` with `ConnectError::Unavailable` naming the setting; the status is `Unconfigured`, and
  `connect.key_status` reports that state rather than a key;
- **builds the preview again** from that snapshot, the live `InputRights` and the request, and compares it with
  `planned` by `Preview::widens(&self, planned: &Preview) -> Option<Widening>`. Anything that makes the call send
  more, to more places, or cost more fails `RunError::PreviewChanged { what }` before any request: `leaves_machine`
  turning true, a destination or effective endpoint not in the planned list, an `outbound` count or byte total
  above the planned one, `trains_on_inputs` or `retention` changing, the key label (`Target::secret`) changing, `needs_person` turning `Some` (the confirmation
  given at plan time no longer covers it), the plan tier changing, or an estimate above the planned one or above
  `reserved_usd_micros`. Narrower changes (stricter rights, fewer bytes, a lower price) pass;
- `omuzaki-connect` wraps the `CancelToken` as `Arc<dyn Cancel>` and `JobCtx::spend` as the `Meter`.

`Preview` carries `secret: Option<SecretLabel>`: the effective key label, never a value, filled from
`Target::secret`. `Preview::widens` compares it, so a selected provider change that changes the label is
`Widening::Secret`.

Acceptance:
- a job queued behind another, whose input bytes gain a `private_test` tray record between its plan and its work,
  sends nothing and fails `PrivateInput`; a record that turns an input's `commercial` to `no` while the call runs
  makes the landed candidates `no`;
- a second connector for the same capability enabled between plan and work is not chosen (the plan's id runs); a
  price raised between plan and work (a config plan change) fails `PreviewChanged` with zero requests;
- an agent queues a free local call sending a project image; the person changes that connector's endpoint to a LAN
  host before the job starts: `PreviewChanged` (`leaves_machine`), zero requests, nothing charged; the same with a
  person as requester also fails (the planned preview said "stays on this computer"); an endpoint change while the
  run is under way does not reach it (the snapshot);
- two `llm.providers` entries with distinct keys: with provider B selected, `key_status` and `status` report B's key
  (B missing is `NeedsKey` though A's is present), and a scripted error echoing B's key comes back as `[key]` while
  A's key text is untouched; switching the selection to A between plan and work fails `PreviewChanged`;
- the same for LLM and vision: a queued local vision call (provider on loopback) whose provider gets a remote
  endpoint with `allow_remote` before work starts fails `PreviewChanged` with zero turns sent, even though that remote
  host was consented to before; with a `private_test` image it stays `PrivateInput`; a provider price override raised
  between plan and work fails `PreviewChanged`; `FakeLlm` records that the client was built from the snapshot;
- a paid fake that bills two outputs and then hits a store failure has metered both charges (the reservation settles
  to them); a cancel after the first billed output meters exactly one;
- conformance: the sum of `Meter::charge` equals `Usage.usd_micros`, and a meter that refuses stops the run with
  `CapReached` before the next billable request;
- an `HttpStream` whose body stalls after its headers ends with `Cancelled` within 250 ms of the job's cancel.

### D5. Grants and money

- **A plan with `cost.usd > 0` needs `spend`** (executor policy): the scope is checked before the budget, an
  agent session without it gets `Need::Scope { scope: Spend }`, a dry-run never needs it, a person holds it. **Then
  the session budget**: `spent + cost` past the cap pauses the session with `Need::Budget`. The two stay
  separate pieces of work.
- **Every call has a cap:** `max_usd_micros`, defaulting to the estimate. The host refuses before sending if the
  estimate is above it; adapters check before each billable request (`CapReached`); a person's confirm card
  shows it. `expect_usd_micros` makes a dearer estimate at run time `PreviewChanged` instead of a surprise.
- **An agent never names a URL, a command line or a key.** Endpoints, MCP command lines, plan tiers and declared
  model licences are Machine settings with `AgentWrite::Never` (D12); keys are entered in the UI only (D8). The op
  schemas have nowhere to put any of them.
- **Assets leaving the machine need a person.** When the requester is an agent, the effective destination leaves
  the machine and the request sends any project asset (not just a prompt), `Preview::needs_person` is the preview's
  sentence and the op calls `ctx.require_person(sentence)`: the approval card shows exactly what is about to leave.
  A person is never asked; an agent sending only a prompt is not asked. **The issue's `publish` exemption is
  rejected:** ops never see grants (`OpCtx`), and `publish` grants exporting results, not handing inputs to a
  vendor; "approve for this session" is a decision the approval queue can grow later (open question 2).
- **No monthly cap now.** Session budgets, the per-call cap and the person's confirm card bound spend; a vendor-side
  spending limit is the user's backstop (open question 3).

### D6. Rights and provenance

**The host builds every `Provenance`; an adapter never does** (`output_provenance`):
`Source::Generated { tool: <connector id>, model: Artifact::model, plan: config.plan, prompt: prompt_text(), seed:
Artifact::seed, inputs: input_assets() }`, `licence` and `commercial` as below, `attribution` from the manifest,
`consent` as found, `private_test` if any input is.

**A baseline, then restrictions that only narrow** (`join_commercial`: `no`, then `unknown`, then `ok`, the same
order as `io-api.md` D10's `derive`). The baseline `licence` and `commercial` depend on `licence.outputs`:

| `outputs` | Baseline |
|---|---|
| `vendor_terms` | the manifest's `spdx` and `commercial` |
| `user_model` | the person's declaration for `Artifact::model` in settings (D12); undeclared: `unknown`, `LicenseRef-User-Model-Undeclared` (ADR 0016 rule 3) |
| `reported` | `Artifact::rights`, the registry's record of the model that ran; none reported: `unknown` |

The manifest's own `commercial = unknown` for the last two (D3) is only the value the preview shows before an output
exists; it is replaced by the baseline, never joined with it. Then each restriction is joined in:

1. a plan rule: `requires_plan` non-empty and `config.plan` not in it gives `no` (free-tier outputs belong to
   the vendor);
2. each input's `commercial` (its effective provenance, below): a `no` or `unknown` input makes the output no better;
3. a voice clone without a consent record: `unknown` (and the preview refuses it first).

The register row is not read at run time (it is a repo file); `cargo xtask connectors` keeps each shipped
manifest's default honest against it: `ok` needs a `yes` or `conditions` row, and a `NO` or `unverified` row allows
only `no` or `unknown`, so such a connector "can't produce shippable output" (§14) without a runtime lookup.

- **`private_test` inputs never leave the machine.** The preview refuses (`PrivateInput`) any connector whose
  effective destination leaves it, whatever the person or an agent asks; local connectors may use them, and their
  outputs inherit the flag, so the gate blocks them from `publish`.
- **A voice clone needs the consent record:** `ConsentLookup::find(person, Likeness::VoiceClone)` in the preview
  (`NeedsConsent`), and the record goes on the output's provenance, so `licence_gate(p, true)` passes.
- **No `omuzaki-store-api` change.** `Source::Generated` already has tool, model, plan, prompt, seed and inputs. The
  connector's manifest version and the full request go on the tray record (`Candidate::connector_version`,
  `Candidate::request`), which is what replay compares (D10); a request hash in the store would duplicate it.
- **Identical bytes, different rights.** `Store::put` keeps the *first* provenance for bytes it already holds, so a
  later run that produces the same bytes under stricter terms (a free plan, a `no` input, private test material)
  must not inherit an earlier, more permissive record. The tray record's `provenance` is the candidate's own, and
  **every reader of a stored hash uses its effective provenance:** `tray::effective_provenance(doc, hash, stored)`
  joins the store's record with the provenance of every tray record naming that hash, in any state, strictest wins:
  `commercial` by `join_commercial`, `private_test` if any is, distinct licences joined with SPDX `AND`, `consent`
  kept only when all agree (the `derive` rules of `io-api.md` D10). The preview's input checks (`PrivateInput`,
  inputs' `commercial`), the publish gate's callers and `io.import` read it; never `Store::provenance` alone for a
  hash the tray names. Acceptance (N4): the same bytes stored first by an `ok` run and then by a
  `no` run give `no`; first by a run without private input and then by one with it give `private_test`; reversed
  order gives the same answers.
- **The data path: the caller op resolves, the host checks.** The host holds no document (it is an App service;
  the document belongs to the executor), so the op that calls it resolves its inputs' effective provenance in `plan`,
  where it has `ctx.doc` and the `Store` service, and hands the result over explicitly:

  ```rust
  // omuzaki-connect-api
  /// The effective provenance (D6) of every asset a request names, keyed by hash.
  pub type InputRights = BTreeMap<ContentHash, Provenance>;
  pub mod tray {
      /// For each of `req.input_assets()`: `Store::provenance` joined by `effective_provenance`.
      /// A hash the store does not hold is `TrayError::UnknownAsset(hash)`.
      pub fn input_rights(doc: &dyn DocRead, store: &dyn Store, req: &Request) -> Result<InputRights, TrayError>;
  }
  pub trait Connectors: Send + Sync {
      fn preview(&self, req: &Request, connector: Option<&ConnectorId>, who: Requester, inputs: &InputRights)
          -> Result<Preview, PreviewError>;
      fn run(&self, call: &Call, env: &RunEnv<'_>) -> Result<RunResult, RunError>;   // Call::inputs: InputRights; RunEnv: D4
      // list, describe, status, ping: as drafted
  }
  ```

  The host reads only the map for rights (`PrivateInput`, the inputs' `commercial` in the join) and refuses a request
  whose map lacks one of its hashes (`UnknownAsset`); it still reads the bytes from the store. The plan's map serves
  the preview only; the job re-resolves it at work start and again at finish (D4.2, D4.3). Acceptance: bytes first stored by a local run with no private input, then produced again by a local run whose input
  is `private_test`, are refused as an input to a hosted connector (`PrivateInput`) though the store's own record
  says `private_test: false`.
- Where a candidate's rights reach a character: `io-api.md` D10's source rows. A part imported from an accepted
  candidate writes its row from the hash's effective provenance (the `io.import` rule), so `licence.derive`
  sees `Generated` with the strictest status those bytes have ever been recorded with.

### D7. The candidates tray

**Decision: records in the project document, bytes in the store.** One record per output at `tray/<candidate>`, a
new top-level node (spec §7 gains `tray/<candidate>.toml`): undo, branches, merge, the journal and recovery cover it
with no new mechanism, and a project that travels carries its decisions. The bytes are already in the store.

```rust
// omuzaki-connect-api, as drafted, with these changes:
pub struct Candidate {
    pub id: CandidateId, pub run: RunId, pub connector: ConnectorId,
    pub connector_version: Version,                // new: what replay compares (D10)
    pub request: Request,                          // the effective request: seed filled in by the host
    pub asset: ContentHash, pub media_type: String, pub kind: MediaKind,
    pub provenance: Provenance,                    // a copy, so the audit survives garbage collection
    pub usd_micros: u64, pub requested_by: Actor, pub created_at: Seconds,
    pub state: CandidateState, pub regenerated_from: Option<CandidateId>,
}
pub mod tray {
    // ROOT, path, read_all, get, add, decide, usable: as drafted
    /// Every candidate of a run, as one patch (creates `tray` when absent). What every caller op lands with.
    pub fn land(doc: &dyn DocRead, candidates: &[Candidate]) -> Result<Patch, TrayError>;
    /// The hashes the store's garbage collection must keep: those named by pending or accepted records.
    pub fn roots(doc: &dyn DocRead) -> Result<BTreeSet<ContentHash>, TrayError>;
    /// The candidates of one run, sorted by id (replay, D10).
    pub fn of_run(doc: &dyn DocRead, run: RunId) -> Result<Vec<Candidate>, TrayError>;
    /// `stored` (the store's record for `hash`) joined with every tray record naming `hash`, strictest wins (D6).
    pub fn effective_provenance(doc: &dyn DocRead, hash: &ContentHash, stored: Provenance)
        -> Result<Provenance, TrayError>;
}
```

- **States:** `Pending`, `Accepted { by, at }`, `Rejected { by, at, reason }`; only pending moves, once. A rejected
  record keeps its request and provenance for the audit log.
- **Who decides.** A person may accept or reject anything. An agent may accept or reject only what its own session
  asked for (`requested_by` is its actor); anything else is `Denied` with a hint to ask the person. This differs
  from `library.md` D9 (agents may only reject their own library candidates): accepting a tray candidate asserts no
  rights (the host wrote the provenance, and the publish gate still applies) and is an undoable document edit,
  while accepting a library candidate adds to the person's library outside the project. Spec CN-03 allows "an agent
  with that scope".
- **Consumers refuse what is not accepted.** Any op that takes a stored asset as input (`io.import`, CB-44 part
  import, sheet views, emote art, a connector's references) calls `tray::usable(doc, hash)` and refuses `Pending`
  (`Conflict`, "accept it in the tray first") and `Rejected`. An asset no record names is usable (an import, a
  library item); the same bytes accepted once are usable. Each consumer adds the check with its feature (N4).
- **Regenerate** re-issues the stored request: `seed: new` (default) clears the seed so the host draws one from the
  new run; `same` keeps it; `changes` is a JSON merge patch on the request, decoded again (a field it breaks is
  `Invalid` naming it). It works on any state and sets `regenerated_from`.
- **Garbage.** Reject and undo delete no bytes. The store's collector takes `tray::roots` among its
  roots, so bytes named only by rejected records (or by no record, after an undone generate) are collectable after
  its retention period; the record's provenance copy keeps the audit (N3).

### D8. Keys

- **Where:** the OS keychain through `SecretStore`. For `deliver = "header"` or `"env"` the label is the
  manifest's `auth.secret`, `connector/<id>`. For `deliver = "provider"` the manifest has no `auth.secret`; the label
  is the selected `llm.providers` entry's `SecretRef` (llm-api D6, usually `connector/llm/<provider id>`).
  `Target::secret` is authoritative in both cases. Never in project files, settings, logs, op records, events or MCP output (CN-05).
- **Who attaches it:** the host's `AuthedHttp`, for `deliver = "header"`: the adapter's requests go through
  it; it adds `<name>: <scheme> <key>`, refuses any request that sets an auth header itself, sends only to the
  allowed hosts (the effective endpoint's host plus the manifest's destinations) and **never sends a key over plain
  `http://` to a host that is not loopback** (`Refused`). For `deliver = "env"` the MCP `StdioOpener` puts the key in
  the child's environment, never argv. For `deliver = "provider"` the seam's provider resolves it (D2).
- **One effective label:** every key operation uses `Target::secret` (D4), never the manifest field: `status`
  (`NeedsKey`), `connect.key_status` (which resolves the target first), `AuthedHttp` and scrubbing. For an LLM
  connector that is the selected provider's `SecretRef`, the same label the seam's client resolves, so status and
  scrubbing can never point at another provider's key.
- **Scrubbing:** the host resolves the key behind `Target::secret`, whatever the delivery, and runs every string
  that comes back (response headers and bodies through `AuthedHttp`; every `ConnectError` string, including
  `stopped`) through `scrub_str` (the key, its percent-encoded and base64 forms become `[key]`).
- **Entry is UI-only:** the masked field calls `SecretStore::set` directly; no op reads or writes a value;
  `connect.key_status` uses `contains`.

### D9. Cancellation, progress, partial results, streaming

- `Connector::run` is blocking, on a job worker. It checks `cx.progress.is_cancelled()` between requests and polls,
  and transports return `Cancelled` within 50 ms of it (`omuzaki-http`) or 250 ms (an MCP call).
- **A cancelled or failing run keeps its complete outputs** (`RunOutput { stopped: Some(e) }`) and charges what was
  delivered; `Err` means no output at all. The job lands the complete ones (D4.3).
- Progress stages: `send`, `wait` (with the vendor's fraction when it reports one), `fetch`, `store`.
- **LLM streaming for the assistant (AG-01): never a second `Connector` method.** The assistant needs streaming, tool
  use and its loop, which are the seam's; it calls the seam directly and charges tokens and money with
  `Sessions::charge`. A connector call is one finished turn.

### D10. Replay and recovery

- **Recovery never calls a connector.** The journal holds the completion patch, so recovery folds patches (ADR
  0021); a job running at the crash has no completion record and is reported as interrupted, never re-run silently.
  Bytes it stored are unreferenced and collectable.
- **Seeds.** A request without a seed gets `Rng::new(run).next_u64()` from the job's run id (ADR 0009), and the
  effective request is what the tray records.
- **TH-12 re-run** re-issues `connect.generate` as a request (agent-sessions 9.2). The replay driver passes
  `reuse: Some(<original run>)`. Reuse happens, and costs nothing and sends nothing, only when every record of that
  run is still in the document, its connector and `connector_version` equal the chosen connector's, its request
  equals the new one (the seed compared only if the new request names one), and every hash is in the store. Then new
  pending records for the new run name the same hashes. Otherwise the call runs again, needing `spend` and a budget
  again. Text and JSON answers are not recorded, so an LLM or vision call always runs again.

### D11. Testing

- **Fakes for every capability** (`omuzaki-connect-mock`): deterministic bytes per (request, index),
  scripted latency (through `FakePace`, never sleeping), cost and failures. `FakeLlm` is the LLM and vision
  fake; a `local-llm` connector over it is tested in `omuzaki-connect-llm`.
- **`FakeHttp`** (`omuzaki-http-mock`, D13) answers from routes or **cassettes written by hand from the vendor's public
  API documentation**, never recorded with a real key or account, holding no private material.
- **A conformance suite** every connector's own tests run: manifest, pure estimate, estimate bounds usage,
  cancel before any request, cap before any request, outputs match the manifest, unsupported refused, secrets stay
  out, transport errors map.
- **Real transports against local test servers** (`omuzaki-http` against `hyper` on `127.0.0.1:0`); **vendor adapters
  against hand-written fixtures**; **live smoke tests are run by a maintainer with their own keys** and recorded in the PR. **No test
  calls a paid or hosted service**, and CI has no key.

### D12. Configuration

| What | Kind | Where |
|---|---|---|
| enabled, endpoint, plan tier, MCP command line, declared model licences (`ConnectorConfig`) | setting `connectors.entries` (one key, a map by id; settings D2 wants two segments): `Machine` scope, `AgentWrite::Never`, `InProfile::Excluded` | `machine.toml` |
| API keys | secret, label `connector/<id>` | OS keychain |
| tray records | project state | `tray/<candidate>.toml` |
| the candidate bytes | store objects | the content store |
| manifests | code (embedded) | the connector's crate |

- `Machine` because an endpoint, a command line and a plan tier are about this computer and this account (settings
  D3); `Never` because each touches network, processes or rights (settings D4's rule).
- **A non-loopback endpoint makes a "local" connector one that leaves the machine,** in the preview too, with a
  warning. A person may set a plain-`http://` LAN endpoint (ComfyUI on a GPU box is the common case): it is allowed,
  the preview warns that it is unencrypted, and no key is ever sent over it (D8). The earlier loopback-only `http://` rule is amended to allow
  `http://` for any host for this reason.
- Every connector starts disabled; nothing runs until a person enables it.

### D13. One HTTP stack

**Decision: `omuzaki-http` is the one implementation, and the transport contract is its own API crate,
`omuzaki-http-api`.** The page fetcher chose `reqwest` (blocking, rustls, system proxy) and `net.http` chose
`ureq`. Two clients mean two TLS stacks (`aws-lc-rs` and `ring`), two proxy behaviours and two sets
of timeouts and caps in one binary. Implementation crates may not depend on each other, so the only way for
`omuzaki-connect*`, `omuzaki-live-nodes` (`net.http`) and `omuzaki-llm` to share one client is a trait in an API
crate:

```rust
// omuzaki-http-api: serde and thiserror only, no network stack
pub trait Cancel: Send + Sync { fn is_cancelled(&self) -> bool; }
pub enum Method { Get, Post, Put, Patch, Delete }
pub enum HttpBody { Empty, Bytes { content_type: String, data: Vec<u8> }, Json(Value), Multipart(Vec<Part>) }
pub struct Part { pub name: String, pub file_name: Option<String>, pub content_type: Option<String>, pub data: Vec<u8> }
pub struct HttpRequest { pub method: Method, pub url: String, pub headers: Vec<(String, String)>, pub body: HttpBody,
                         pub timeout_s: u32, pub max_response: u64 }
pub struct HttpResponse { pub status: u16, pub headers: Vec<(String, String)>, pub body: Vec<u8> }
/// A streaming answer (server-sent events): the body is read as it arrives. It owns the cancel handle
/// it was opened with, so a stalled body still stops: `read` fails within 50 ms of `is_cancelled()` turning true,
/// with an `io::Error` whose inner error is `HttpError::Cancelled`, and the connection is dropped.
pub struct HttpStream { pub status: u16, pub headers: Vec<(String, String)>, pub body: Box<dyn Read + Send> }
pub enum HttpError { Cancelled, Timeout, Network(String), TooLarge { limit: u64 }, Refused(String) }
pub trait HttpTransport: Send + Sync {
    /// The whole answer. A non-2xx status is a response, never an error.
    fn send(&self, req: &HttpRequest, cancel: &dyn Cancel) -> Result<HttpResponse, HttpError>;
    /// The answer as a stream; `max_response` and `timeout_s` still bound it.
    /// Takes the handle owned (`Arc`) because the returned stream outlives this call and keeps checking it.
    fn open(&self, req: &HttpRequest, cancel: Arc<dyn Cancel>) -> Result<HttpStream, HttpError>;
}
```

`omuzaki-http`'s `ConnectHttp` follows the URL rules that apply to every caller: `http` and `https` only, no
user-info, **no redirects followed** (a 3xx is a response), whole-call timeout, streamed body cut at `max_response`,
8 requests in flight, cancel within 50 ms, `User-Agent: Omuzaki/<version>`. **Which hosts a caller may reach is the
caller's rule, above the transport:** connectors through `AuthedHttp` (D8), `net.http` through the person-approved
`Need::Host` (automation D11), the seam through its own settings. The host adapts `Progress` to `Cancel` with a
one-line wrapper; for `open` it wraps the job's owned `CancelToken` (D4), which outlives the borrow. Acceptance for
`open` (N1): a body that stalls after its headers and first chunk returns `Cancelled` from `read` within
250 ms of the handle being cancelled (one real-clock test), and `FakeHttp`'s scripted stalls do the same. Consequences: the connect-api draft drops its HTTP types (they move here), its `FakeHttp` moves to
`omuzaki-http-mock`, `net.http` is re-cut onto `HttpTransport` (its fence loses `ureq` and its TLS rows), and the LLM seam is
recommended to build its providers on `open`: llm-api D11 already routes every request through one
`omuzaki_llm::http` module that "becomes its implementation" of such a trait; the provider work decides whether the
async seam wraps the blocking `open` or the trait gains an async twin. `HttpFetcher` and its `deny.toml` exception stand.

### D14. The phase cut

**Phase 3** (CN-01 to CN-09, CN-11, CN-12 are Phase 3 features): the framework (and the new
`omuzaki-http-api` and mock), `omuzaki-connect-mcp`, ComfyUI, the LLM and vision wrapper (the wrapper first,
then its manifests), the hosted-image research, the UI work, the App
wiring (N2), GC roots (N3) and the consumer guards (N4). Meshy is Phase 3, P1, and waits on the plan-tier rights question. **Phase 4:**
CN-10, the local TTS bridge and the rest of CN-10.

| Feature | What it still needs beyond the framework |
|---|---|
| CN-07 3D (P1) | a Meshy manifest (needs a maintainer decision on the plan-tier rights); a Tripo manifest after the same terms check (N9); TRELLIS stays ML-06's local path, offered later through the reverse bridge; results land as candidates, and CB-44's part import refuses until accepted (N4) |
| CN-08 DCC (P1) | a Blender MCP manifest over `McpConnector` (capability `dcc`, the person's own Blender MCP command line, never bundled; N5); **PSD layer I/O has no issue: filed as N6** (read and write layered PSD for the Krita and Photoshop round trip, a `omuzaki-io` format, licence-clean reader) |
| CN-10 voice (Phase 4) | the local TTS bridge; a hosted-TTS research note and vendor pick (N7); voice clone requests need the consent hook and `ConsentLookup` over the project's records; voice conversion is ML-14's slot, bridged like the local TTS bridge |
| CN-11 music and SFX (P1) | a research note on hosted music and SFX generators (terms, ownership, price) like the hosted-image note (N8); the framework and `mock-music` already cover the rest |
| CN-12 motion (P1) | own recordings and BVH or FBX files are imports, not connectors (`io`); video-to-motion needs a licence-clean research note (N10); outputs of a motion connector are candidates like any other |

## 4. Worked example: a ComfyUI-like call, from preview to accepted candidate

**The manifest** (`crates/omuzaki-connect-comfyui/connectors/comfyui.connector.toml`):

```toml
id = "comfyui"
name = "ComfyUI"
version = "1.0.0"
transport = "http"
endpoint = "http://127.0.0.1:8188"

[[capabilities]]
capability = "image"
inputs = ["image", "text"]
outputs = ["image"]
max_count = 8
params = [
  { name = "steps", kind = { int = { min = 1, max = 150 } }, default = 20, summary = "Sampling steps." },
  { name = "cfg", kind = { float = { min = 0.0, max = 30.0 } }, default = 7.0, summary = "Prompt strength." },
]

[auth]
kind = "none"

[cost]
model = "free"

[privacy]
leaves_machine = false
destinations = ["127.0.0.1"]
sends = ["prompt", "images"]
retention = "Stays on this computer."
trains_on_inputs = "no"

[licence]
register = "ComfyUI"
outputs = "user_model"
spdx = "LicenseRef-User-Model"
commercial = "unknown"
```

**The person's settings** (`machine.toml`, key `connectors.entries`): enabled, the default endpoint, and a declared licence
for their checkpoint.

```toml
[connectors.entries.comfyui]
enabled = true
models = { "sd_xl_base_1.0.safetensors" = { spdx = "LicenseRef-OpenRAIL-M", commercial = "ok" } }
```

**1. The agent asks for a dry-run** of `connect.generate`:

```json
{ "op": "connect.generate", "dry_run": true,
  "args": { "request": { "capability": "image", "prompt": "chibi fox mascot, front view", "negative": null,
            "width": 1024, "height": 1024, "count": 2, "seed": null, "references": [],
            "model": "sd_xl_base_1.0.safetensors", "loras": [], "params": { "steps": 30 } } } }
```

**2. The plan** (no connector called, nothing spent):

```json
{ "patch": [], "cost": { "usd": 0.0, "seconds": null },
  "output": { "connector": "comfyui", "name": "ComfyUI", "leaves_machine": false, "destinations": ["127.0.0.1"],
              "outbound": [{ "what": "prompt", "count": 1, "bytes": 28 }],
              "estimate": { "usd_micros": 0, "exact": true, "basis": "free", "seconds": null },
              "retention": "Stays on this computer.", "trains_on_inputs": "no",
              "commercial": "ok", "licence": "LicenseRef-OpenRAIL-M", "plan": null,
              "needs_spend": false, "needs_person": null, "warnings": [],
              "english": "This stays on this computer and costs nothing." } }
```

**3. The job runs** with run id 42: the host draws seed `Rng::new(42).next_u64()` (the value below is illustrative), ComfyUI returns two PNGs, the host
stores each with provenance and lands two pending records in one transaction, "Generate 2 images with comfyui".
**The provenance record** the store keeps for the first:

```json
{ "source": { "kind": "generated", "tool": "comfyui", "model": "sd_xl_base_1.0.safetensors", "plan": null,
              "prompt": "chibi fox mascot, front view", "seed": 1311768467294899695, "inputs": [] },
  "author": null, "licence": "LicenseRef-OpenRAIL-M", "attribution": null, "commercial": "ok",
  "consent": null, "private_test": false }
```

**4. The tray record** at `tray/r42-0` (`tray/r42-0.toml` in the project), after the person accepts it:

```toml
id = "r42-0"
run = 42
connector = "comfyui"
connector_version = "1.0.0"
asset = "9f2c…"                  # the 64-hex-digit content hash, shortened here
media_type = "image/png"
kind = "image"
usd_micros = 0
requested_by = { kind = "agent", id = "session-7" }
created_at = 1734.5

[state]
state = "accepted"
by = { kind = "person", name = "person" }
at = 1801.0

[request]                          # the effective request: the seed is filled in
capability = "image"
prompt = "chibi fox mascot, front view"
width = 1024
height = 1024
count = 2
seed = 1311768467294899695
model = "sd_xl_base_1.0.safetensors"
params = { steps = 30 }

[provenance]                       # the copy of the record above
licence = "LicenseRef-OpenRAIL-M"
commercial = "ok"
source = { kind = "generated", tool = "comfyui", model = "sd_xl_base_1.0.safetensors", seed = 1311768467294899695 }
```

Absent options are omitted, and `Actor` is `omuzaki-ops-api`'s tagged form. `r42-1` stays pending: `io.import` of its
bytes is refused with "accept it in the tray first" until a person (or the asking agent's session) accepts it.

## 5. Crates and contract changes

| Crate or file | Change |
|---|---|
| `omuzaki-connect-api` (new) | D1 to D9 contracts |
| `omuzaki-http-api`, `omuzaki-http-mock` (new) | D13 |
| `omuzaki-connect-mock`, `omuzaki-connect`, `omuzaki-connect-{comfyui,mcp,llm,models}` (new) | as designed here |
| `omuzaki-http` | `ConnectHttp` beside `HttpFetcher` |
| `omuzaki-ops` | the spend scope, the session budget and the job path |
| `omuzaki-store` | the collector takes `tray::roots` (N3) |
| spec §7, §9 | `tray/` in the layout; the `omuzaki-ai` row split; four connector rows |
| `crates/README.md` | a planned-crates note for `omuzaki-connect*`, `omuzaki-http-api`, `omuzaki-http-mock` |

`cargo xtask lint-deps` accepts the graph: `omuzaki-connect-api` depends on `omuzaki-core`, `omuzaki-store-api`,
`omuzaki-ai-api`, `omuzaki-ops-api`, `omuzaki-doc-api`, `omuzaki-http-api` and `omuzaki-secrets-api` (`SecretLabel`; API to API); the mocks on API crates;
the implementations on API crates (mocks as dev-dependencies); `omuzaki-stage` wires them.

`SecretLabel` makes the keychain `SecretStore` a prerequisite of the first connector that uses it.

## 6. Where the sources disagree, and the pick

| Disagreement | Pick and reason |
|---|---|
| `reqwest` (page fetcher) vs `ureq` (`net.http`) | `reqwest` in `omuzaki-http` behind `omuzaki-http-api` (D13): one TLS stack and proxy |
| The draft (agents may accept their own) vs `library.md` D9 (agents reject their own only) | agents may accept and reject their own tray candidates (D7): tray acceptance asserts no rights and is undoable |
| The earlier rule (`http://` only for loopback) vs (a LAN endpoint is allowed with a warning) | LAN `http://` allowed, flagged as leaving the machine and unencrypted, never carrying a key (D12, D8) |
| The first draft (an agent holding `publish` skips the confirmation) vs `OpCtx` (ops never see grants) | always confirm (D5); a per-session approval is open question 2 |
| Manifest prices pinned to the seam's vs "one source, or a test" | one source, the seam's table (D2) |
| Seed from the run id vs TH-12 reuse | both: seed from the run id, reuse by `reuse: <run>` (D10) |

## 7. Alternatives rejected

- **Tray as a store-side table or a sidecar file:** it would need its own undo, merge, branch and journal story;
  document records get all four for free.
- **Text answers in the tray:** an answer is used at once by the op that asked; a pending text nobody accepts helps
  no one, and the op record already holds it.
- **Adapters reading the store or the keychain:** least privilege; a bug in one adapter cannot read other assets or
  leak a key (`Inputs` gives only the request's assets).
- **A `Connector::stream` method for AG-01:** duplicates the seam (D9).
- **Prices in the manifest for LLM providers:** two copies drift; the seam's dated table is the one source (D2).
- **A connector-version or request-hash field in `Source::Generated`:** a store API change that duplicates the tray
  record (D6).

## 8. Sources

Spec v3 §6, §7, §9, §14, §35, §39; `docs/design/{agent-sessions,ai-api,io-api,library,ops-api,settings}.md`;
`docs/design/automation.md` (D11, §5; read 2026-10-13); ADRs 0009, 0015, 0016, 0017, 0018, 0020, 0021,
0026; `crates/omuzaki-{store,ops,ai}-api/src` (read on 2026-10-13). No external source was needed: no vendor
term or price is quoted here (each vendor connector reads them at the primary source, dated).
