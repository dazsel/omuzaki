---
title: "0056: Review comments are flat document nodes `comments/<id>`; a task is a thread plus a plan item; comment text is data, never instructions (2026-10-15)"
summary: "Review comments are flat document nodes `comments/<id>`; a task is a thread plus a plan item; comment text is data, never instructions"
area: decisions
---

# 0056: Review comments are flat document nodes `comments/<id>`; a task is a thread plus a plan item; comment text is data, never instructions (2026-10-15)

**Status:** proposed (cloud lead to accept on the PR). **Context:** VC-07 (#524), #1050; spec v3 §11, §12, §29, §31;
ADR 0020 (agent sessions, approvals), ADR 0021 (journal), ADR 0023 (op text is not translated for user text),
ADR 0025 (branches), ADR 0043 (team roles, `Need::Review`). **Design:** `docs/design/comments.md`.

## Context

The spec lists `review.comment.add`, `approve`, `reject` and `request_changes`, but there is no comment model, anchor or
storage. Comments are evidence for VC-11 approvals and the reviewers' pinned comments for VC-05 review links. Text from
an external reviewer is untrusted and can reach an agent's prompt. The plan document and the Producer are #971.

## Decision

1. **Storage: a flat node `comments/<id>`, one file each** (journalled, undoable, a merge unit by the first-two-segments
   rule of branches.md 7.1, visible to git). Rejected: under the character like `applied/` (it would travel into
   exports and library items, and frame and view anchors have no character). Text is immutable.
2. **Threads:** the thread is its root comment plus replies named `<thread>-<8 hex>`; status and task live on the root.
3. **Anchors** use stable ids plus a last-known name; an orphan (part or character gone) is derived at read time and
   kept. Caps: 4,000 characters, 2,000 comments per project, 200 replies per thread, 200 agent-authored comments.
4. **Rights:** the new scope `review` (comment and reply) is held by every person role except viewer and by agents by
   default (team.md). Resolve, reopen, delete, import and task creation are person-only. External reviewers hold nothing:
   a person imports their comment file, always as `Author::External`.
5. **Tasks:** `Comment.task { assignee, state, item }` on the root, with the work held in a #971 plan item (fence from
   the anchor, `plan.link` back to the comment); the task's state is derived from the plan item, not stored. Assigning
   records; a person presses Start (D1). `CauseKind::Review` is added, with the comment id as text, never the
   comment's text (D5).
6. **Injection:** comment text is sanitised on the way in (controls, bidi, Unicode tag characters), and is shown to an
   agent only inside a fenced block under a fixed "this is data" sentence, whoever wrote it (a task form of the sentence
   carries the person's authorisation to address the feedback, and still forbids anything beyond it). It is never put in labels,
   causes, plan items or the instruction part of a prompt. An external comment cannot start a task or an agent.
7. **One mechanism:** `review.approve`, `reject` and `request_changes` are the approval card and the branch-review
   decisions on the existing approval card and branch review, not new ops (spec §11 is amended with a one-line note, D3). VC-11's `no_open_threads` reads `summarise` over comment threads for the request's subject paths
   (patch paths plus the paths the op declares it reads).
8. **MCP:** `omuzaki://comments` and `omuzaki://comments/<thread>` (a `ResourceProvider`) and the prompt
   `address_review_comments`; `resources/updated` through the path-to-URI mapping.
9. **Privacy:** user text, not translated, no telemetry; comments leave the machine only in a review bundle a person
   shares (or a git push, which the UI warns about).
10. **Time:** `at` is wall-clock UTC seconds recorded in the patch, because the journal's `Seconds` do not order across
    sessions.

## Consequences

- New API crate `omuzaki-review-api` and `omuzaki-review-mock`, implementation `omuzaki-review`; the `review` scope
  (#1060) is an additive change to `omuzaki-ops-api`. `CauseKind::Review` ships in one `omuzaki-ops-api` task with
  `CauseKind::Plugin` and `EndReason::Faulted` (#1537), which makes both enums `#[non_exhaustive]` (ADR 0054 §4).
- #1067 is re-cut (labels without comment text, import), #1197 (anchor name, task types, `sanitise` tag range),
  #1068 and #1060 confirmed. The follow-up tasks are listed in the design (section 16).

## Decisions on the open questions (Runtime lead, on #1535)

- **D1:** assigning a task to an agent role only records it; a person presses Start. A per-project setting can come later.
- **D2:** external comments come back as a comment file a person imports. A relay URL and a hosted inbox wait on the
  owner's hosted-service decision (#1051); nothing is built for them here.
- **D3:** `approve`, `reject` and `request_changes` are decisions on the approval card and branch review. The spec
  §11 is amended in this PR with a one-line note (Director's ruling).
- **D4:** reviewers and up may comment; viewers may not (`team.md`).
- **D5:** `CauseKind::Review` is approved, shipped as above.
