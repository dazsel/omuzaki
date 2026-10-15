---
title: "0002: Characters are drawn in code, on a VRM skeleton (2026-10-11)"
summary: "Characters are drawn in code, on a VRM skeleton"
area: decisions
---

# 0002: Characters are drawn in code, on a VRM skeleton (2026-10-11)

**Context.** Generated meshes never matched the sheets and carried no usable expressions (journals 04–05). A code-drawn character (parametric parts plus a painted canvas face) matched the sheet and could be edited part by part.

**Decision.** The character is a set of parametric parts (body, head, face, hair, outfit), skinned to a standard VRM skeleton. The face is painted procedurally from named parameters.

**Consequences.**
- Every character gets the full 52-control expression set automatically. No hand-sculpted blendshapes.
- Parts are swappable and agent-editable.
- VRM export needs a bake step (parts → meshes; face → texture atlas plus blendshape or flipbook).
