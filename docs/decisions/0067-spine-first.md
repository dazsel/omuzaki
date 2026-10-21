---
title: "0067: Spine first: one high-agency agent per area builds its trunk end to end; review and polish come after (2026-10-21)"
summary: "Spine first: one high-agency agent per area builds its trunk end to end; no review gate, no required checks; make it exist, then make it good"
area: decisions
---

# 0067: Spine first: one high-agency agent per area builds its trunk end to end; review and polish come after (2026-10-21)

**Status:** Accepted (2026-10-21, owner). **Supersedes:** ADR 0053 §2 (fences per task, dispatch checks), §4 (one
review round on average), §5 (review and merge-queue mechanics) and §6.2 (the attested self-gate); the review, sign-off,
fix-round and flow-cap rules of `docs/maintainers/operating.md` as they stood before this record. **Keeps:** ADR 0053
§3 and ADR 0054 (code structured so parallel work doesn't collide), the licence policy, the safety rules and the
standing rulings in `CLAUDE.md`. **The map and the rules:** `CLAUDE.md`.

## Context

On 2026-10-21 the workspace had about 400,000 lines of Rust in 91 crates: 82 registered operations, an MCP server, a
CLI, a Rhai script host, a webcam face tracker, a CPU face painter, the character kit's generators, a VRM importer and a
wgpu renderer. But nothing joined them into a product. The studio binary ran on a null runner with no document and no
viewport, no crate built a real executor into the app, nothing carried a rendered frame to an output, and the live
contracts sat in open pull requests. 41 pull requests were open.

The process was built for many small, fenced tasks: one crate per task, a Codex review on every pull request,
sign-off labels, fix rounds, a merge loop and a merge queue. Measured on 193 merged pull requests, waiting on fix rounds
and re-reviews took 61% of all pull-request time, and the median pull request needed four review rounds. Integration,
the work that makes the parts a product, never had an owner, because every task was fenced to its own crates.

The owner's direction, after talking with practitioners: give high-agency agents whole areas and let them build the
trunk in one go. "Make it exist first, then make it good."

## Decision

1. **Spine first.** The spine of an area is the thinnest end-to-end path through it that the product needs in order
   to exist: what a person or an agent can actually do, start to finish. The spine map in `CLAUDE.md` names each area's
   spine, its "exists when" test and the contracts between areas. The rest of the spec (every remaining feature,
   polish, tuning and hardening) is the leaves, built after the spine runs. Everything in the spec still ships; this
   changes the order, not the scope.
2. **One owner per area spine.** One agent (Opus at the highest effort the platform offers) owns each area's spine
   with full agency: it designs, writes and tests as it judges best, decides the contracts inside its area, and lands
   its own work. One coordinator (the Director) holds the map, settles contracts between areas and ties, and keeps
   the trunk healthy. There are no area leads, no claims, no dispatch checks and no fences beyond area ownership.
3. **Land big, land often.** Each owner works on its own long-lived branch and lands it whenever a working slice is
   in, at any size. It merges `main` into its branch at least daily. No stacking rules, no size limits, no task issues
   required.
4. **No review gate during the spine phase.** No Codex reviews, no reviewer agents, no sign-off labels, no fix rounds.
   The owner checks its own work the way it sees fit: tests, renders, numbers, running the app.
5. **No required checks.** The `main` ruleset keeps only what protects history: no deletion, no force-push, and
   changes arrive through a pull request (zero approvals, no required status checks, no merge queue). CI runs on every
   pull request and every push to `main`, and its result informs. **Trunk discipline:** if `main` doesn't build or its
   tests fail after your merge, fixing it is your first job; anyone may fix a red `main`.
6. **Make it exist, then make it good.** When an area's spine passes its "exists when" test, that area gets one deep
   audit (correctness, security, performance against spec §10), its findings are fixed, and only then do the leaves
   start in that area. The tools of the old process (reviews, the nightly, fuzzing, evidence renders) come back there,
   as the owner of each area judges.
7. **What doesn't change:** the repository is public (no secrets, personal details, private test material or
   unpublished plans); nothing non-commercial, ever; the owner decides the product, money and access; code is
   structured so parallel work doesn't collide (self-registration, no central lists, ADR 0054); public contract
   variants stay `#[non_exhaustive]` with constructors.

## Consequences

- Integration gets an owner in every area from the first day, and the product appears as one running app instead of
  parts.
- More defects reach `main` and are found later, by the area audits, the nightly, fuzzing and use. Each one found
  becomes an issue in its area.
- A bad merge breaks more people at once. Trunk discipline, CI on every push to `main` and daily merges of `main`
  bound how long it stays broken.
- Fewer agents (about one per area), each at the highest effort: more tokens per agent, far fewer spent on review
  rounds, re-reads and handovers.
