---
title: "0003: Heavy assets live outside git (2026-10-10)"
summary: "Heavy assets live outside git"
area: decisions
---

# 0003: Heavy assets live outside git (2026-10-10)

**Decision.** Images, models, `.blend` files, renders, audio, video and datasets live in `$OMUZAKI_ASSETS/`. Git holds text and code only.

**Consequences.** The repo stays small and fast, and notes reference assets by path. If assets ever need versioning, use a content-addressed store, never git.
