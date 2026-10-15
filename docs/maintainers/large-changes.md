---
title: "Large changes: how work is cut"
summary: "How leads cut their areas into large changes: one agent owns a whole subsystem or feature with one context and opens one PR, with one deep review."
area: maintainers
---

# Large changes: how work is cut

The Director's rule since 2026-10-15, at the owner's direction: **bias towards larger changes.** One agent owns a
whole subsystem or feature, keeps its context from design to merge, and opens one pull request. It gets less review,
but deeper.

## Why

Measured on the 193 PRs merged between 2026-10-14 12:00 and 2026-10-15 20:40 UTC:

- **Review per 1,000 changed lines fell twelvefold with size:** 33.7 review rounds below 200 lines, 10.5 at 200–600,
  6.0 at 600–1,500 and 2.8 above 1,500.
- **The median PR needed 4 review rounds.** Waiting on fix rounds and re-reviews took 61% of all PR time.
- **Seams between small PRs made their own findings:** stacked bases moving under each other, a fix that needed a file
  fenced to another PR, and every new session re-reading the same crate.

A large change pays the fixed costs once: reading the crate, the gate, CI, the review and the sign-off.

## What a large change is

- **One coherent outcome:** a subsystem behind its contract, or a feature end to end within one area, together with
  its tests, docs and evidence. For example, "settings, end to end" rather than an API task, a mock task, a store task
  and a keys task.
- **One owner session from start to merge.** That's what keeps the context and makes the review deep instead of
  repeated.
- **Cut by cohesion, never by line count.** A large change is usually one crate, a crate with its `-api` and `-mock`,
  or one module family in `omuzaki-char` or `omuzaki-face`. It's commonly a few thousand lines. It's too big only when
  one agent can't hold its design; then cut it along a contract, not by size.
- **It closes every issue it covers.** The old task issues become its checklist, and its PR says `Closes #a, #b, …`.
  Chains (S1…S6, A1…A6, task 3a/3b) collapse into it.

## How a lead cuts its area

1. **Start from the digest:** `area-<slug>.md` on the `ops/snapshot` branch
   (`https://raw.githubusercontent.com/dazsel/omuzaki/refs/heads/ops/snapshot/area-<slug>.md`). It lists every open
   issue in one line, grouped by crate, with its status, milestone and blockers. Reading it costs no API budget.
2. **Group by crate or module and by outcome.** A `Blocked by` between two issues in the same group disappears: they
   become one change.
3. **Cut only where one of these is true:**
   - a contract that another area calls must land first (that contract change is its own change, and small is fine);
   - two groups touch disjoint files and are worth running in parallel;
   - the group is more than one agent can hold.
4. **Order the changes:** V0 first, then the changes that unblock the most others, then the rest.
5. **File one issue per change:**
   - **Title:** `Change: <outcome>`.
   - **Labels:** `change`, the area, the priority, and the milestone of the most urgent issue it covers.
   - **Body sections:**
     - **Outcome:** what works when it merges, in two or three lines.
     - **Covers:** a checklist of the issues it closes.
     - **Crates:** what it may touch. Two changes in flight never share a file, apart from line additions to a
       `Cargo.toml`.
     - **Contracts:** what it uses, and what it changes. A change to another area's contract needs that lead's co-sign.
     - **Prior art to beat:** the best open-source implementation, with its licence.
     - **Acceptance:** merged from the covered issues, without duplicates.
     - **Evidence:** what the PR shows.
     - **Model:** Sonnet by default; Opus for a subsystem that needs real design. Not Haiku: large changes need care.
     - **Depends on:** the changes it waits for.
6. **Keep the area's change map on your state issue,** in order, with each change's state. That's the area's plan;
   edit it in place.

Open PRs aren't re-cut. They finish under the review rule (one more round at most, then merge with follow-ups), and
the change that follows absorbs what's left.

## Running a change

- **The session starts with a short design note** at the top of its draft PR: the modules, the types, the order of
  work. Then it builds, committing and pushing often so nothing is lost. It runs the gate and self-reviews the whole
  diff before it marks the PR ready.
- **Review:** one deep review at open, with every finding in one pass, then one verification after a single fix round
  (`operating.md`). After that, only a blocking correctness or security bug that the lead confirms gets another push.
  Everything else becomes a follow-up issue.
- **Concurrency:** at most one change in flight per crate, or per top-level module where a crate's module map was
  built for parallel work. The owning lead decides which of its crates qualify; `omuzaki-char`, `omuzaki-face` and
  `omuzaki-stage` do. File overlap stays absolute. Files at the crate root and `lib.rs` are shared, so only one
  change at a time holds them.
- **A change that spans two areas** has one owning lead, named in its issue. The other lead co-signs the part in its
  crates.

## Don't

- Split for size, stack branches, or open a PR per task issue.
- Start a new session per fix round. The owner session does every round of its own change.
