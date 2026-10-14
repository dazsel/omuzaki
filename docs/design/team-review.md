# Teams: review policy (VC-11) and the audit trail

**Status:** proposed (cloud lead to accept on the PR). **Features:** VC-11, VC-10's audit
log. **Decision:** ADR 0043. This page continues `docs/design/team.md` (roles, membership, enforcement) and
keeps its section numbers, so "team-review §8.3" and "team §6.3" never collide.

## 8. Review policy (VC-11)

### 8.1 What queues as `Need::Review`

`ReviewPolicy { publish: Requirement, broadcast: Requirement, library: LibraryPolicy, no_open_threads: bool }`,
`Requirement { None, Reviewer { approvals: u8 } }`, `LibraryPolicy { approvals: u8, apply_only_approved: bool }`
(the team library's rules, which this design uses).

- **`no_open_threads`:** while it is set, an approval is refused (`NotEligible`, "2 comment threads are still open")
  until the threads anchored to the request are resolved; the count comes from `omuzaki_review_api::summarise`.

- **`publish`:** every op whose scope is `publish`, by an agent or a person, queues `Need::Review` after its dry-run.
- **`broadcast`:** granting `broadcast` to an agent session (the `Scope` approval's "for session", or the grants
  popover), and a person's first `broadcast` op in a studio run; the approval then covers that session or that run,
  because a review per scene switch mid-show would be unusable.
- **The licence gate runs first:** the dry-run (agent-sessions §7.1 step 1) runs VF-08; a request that fails it is
  refused and never reaches a reviewer.
- **Needs come one at a time:** an agent lacking `publish` first gets the `Scope` approval from its person; the
  approved run then hits `Review`.

### 8.2 People's own requests

A new field, `PendingApproval.own: bool` (serde default `false`), marks a person's own request: `actor` is that
person and `session` is `SessionId::NONE` (a documented constant, `SessionId(0)`), which the executor never allocates
(the executor's ids start at 1, and a test asserts it). That keeps the change
additive; making `session` an `Option` was rejected because it breaks every reader, and a "person session" because
persons have no session by design (agent-sessions §3.3) and one would reach every person code path.
A person's pending review is withdrawn by that person or when the project closes; it never times out.

### 8.3 Who approves, and how many

- **Eligible:** a person whose key holds reviewer, admin or owner, **never the requester** (the principal: an agent's
  person counts as the requester), **never an agent** (approvals are resolved in Omuzaki's window, agent-sessions §7.2).
- **N distinct approvers:** counted by the full key, never a fingerprint, so one key twice counts once. `approvals`
  must be at least 1 and at most eligible minus one, so any eligible requester can still be approved;
  `team.set_policy` refuses other values with the counts. Removing a reviewer that would break that rule is refused
  the same way.
- **One rejection ends it**, with its comment reaching the requester; later approvals are not asked for.
- **Role changes in between:** an approval counts only if its key is eligible when the last approval arrives, and the
  run re-checks the requester's grants (agent-sessions §7.1 step 3).
- **Approvals bind to what was reviewed.** Every approval signs the request hash (8.5), which includes the dry-run's
  `preview` digest: the canonical hash of the plan (patch, outputs, cost) and, for a `publish` op, of the bytes it
  would send out. All N approvals must sign the same request hash. When the last one arrives, the executor re-plans
  against the current document (agent-sessions §7.1 step 3); if the new digest differs, it runs nothing, discards the
  approvals (each recorded as withdrawn, "the content changed after review") and queues a fresh `Review` with the new
  preview. Content changed between two approvals is therefore never published under either.
- The applied op's `OpRecord.approval` names the last approver in `by` and the others in `also`.

### 8.4 The approval card's evidence

The requester and, for an agent, its session and cause; the op and the dry-run preview (patch, outputs, cost); the
licence gate's result; **unresolved comment threads** anchored to anything the request touches (the comments design); **imported review feedback** from review links (VC-05, the review-links design); "1 of 2 approvals" with the
names so far; Approve and Reject with a comment. The requester sees the card with Approve disabled and the reason.

### 8.5 A reviewer on another machine

1. The requester exports a `ReviewRequest { team, id: RequestId, op, args, preview: ContentHash, requester: KeyId }`
   file from the card, or it lands in the team's shared storage (the shared-library-sources design, planned). `RequestId { session: String, lsn:
   u64 }` holds the journal session id exactly as `HistoryEntry::Session.session` writes it (a string, kept losslessly)
   and the LSN of the request's `ApprovalRequested` audit record. The LSN alone is unique: the journal's LSN continues
   across sessions and is never reused (`omuzaki-history-api`), so the id is unique per project and survives restarts,
   unlike `ApprovalId` (one executor's lifetime) or `DocVersion` (resets on open); the session string is carried for
   readability and audit joins. An unchanged request queued again after a restart gets a new id, so its hash differs
   and no earlier statement fits.
2. The reviewer opens it in their studio, and their client re-plans the request against its own copy. **Approve is
   enabled only when that re-planned digest equals the request's `preview` digest**, so a reviewer can never sign
   content A while looking at content B. On a mismatch the card shows "your copy differs from what was requested
   (sync the project, then review)", and only Reject (with a comment) is offered. The client writes a
   `Signed<ApprovalStatement>` with `ApprovalStatement { request: ContentHash, decision, comment }`
   (`Domain::Approval`).
3. A person on the requester's side runs `team.import_approval`. It checks the signature, that the key is eligible in
   the current roster and isn't the requester's, and that the request hash matches a pending one; then it resolves
   with `Decision::ApproveSigned { statement }` (or rejects). A statement already stored under `team/approvals/` is
   refused as a replay. Agents can't import: they could feed in statements.

Local approvals are signed the same way, so every review approval is an `ApprovalStatement`, stored with the
approved op's transaction under `team/approvals/<request hash>` and therefore shared, verifiable and part of history.

## 9. The audit trail

### 9.1 The facts and where they live

| Fact | Lives in | Seen by |
|---|---|---|
| role and membership changes, revocations (with reason), policy and group-map changes | the signed log (`team/log/`) | every member's client, so the record is the team's, not one machine's |
| approvals that ran (each `ApprovalStatement`) | `team/approvals/` | the team |
| every applied `publish`-scope op, reviewed or not: `Published { op, by, on_behalf_of, approval, outputs }` | the journal's `audit` record (kind 11), written with the op record under the lock | this machine |
| session open and end, grant and budget changes, pause and resume, approvals requested, rejected and withdrawn | the journal's new `audit` record (kind 11, skippable), written by the executor under its lock | this machine |

The journal is per machine (`.omuzaki/`), so session facts are honest about scope: they record what agents did here.
Anything a teammate must be able to check is signed and in the document.

### 9.2 Retention

The op-log archive is pruned after 90 days by default (journal.md §7.3). Before the pruner deletes a segment that
holds `audit` records, it copies them to `.omuzaki/audit/` (same framing) together with each journal session's kind 1
`session` record that they belong to, never pruned automatically. Audit times are session-relative `Seconds`, and the `session` record's `started` is what turns them back into dates (`started + at`), so `since`,
sorting and export keep working after pruning. Because every publish also writes a `Published` fact (9.1), no
publish, reviewed or not, leaves the audit trail when its op records are pruned. An owner may prune `.omuzaki/audit/`
only through a signed export (9.3); pruning is itself an audit fact. Default: keep everything; at
about 300 bytes a fact, a busy year is a few MB.

### 9.3 Reading and exporting

- `team.audit {since?, kinds?, actor?, limit, after?}` merges the three sources newest first: `AuditRow { at,
  kind, by, on_behalf_of, subject, detail, version }`, `AuditKind { Role, Invite, KeyRevoked, Policy, Grant,
  SessionOpened, SessionEnded, Approval, Publish }`. Owners, admins and their agents only; each member sees their own
  approvals on their cards.
- `team.export_audit {to}` writes JSON Lines plus a manifest with the rows' hash, signed by the exporting person
  (`Domain::AuditExport`, `omuzaki-audit-export-v1\0`, so the signature can't be replayed as a platform export).
