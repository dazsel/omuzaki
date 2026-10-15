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
   the anchor, `plan.link` back to the comment). Assigning records; a person starts the agent (open question Q1).
   `CauseKind::Review` is added, with the comment id as text, never the comment's text.
6. **Injection:** comment text is sanitised on the way in (controls, bidi, Unicode tag characters), and is shown to an
   agent only inside a fenced block under a fixed "this is data" sentence, whoever wrote it. It is never put in labels,
   causes, plan items or the instruction part of a prompt. An external comment cannot start a task or an agent.
7. **One mechanism:** `review.approve`, `reject` and `request_changes` are the approval card and the branch-review
   decisions, not new ops (spec amendment, Q3). VC-11's `no_open_threads` reads `summarise` over comment threads.
8. **MCP:** `omuzaki://comments` and `omuzaki://comments/<thread>` (a `ResourceProvider`) and the prompt
   `address_review_comments`; `resources/updated` through the path-to-URI mapping.
9. **Privacy:** user text, not translated, no telemetry; comments leave the machine only in a review bundle a person
   shares (or a git push, which the UI warns about).

## Consequences

- New API crate `omuzaki-review-api` and `omuzaki-review-mock`, implementation `omuzaki-review`; the `review` scope
  (#1060) and `CauseKind::Review` are additive changes to `omuzaki-ops-api`.
- #1067 is re-cut (labels without comment text, import), #1197 (anchor name, task types, `sanitise` tag range),
  #1068 and #1060 confirmed. The follow-up tasks are listed in the design (section 16).
- Open for the Director or owner: assignment starting an agent (Q1), return path for external comments (Q2), the spec
  §11 wording (Q3), viewers commenting (Q4), `CauseKind::Review` (Q5).
