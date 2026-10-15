---
title: "0053: How the work runs: finish before starting, schedule for disjoint work, structure code so parallel PRs can't collide (2026-10-13)"
summary: "How the work runs: finish before starting, schedule for disjoint work, structure code so parallel PRs can't collide"
area: decisions
---

# 0053: How the work runs: finish before starting, schedule for disjoint work, structure code so parallel PRs can't collide (2026-10-13)

**Status:** accepted (2026-10-14). **Context:** spec v3 §39 (parallel development), `CONTRIBUTING.md`, `CLAUDE.md`,
ADR 0054 (how features are added without editing shared files).

## Context

Omuzaki is built by many AI agents in parallel, in two lanes (a local lane and a cloud lane), directed by a Director
session. On 2026-10-13 the project merged **172 pull requests**, with a median of 2 hours from open to merge. It also
**opened 95** and ended the day with **98 open** (median age 8 hours). Intake outran merging, and the cost showed up
everywhere:

- **Reviews:** 598 automated reviews, of which 184 were first reviews and 414 re-checks. 60 % of all verdicts asked for
  changes. 43 re-reviews happened only because a rebase changed the patch of an already approved pull request.
- **Merging:** the merge queue ran on one machine and rebased each pull request onto a moving `main`. `main` moved under
  a run 53 times, and each time meant another rebase and another CI run. There were 17 rebase bounces, and 6 pull
  requests looped; one re-gated 4 times.
- **Compute:** gates waited up to about an hour for one of four local build slots.
- **Conflicts** came from the same few places:
  - central lists every feature appends to: bench entries, the render conformance checks, `mod` lists, part-kind
    registration, example flags;
  - the spec's feature table;
  - a test that asserted which sections were *not* built yet (`omuzaki-face/tests/system.rs`, three bounces);
  - shared structs several features extend (`ProgramIn`, shader hooks);
  - design-doc paragraphs owned by no one.
- **Re-gates from overlap** came from dependency reach: `omuzaki-test` reaches every crate, then `omuzaki-char` and
  `omuzaki-io-ops`.
- **Review findings:** about a third of the local lane's blocking findings were fence or scope, and a quarter were
  contract mismatches. Those are planning defects, found at review time instead of at dispatch.

Spec §39.1 already states most of the structural rules (no central registries, vertical slices, generated artefacts not
committed). The failures were where they were not applied or not enforced. There was no rule at all for how much work
may be in flight.

## Decision

### 1. Flow: finish before starting

1. **Limits on work in progress:**
   - at most **40 open pull requests** in total, about four hours of the merge rate;
   - at most **3 open pull requests that change the same crate**;
   - each lane has its own cap, set on the coordination issue (#2) from the measured merge rate.
2. **Pull, not push.** A task starts only when the crates in its fence are under the per-crate limit. Whoever dispatches
   (the Director for the local lane, the cloud lead for the cloud lane) checks first, and tooling enforces it.
3. **Freeze trigger.** Intake freezes in both lanes, announced on #2, when any of these holds:
   - the open-PR cap is passed;
   - CI waits more than 30 minutes for a runner;
   - opens per hour exceed merges per hour for two check-ins.

   While frozen, agents only drain: fix rounds, updates from `main`, and closing obsolete pull requests with a reason.
   The exceptions are critical-path items the Director names and fixes for bugs on `main` that block merging.
4. **Order of agent time:** unblock merges (bounces, rulings); fix rounds on open pull requests; the critical path; new
   tasks.

### 2. Schedule for disjoint work: decide conflicts at dispatch, not at merge

1. Every task issue names its fence (crates and folders). Two tasks whose fences share a module are **chained**
   (blocked by), not run in parallel. The dispatch check refuses a task whose fence touches the changed files of an open
   pull request unless they are chained.
2. **Waves by crate.** A wave puts at most one task on each hot module of a crate. Contract changes (`*-api`) land
   **first and alone**, and their implementations start after the contract has merged.
3. **No stacks**, with one exception: a single level, when an implementation needs a contract that is already approved
   and queued. Nothing is ever stacked on a branch that is still in review.

### 3. Structure code so parallel pull requests can't collide (§39.1 enforced, plus new rules)

1. **No list that every feature appends to.** Conformance checks, bench targets, part kinds, example flags and module
   lists become self-registration or one file per item that the build discovers (ADR 0054). Text merge drivers such as
   `merge=union` are not relied on: GitHub's server-side merges and the merge queue don't run them.
2. **The spec's feature rows live one file per feature** (`docs/product/features/<ID>.md`), and the table is generated.
   Pull requests never edit the big table.
3. **A test asserts its own feature, never the absence of another.** A test like "the stubs draw nothing" breaks for
   every new feature.
4. **Extension points are contract.** A shared struct or hook gains fields only in its contract owner's pull request,
   which reserves what the wave needs. Implementations then only add files.
5. **Narrow dependency reach.** Split `omuzaki-test` by area, and keep dev-dependencies minimal, so a merge in one area
   doesn't re-test every other.
6. **One file per design topic.** A shared overview paragraph has one owner, and features link to it instead of editing
   it.

### 4. Right first time: at most one review round on average

1. **Fences come from reading the code.** The task names the neighbouring files a change will probably need. An agent
   that has to cross a fence asks on #2 *before* opening the pull request and gets a one-line ruling. Review is not where
   fences get discovered.
2. **Self-review before the pull request:** the implementer checks its own diff against the review checklist. Findings
   reviews keep raising become lints or a pre-PR script, for example: `Default` that bypasses `new()`, unseeded property
   tests, unchecked indexing in public helpers, GPU writes outside an error scope, missing allocation tests.
3. **The task states the acceptance numbers and the evidence**, and the pull request proves them. Reviews check against
   them, so taste isn't reopened on every round.

### 5. Review and merge mechanics

1. **GitHub's merge queue merges.** It tests the exact commit that will land on `main`, so pull requests are never
   rebased to merge. One required check, `gate`, aggregates every CI job.
2. **An approval survives a clean update.** If a pull request's own change is unchanged after a clean merge of `main` or
   a pure rebase (same patch), the approval stands: no re-review.
3. **One fix-round trigger for both lanes, checked by tooling:** a fix round ends with a comment that starts
   `fixed (head <sha7>)`.
4. Only the Director adds pull requests to the merge queue; the review automation does it for approved, green ones.

### 6. Compute placement

1. **CI runs on GitHub-hosted runners**, for every lane. It is the gate.
2. **Agents self-gate where they can build** (`cargo xtask check --affected`) before marking a pull request ready.
3. **The maintainers' local machine reviews and directs.** It builds only what needs local hardware: renders for visual
   review on a real GPU, and private test material that never leaves it.

### 7. Measure every check-in

1. **The tracker reports:** open pull requests by lane and by crate; merges and opens per hour; median open age; the CI
   wait; review-queue depth; the ratio of re-checks to first reviews; bounces.
2. The thresholds in 1.3 trigger the freeze. The Director's status post leads with these numbers.

## Consequences

- **Throughput:** each agent waits more, but more work actually merges, and far fewer tokens go on re-reviews and
  re-runs.
- **This decision's own tasks land first, alone:** approval carry across a clean update in the review automation; a
  per-crate and total cap check at dispatch; flow metrics in the tracker; a pre-PR self-review script with the recurring
  checks.
- **The structural changes (ADR 0054) come after the open work has drained,** because each touches many crates and
  would conflict with work in flight.
- `CONTRIBUTING.md` and `CLAUDE.md` follow these rules.
