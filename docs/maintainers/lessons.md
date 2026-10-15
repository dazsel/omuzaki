---
title: "Operating lessons"
summary: "Real problems from running the project: what happened, why, and the rule each one led to."
area: maintainers
---

# Operating lessons

This file records what went wrong while running the project, why it happened, and the rule that keeps it from happening
again. It's for everyone who runs the work: the Director, the area leads, implementer sessions and whoever maintains the
automation (the merge loop, the stall audit, the watchers). The rules themselves live in `operating.md`; this file keeps
the reasons, so nobody has to relearn them.

**Add an entry** when a problem costs real time or lets a defect through. Write down what happened, the cause and the rule.
Leave out slips that cost nothing.

## Before acting: four questions

1. Is this the right tool for the job, or just the process that happens to exist?
2. What evidence would prove me wrong, and have I looked at it?
3. What isn't being watched? Silent stalls, half-finished changes, my own queue.
4. Am I claiming more than I checked?

## Reviews

**Re-reviews find most of the real P1s, so keep them, but once per fix round** (2026-10-15).
- **What happened:** we proposed reviewing each pull request only once, when it opens. Then we measured 105 pull
  requests over 21 hours. Re-reviews found 85 P1 findings against 35 from first reviews, and 62 of the 89 P1s that were
  fixed or followed up came from re-reviews. None were dismissed as wrong. They clustered in large design and tooling
  pull requests, where the light reviewer reads a different slice on each pass.
- **The rule:** Codex reviews when a pull request opens, then once per fix round. A round is triggered by the session's
  single `fixed (head <sha>)` comment, or by new non-merge commits after 10 quiet minutes. Merges of `main` and
  rebases don't trigger a review. Large pull requests (over 400 lines, design docs or ADRs, `xtask/`, `.github/`,
  `tools/`) also get one deep local review, and merging waits for it for up to 60 minutes.
- **The lesson:** before removing a quality check, measure what it catches.

**Match the review to the change** (2026-10-15).
- **What happened:** the Director's own process-doc pull requests went through Codex rounds: eight findings and three
  hours spent on wording the Director decides anyway. A few findings were useful, such as one showing that a
  maintainer's ruling could have waived a security finding. Most were wording.
- **The rule:** the Director's process-doc pull requests (`AGENTS.md`, `CLAUDE.md`, `CONTRIBUTING.md`,
  `docs/maintainers/`, `docs/README.md`) carry the label `director-docs` and merge on a green gate without a Codex
  review. The Director checks safety wording personally. ADRs and design docs are still reviewed, because they shape
  code.

**State your assumptions in sign-offs** (2026-10-15).
- **What happened:** a sign-off said a lazily built default "can't deadlock", because a `Default` can't reach the view
  it's built for. A view stored in a `static OnceLock` can. Codex found the hang, which the code on `main` had already
  avoided by building the default outside its lock.
- **The rule:** write "assuming X" rather than "X can't happen". Before approving, check the assumption against the
  code on `main` that the change replaces.

## Automation

**Check exit codes, not just output** (2026-10-15).
- **What happened:** conflict checks ran `git merge-tree --write-tree`, which this git (2.34) doesn't have. It printed
  its usage to stderr, the script read the empty output as "no conflict", and every pair looked clean.
- **The rule:** a check that fails must fail loudly, so run its negative case once on the real machine. For conflict
  checks on older git, use `git merge-tree <base> <a> <b>` or a throwaway merge.

**In the merge queue, UNMERGEABLE doesn't only mean a conflict** (2026-10-15).
- **What happened:** a pull request's CI run in the merge queue failed while downloading a pinned action, during a
  GitHub outage. The merge loop took the entry's UNMERGEABLE state for a conflict. It dequeued the pull request,
  labelled it `needs-rebase`, and wouldn't queue it again until something was pushed. That parked a clean P0 pull
  request.
- **The rule:** read the failed run's log before labelling anything. The loop now dequeues only after two passes in a
  row, adds `needs-rebase` only when the pull request conflicts with `main`, and retries once at the same head after
  `main` moves.

**Deploy an automation change whole, then watch its first live pass** (2026-10-15).
- **What happened:** a change to the review rule went live in pieces. The classifier changed before the loop did, so
  for a while a pushed pull request could count as clean on an old review. The next version passed its self-tests but
  read words from the classifier's output as review-thread IDs.
- **The rule:** change, self-test, restart everything that shares the change at the same time, and read the first live
  pass of the log before calling it done. Self-tests don't cover how the pieces fit together.

## Sessions

**A blocked session is silent** (2026-10-15).
- **What happened:** an implementer session called an interactive messaging tool, which waits on a permission prompt
  nobody can answer. A P1 follow-up never got a pull request, and nothing showed it for about an hour.
- **The rule:** implementer sessions never use interactive messaging. Leads check their sub-sessions for pending prompts
  every round. The stall audit flags a milestone issue that has no pull request after 30 minutes.

**One session per issue** (2026-10-15).
- **What happened:** two leads each started a session on the same issue within minutes of each other.
- **The rule:** the issue's area owns it. Before starting a session, check the issue for a linked branch or pull request.
  Leads archive each sub-session in the same step that reports its pull request merged or closed.

## Structure

**A list every feature edits becomes a hub** (2026-10-15).
- **What happened:** within a few hours, four pull requests edited `xtask/src/main.rs`, `xtask/src/check.rs` and
  `.github/workflows/ci.yml`, and two of them then had to merge `main` again to resolve conflicts.
- **The rule:** a hand-written list that every new feature edits is a hub. Make it self-registering (ADR 0054). For the
  `xtask` commands, that's #1641.

## Director duties that slipped

**Spot-check routine pull requests** (2026-10-15).
- **What happened:** on a busy day only Codex and the area leads checked feature code. The Director reviewed hub, API
  and architecture pull requests and sampled none of the rest.
- **The rule:** every day, sample about 1 in 10 merged feature pull requests. Check whether Codex's findings were really
  fixed and whether the lead's sign-off caught what it should. Post the results in the status issue.
