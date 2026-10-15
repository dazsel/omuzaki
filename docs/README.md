# Docs index

## Product
1. `product/02-studio-spec-v3.md`: **the Rust studio spec**. Architecture, MCP/AI core, Build Theatre, connectors, 250 numbered features (P0/P1/P2 is build order; every one ships), library, craft, implementation plan.

## Design
- `design/style-guide-soft-sd.md`: the default Soft SD character style.
- `design/ai-performer.md`: the AI performer: crates, persona, the turn loop with barge-in, the inline-tag LLM output protocol and the intent API (ADR 0031).
- `design/mcp-resources.md`: the MCP resource provider contract, the `omuzaki://` URI table and the `omuzaki://project` summary shape.
- `design/team.md`: team roles over the scopes, the signed membership log, enforcement, agency accounts and the SSO, LB-07 and git hooks (ADR 0043).
- `design/team-review.md`: review policy for publish and broadcast (VC-11) and the team audit trail (ADR 0043).
- `design/platform-export.md`: platform exports (ADR 0047): limit tables as data, the fit pipeline (strip, drop, meshes, textures, atlas, bones, decimate), platform profiles, the `platform.*` ops, variants and the worked Quest example.
- `design/track-api.md`: the tracker contract (PF-01): the camera seam, the `Tracker` trait, threads and real-time rules, clock mapping, the newest-wins mailbox, lost tracking, budgets, the permission seam and the unlocked tracker tasks.
- `design/llm-api.md`: the LLM seam (ADR 0032): the `LlmClient` trait, blocking streams, the provider-neutral message model and MCP mapping, micro-dollar pricing and the pre-request bound, retries and cancellation, providers, privacy, `FakeLlm` and the conformance suite.
- `design/git.md`: the git integration (ADR 0045): the person's own `git` as a process, what "commits cleanly" promises, binaries via the second location, the commit view with render previews, person-only commit and push, and the `Git` trait.
- `design/identity.md`: keys, signatures and trust (ADR 0042): one ed25519 key per person per machine by default, one per project when needed, in the OS keychain, the domain-tagged signature envelope, the trust list, signed exports (`OMUZAKI_signed_export`), key statements, agent rules and the watermark hook.
- `design/agent-runtime.md`: the in-app assistant's host: crates, the turn loop, Stop, context and compaction, conversation storage, settings, host-local tools and the chat panel contract (ADR 0040).

## Research
Dated notes. Licences and facts go stale, so always check the date.
- `research/2026-10-rust-stack.md`: Rust crates for rendering, UI, assets, ML, media, MCP, plugins, collab.
- `research/2026-10-reference-machine-definition.md`: the evidence behind the named reference machines in §10 (hardware tiers, the CPU and RAM counting rules, measurement choices, which budgets are at risk).
- `research/2026-10-crdt-fit.md`: can loro carry the project document and our patches? A three-peer spike: convergence, last-writer-wins conflicts, costs, alternatives (automerge, yrs, a sequencer), recommendation.
- `research/2026-10-vts-shim-terms.md`: answering the VTube Studio API as a server (PL-05): terms, wire identity, what stream tools send, safety, which requests to answer or refuse, naming rules.
- `research/2026-10-model-watermarking.md`: watermark and theft-protection techniques for exported avatars (what survives Blender re-export, JPEG, resize, decimation), a measured recommended texture mark, the attack model and the open legal points.
- `research/2026-10-text-safety-filters.md`: text-safety filters for the AI performer's speech (AP-07): twelve candidates with the three licence checks, language coverage and claimed latency, the recommended fail-closed chain and a licence-clean test-set plan.

## Decisions
- `decisions/`: one short record per architectural decision (context, decision, consequences).
