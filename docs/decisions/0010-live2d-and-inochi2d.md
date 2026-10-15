---
title: "0010: Live2D Cubism is not used; flat 2D is first-class through Inochi2D and the cel-flat style (2026-10-11)"
summary: "Live2D Cubism is not used; flat 2D is first-class through Inochi2D and the cel-flat style"
area: decisions
---

# 0010: Live2D Cubism is not used; flat 2D is first-class through Inochi2D and the cel-flat style (2026-10-11)

**Context.**
- An earlier spec revision said "keep Live2D import and runtime compatibility". Spec v3 dropped it without a record and left open question 2 (is flat 2D first-class?) open. The project decision of 2026-10-11 is that nothing is cut silently and every feature ships, so the question needs a recorded answer.
- Running a Live2D model needs Cubism Core, which comes only in Live2D's SDK package. Primary sources, read 2026-10-11:
  - Cubism SDK for Native manual (`docs.live2d.com/en/cubism-sdk-manual/cubism-sdk-for-native/`): Core is included in the SDK package and, "in accordance with the Live2D Proprietary Software License Agreement, Cubism Core is not published on GitHub".
  - Live2D Proprietary Software License Agreement (`live2d.com/eula/live2d-proprietary-software-license-agreement_en.html`; the English text is a translation and the Japanese text prevails, §15.2):
    - the licence is non-sublicensable and non-transferable; modification and reverse engineering are prohibited (§6)
    - redistribution is limited to the "Redistributable Code" named in the SDK, only to make our own work available to end users, with equivalent protection terms passed on to distributors and users and an indemnity to Live2D (§5)
    - publishing a work needs a Publication License Agreement unless the publisher is exempt (general users, educational institutions, small-scale enterprises under ¥10M of sales), and "the exemption does not cover Expandable Applications" (§2.1, §2.2)
    - "non-commercial" excludes anything done by or for a company (§1.7), and §6.8 limits the fields of use
  - SDK licence page (`live2d.com/en/sdk/license/`): VTuber live-streaming and tracking software is named as an Expandable Application. It "needs to be reviewed for approval and enter into a special Publication License Agreement" at least one month before release; the page cites ¥20M of annual sales as the trigger for VTuber tracking software. The plans are a running royalty, a one-time purchase or a non-profit plan. No fee figures are published.
- Inochi2D is an open 2D puppet format. The core repository and inox2d are BSD-2-Clause (checked 2026-10-11). inox2d calls itself a prototype and "the newer models which use the MeshGroup feature don't work yet".

**Decision.**
1. **Live2D Cubism (Core, the SDKs and `.moc3` runtime import) is not used.** It fails `docs/licensing/policy.md`:
   - The policy admits a proprietary runtime only "with a redistribution grant" whose conditions are recorded (NDI, CUDA, DirectML, the NVIDIA Open Model License). For a VTuber app the grant does not exist until Live2D approves a special agreement, and that agreement's terms (royalty or purchase plan, thresholds, approval criteria) are not public. A licence we cannot read is `unverified`, and `unverified` is treated as `NO`.
   - Even a signed agreement would make shipping depend on a third party's approval. That is for the maintainers, not for an engineering default.
   - Redistributing Core would also pass Live2D's protection terms and an indemnity on to every distributor and user (§5), which a free, open-format product should not ask of its users.
   - The register gets a row for it (`unverified`, with the sources above).
2. **Flat 2D is first-class without it** (closes open question 2):
   - **3D characters can look flat.** The cel-flat preset (LK-06) is the flat look for any 3D character.
   - **Inochi2D import at full scope (EX-08, now P1, phase 3).** `.inp`/`.inx` puppets with meshes, deformers, parameter bindings, masks, blend modes and physics, driven by our shared channels through an editable ARKit-52 mapping preset, drawn by `omuzaki-render` on the stage next to 3D characters and reaching every output. BSD-2 code only; where inox2d falls short (MeshGroup) we implement it ourselves. Before building, read the format documentation (`tech-docs/` in the Inochi2D repository) and record its licence in the register.
   - Layered PSD import (`omuzaki-io`) stays the way into 2D art.
3. **Honest gap.** A creator who has only a Live2D model cannot bring it in. EX-12 (bring your own model) covers VRM and glTF, EX-08 covers Inochi2D, and the creator can have a puppet rigged in Inochi2D's tools. The maintainers can open a conversation with Live2D at any time; that needs legal sign-off, so it is a maintainer decision.

**Consequences.**
- Spec: the §3 related-tools table, the §36 licensing risk, §38 Q2, EX-08 and LK-06 say so.
- If Live2D later grants a written Publication License Agreement for Omuzaki, and legal review clears the terms, a new ADR supersedes this one and adds a `.moc3` import row. Until then no Cubism file, library or SDK enters the repo, the build or a model.
- Licence text can change; re-read the three sources above before reopening this decision.
