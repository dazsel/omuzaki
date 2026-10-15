---
title: "0030: Connectors are self-registered manifests behind one host that previews, caps, records and trays every call (2026-10-13)"
summary: "Connectors are self-registered manifests behind one host that previews, caps, records and trays every call"
area: decisions
---

# 0030: Connectors are self-registered manifests behind one host that previews, caps, records and trays every call (2026-10-13)

**Status:** proposed. It adds three new API crates
(`omuzaki-connect-api`, `omuzaki-http-api`, and the mock crates beside them), an executor rule and the job path.
**Context:** CN-01 to CN-12; spec v3 §6, §7, §9, §14, §39 rules 2 and 6; ADR 0009
(determinism), 0015 (registration linking), 0016 (user-supplied models), 0017 (model registry), 0018 (the spec row
split it follows), 0020 (agent sessions and budgets), 0021 (journal and recovery), 0026 (settings). Design:
`docs/design/connectors.md`.

## Context

Spec §14 promises connectors with a manifest, provenance on every output, a candidates tray, a cost and privacy
preview and keys in the OS keychain. Twelve features and about thirty filed tasks depend on one
contract that did not exist, while the pieces it must reuse do: the content store and its licence gate, ops with
cost hints and approvals, session budgets, the model registry's manifest pattern, the secret store, the
settings registry, the HTTPS fetcher and the LLM seam. Two designs picked different
HTTP clients (`reqwest` and `ureq`).

## Decision

1. **Crates (design D1).** `omuzaki-connect-api` (contracts and the `Connectors` service trait),
   `omuzaki-connect-mock` (fakes and the conformance suite), `omuzaki-connect` (the host and the `connect.*` ops), one
   `omuzaki-connect-<family>` crate per adapter family. Other crates' ops reach the host through
   `ctx.service::<dyn Connectors>()` and land results with `tray::land`. `omuzaki-stage` wires `Services` for every
   App. The spec's `omuzaki-ai` row keeps the model registry only; connectors, the assistant's model client and
   cost tracking (ADR 0020) move out. Connectors are not registry runtimes; a registry slot may be offered as a
   connector (the reverse bridge).
2. **A closed capability set with one typed request each** (D2): image, texture, model3d, voice, music, motion, llm,
   vision, dcc. Adapter knobs go in `params`, declared in the manifest and checked by the host. Bytes become
   candidates; text and JSON answers go back to the caller and never enter the tray.
3. **LLM and vision wrap the seam** (D2): one turn mapped onto `TurnRequest` (ADR 0032), with no caller tools; a JSON schema becomes one forced internal tool whose arguments are the answer. The seam's provider
   resolves its own key (`deliver = "provider"`), the seam's dated price table is the only price source
   (`cost = "provider"`), and the job reports tokens and money once for the executor to charge. Streaming and tool use
   stay in the seam; `Connector` gets no streaming method.
4. **Manifests are TOML files embedded and self-registered** (D3), `register_connector!("<id>",
   include_str!(..), Type)`, validated with every problem reported, gated against the licence register by
   `cargo xtask connectors`. An MCP connector is a manifest with a tool map over one `McpConnector`.
5. **Ops** (D4): `connect.list`, `describe`, `key_status`, `preview`, `tray_list` (queries), `accept`, `reject`
   (edits), `generate`, `test`, `regenerate` (jobs, scope `net`). A dry-run of `generate` is the preview and calls
   nothing. Connectors use the job path (ADR 0036): `plan` previews; `work` re-resolves the inputs' rights
   from the live document before anything is sent (it gets no payload from `plan`); `finish` joins them with the
   rights at finish time and lands the candidates as one undo step (partial results included). The `JobId` is the
   run id. `work` runs the plan's connector under the plan's reservation, with the connector's target (its `connectors.entries` config and, for LLM connectors, the `llm.providers` entry) resolved once per run and shared by preview, price, client and key handling (`Target::secret`, the one effective key label) and the preview rebuilt and refused (`PreviewChanged`) if it widens what the plan showed, and reports each billable request through a
   `Meter` wired to `JobCtx::spend` as it completes; owned cancellation comes from a `CancelToken`. These need three
   `JobCtx` accessors added by the jobs T1 (lead ruling 6149160268: `plan_output`, `reserved_usd_micros`,
   `cancel_token`). A running job is
   never re-run after a crash.
6. **Money and grants** (D5): a plan with `cost.usd > 0` needs `spend`, then the session budget;
   every call has a cap defaulting to its estimate and fails `PreviewChanged` if the estimate rose. Agents never name
   a URL, command line or key. An agent sending project assets to a destination off the machine needs a person's
   confirmation with the preview's sentence, always. No monthly cap now.
7. **Rights** (D6): the host builds every `Provenance` from `Source::Generated` without a store API change;
   `licence` and `commercial` start from a baseline (the manifest for vendor terms, the person's declaration for a
   user model, the registry's report for a slot) and then only narrow (plan rule, each input, voice consent).
   Because the store keeps the first provenance for identical bytes, every reader of a stored hash uses its effective
   provenance: the store's record joined with every tray record naming it, strictest wins. The calling op resolves
   it from its document and passes it to the host (`InputRights`); the host holds no document. `private_test` inputs never leave the machine. A voice clone needs the consent
   record.
8. **The tray** (D7): records at `tray/<candidate>` in the project document, bytes in the store; pending, accepted,
   rejected, decided once; a person decides anything, an agent only what its own session asked for; consumers refuse
   a pending or rejected asset through `tray::usable`; garbage collection keeps what `tray::roots` names.
9. **Keys** (D8): the keychain, label `connector/<id>`; the host's `AuthedHttp` attaches a header key, sends it only to
   the manifest's and the configured endpoint's hosts and never over plain `http://` off the machine, and scrubs every
   string that comes back; MCP keys go in a child's environment, never argv; no op reads or writes a value.
10. **Replay and recovery** (D10): recovery folds patches and never calls a connector; seeds come from the run id; a
    TH-12 re-run reuses a run's recorded outputs (free, nothing sent) when the request, connector version and bytes
    all match, and otherwise calls again under `spend` and the budget.
11. **Testing** (D11): fakes for every capability, hand-written cassettes from public docs, a conformance suite
    every connector passes, real transports against local servers, maintainer-run live smoke tests. No test calls a paid
    or hosted service.
12. **Configuration** (D12): one settings key `connectors.entries` (Machine, `AgentWrite::Never`, excluded from profiles);
    keys in the keychain; only tray records in the project. A non-loopback endpoint makes any connector one that
    leaves the machine; a plain-`http://` LAN endpoint is allowed with a warning and never carries a key.
13. **One HTTP stack** (D13): `omuzaki-http` (`reqwest`, rustls) is the only HTTP client; its transport contract is
    the new API crate `omuzaki-http-api` (`HttpTransport` with `send` and a streaming `open` that owns its `Arc<dyn Cancel>`), with
    `FakeHttp` in `omuzaki-http-mock`. Connectors, `net.http` (re-cut off `ureq`) and the seam's providers
    share it (recommended). Host rules are each caller's, above the transport.
14. **Phase cut** (D14): the framework, MCP, ComfyUI, LLM and vision, hosted-image research and the UI are Phase 3;
    Meshy is Phase 3 P1, behind the paid-plan licence check; CN-10 is Phase 4.

## Consequences

- New crates: `omuzaki-connect-api`, `omuzaki-connect-mock`, `omuzaki-connect`, `omuzaki-connect-comfyui`,
  `omuzaki-connect-mcp`, `omuzaki-connect-llm`, `omuzaki-connect-models`, `omuzaki-http-api`, `omuzaki-http-mock`. New
  dependencies only where tasks already list them (rmcp client features); each with its register row in its
  task.
- The spec gains `tray/<candidate>.toml` in the §7 layout and four §9 rows; the `omuzaki-ai` row narrows.
- Existing tasks are re-cut: the connector API and mock tasks lose the HTTP types and `FakeHttp` to
  the new crates; the manifest task gains `Transport::Llm`, `Deliver::Provider`, `CostModel::Provider` and `ParamDecl`;
  the tray task gains `connector_version`, `tray::land`, `roots`, `of_run` and `effective_provenance`; the generate task gains `reuse`;
  the settings task allows LAN `http://`; the Claude manifest task drops its price-pin test; and the `net.http` task moves to `HttpTransport`.
- New tasks: `omuzaki-http-api` and its mock, the App's `Services` wiring in `omuzaki-stage`, GC roots from the tray, the
  consumer guards, a Blender MCP manifest, PSD layer I/O, and research notes for hosted TTS, music and SFX, and
  video-to-motion.
- `connect.generate` and every connector job wait for the job path.

## Rejected

- **Connectors as registry runtimes or inside `omuzaki-ai`:** a slot is a pinned-file tensor session; a connector has no
  files, costs money, leaves the machine and returns files. One crate for registry, vendors and assistant is a
  merge hotspot (§39).
- **Two HTTP clients** (`reqwest` for fetching, `ureq` for `net.http`): two TLS stacks and proxy behaviours in one
  binary, and no shared fake.
- **Manifests loaded from disk at run time:** they could claim any licence; shipped manifests pass the register gate.
- **The tray outside the document** (a store table or sidecar file): it would need its own undo, merge, branches and
  journal.
- **Prices copied into LLM manifests:** two copies drift; the seam's dated table is the one source.
- **An agent's `publish` grant waiving the confirmation for assets that leave:** ops never see grants, and `publish`
  covers results, not inputs handed to a vendor.
- **A `Connector::stream` method for the assistant:** it duplicates the seam.
- **A connector-version or request-hash field in `Source::Generated`:** a store API change that duplicates the tray
  record.
