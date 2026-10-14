# 0016: People can put their own models in any model slot; the licence is theirs, and the provenance is ours to keep (2026-10-12)

**Status:** accepted (2026-10-12). **Amended 2026-10-12:** voice conversion (ML-14) joins the slots below; a converted voice that imitates a real person also needs that person's consent record (SE-05). **Context:** ML-01 (the model registry), ML-02 to ML-05 (the built-in slots), CN-01 (connectors), licence records (strictest of parts), ADR 0013 (file roots), `docs/licensing/policy.md`.

## Context

Connectors already let people bring any hosted model, or any local model behind a standard server: Ollama, llama.cpp, LM Studio, vLLM, ComfyUI. The built-in slots run inside the app, so they don't have that freedom:
- speech to text;
- voice activity;
- text to speech;
- voice conversion (ML-14, added after this decision);
- face, hand and pose tracking;
- voice to face.

Each is fixed to the model we ship. People who want a bigger Whisper, a voice they trained, or a newer tracker can't use one, so the app falls behind the frontier. Our licence policy, "nothing non-commercial", governs what **we** ship, bundle or download. It can't govern a model a person chooses and installs, and it shouldn't stop them.

## Decision

1. **Every built-in slot accepts a user-supplied model.** A slot is the versioned interface ML-01 already defines (fixed inputs and outputs). A model fits a slot when its manifest declares that slot and version, and the registry's checks pass (rule 4). A local model is a file the person picks (ONNX, GGUF, or a slot-specific format) plus a small manifest, which the app writes for them when the model has none.
2. **The licence is between the person and the model's author.** We don't make, ship, bundle, mirror or auto-download a user-supplied model.
   - When it's added, the app shows the licence it declares (or "unknown") and a plain disclaimer, and records one acknowledgement per model hash. The disclaimer: *"This model isn't made or distributed by Omuzaki. You chose it; its licence and terms are between you and its author. Omuzaki checks that it runs safely and records it in your project, but doesn't vouch for its quality, safety or licence."*
   - Our licence policy and register cover what we ship. User models are out of the register's scope by definition, and the policy says so.
3. **Provenance flows into what it makes.** Whatever a user model produces carries "made with model X (hash, declared licence Y)" in the licence record (strictest of parts): a voice, a take's tracking, generated art.
   - Export and `io.check_export` warn when a non-commercial or unknown licence would reach a commercial export. They inform, and never block personal use.
   - Projects and exports reference a user model by name and hash, and never embed its weights.
4. **Models are untrusted input.** Before a model is enabled:
   - its format, size and hash are checked;
   - ONNX custom-op libraries and any other native code a model asks to load are refused;
   - the model loads in a separate, resource-limited worker;
   - it must pass the slot's smoke test, a small eval with a quality floor: speech to text on a short public-domain clip set, text to speech round-tripped through speech to text, a tracker on synthetic faces.

   A model that fails stays disabled, and the report says why. A model that crashes or stalls at run time falls back to the shipped model, and the health meter shows it.
5. **People choose and agents ask.**
   - People pick the model per slot on a Models page in Settings, with a live comparison against the shipped model where the slot allows it.
   - Agents can list models and *request* a switch through ops. Switching to a model the person hasn't acknowledged needs the person's confirmation, never the agent's.
   - The shipped model is always one click away.

## Consequences

- Spec: ML-13 (user-supplied models and the Models page). Policy: a "User-supplied models" section.
- Tasks:
  - the manifest and slot validation in `omuzaki-ai`;
  - the sandboxed loader and smoke tests;
  - the acknowledgement and provenance in licence records;
  - the `model.*` ops;
  - the Models page (after the app shell).
- Rejected:
  - **Allow-listing approved community models:** we'd own their licences and fall behind anyway.
  - **Banning non-commercial user models:** it's the person's choice, and the policy can't reach it.
  - **Silent use with no record:** commercial exports would carry rights problems nobody can see.
