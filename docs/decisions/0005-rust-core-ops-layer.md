# 0005: Rust core with one operation layer; egui + wgpu (2026-10-11)

**Context.**
- The studio must be fast, light and fully agent-drivable (spec v3 §2, §4).
- The Rust stack survey (`docs/research/2026-10-rust-stack.md`) found: wgpu mature; Bevy fast-moving with no editor; egui the best fit for a dense editor; rmcp official for MCP; ort for inference.

**Decision.**
1. A Rust workspace (spec v3 §9) with a custom wgpu toon renderer (the Rerun pattern) and an eframe/egui editor UI.
2. Every state change is a typed op in one registry. The registry generates the MCP tools (rmcp), CLI, script bindings, command palette and docs.

**Consequences.**
- UI and agents are peers, with no UI-only features.
- We own more rendering and UI code than with an engine, but avoid engine churn.
- egui polish is re-evaluated at the end of phase 1 (spec v3 open question 1).
- Versions are pinned and upgraded quarterly.
