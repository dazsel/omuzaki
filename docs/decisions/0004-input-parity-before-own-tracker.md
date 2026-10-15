---
title: "0004: Match existing input fidelity before building our own tracker (2026-10-11)"
summary: "Match existing input fidelity before building our own tracker"
area: decisions
---

# 0004: Match existing input fidelity before building our own tracker (2026-10-11)

> **Amended 2026-10-11 (no later tier).** Item 2 is superseded: the custom tracker is not parked. Parity still ships first; the tracker then ships as ML-08 and becomes the default webcam model once it beats the MediaPipe baseline on the iPhone-labelled test set (QG-09).

**Context.**
- A human-driven test showed the face engine carries a real performance; the gap with the test clip was the driver.
- High-fidelity VTubers today use an iPhone (ARKit, 52 scores incl. tongue) sent to the PC; webcam (MediaPipe-class) is the free tier.
- No commercially usable open model beats MediaPipe.

**Decision.**
1. Ship parity first: iPhone input (iFacialMocap / VTube Studio / Live Link Face protocols via a local bridge), webcam (MediaPipe), VMC in and out.
2. ~~Park the custom tracker.~~ Build it after parity (ML-08, spec §16). Its research, plan and synthetic pipeline are kept.

**Consequences.** Full existing fidelity in days, not months. Our own tracker comes back as a webcam-tier differentiator once the core product exists.
