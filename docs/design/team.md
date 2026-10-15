---
title: "Teams: roles, membership, review policy and the audit trail"
summary: "team roles over the scopes, the signed membership log, enforcement, agency accounts and the SSO, LB-07 and git hooks (ADR 0043)."
area: design
---

# Teams: roles, membership, review policy and the audit trail

**Status:** proposed (cloud lead to accept on the PR). **Features:** VC-10, VC-11,
VC-12 hook, LB-07 hook. **Decision:** ADR 0043.
**Spec:** v3 §6 (scopes), §12, §28 (LB-07), §29 (VC-10 to VC-12), §37.
**Builds on:** `docs/design/agent-sessions.md` (ADR 0020: `Principals`, the approval queue, `Need::Review`,
`OpRecord.on_behalf_of` and `approval`), `docs/design/journal.md` (ADR 0021), `docs/design/library.md` (D11),
the identity outline (`KeyId`, `Signer`, `TrustStore`, `Domain`; ADR 0042, in flight).
**Seams with designs in flight:** identity, comments, shared library sources; the cloud
lead's accepted answers are listed in 11.3, and the other side decides its own half.

Team roles are not AG-03 agent roles. This page says **role** for a team role (`omuzaki_team_api::Role`) and
**agent role** for an AG-03 role file. They never share a type or a name.

## 1. What this settles

| Question | Answer in one line | Section |
|---|---|---|
| Where do roles apply? | Per project (`team/` in the tree), per shared library source, per agency client (a registry in the user's config); no `team/` means solo, as today | 2 |
| What does each role hold? | A fixed table over every scope in `Scope::ALL`; never wider than `Grants::person()` | 3 |
| What scope do `team.*` ops carry? | `read`, plus `person_only` and a role check inside the op; agents read the roster, never change it | 4 |
| How is a person a key? | `PersonDirectory` maps the local `Actor::Person` to the machine's identity key; `Actor` is unchanged | 5 |
| How is the roster trusted? | An append-only log of signed membership statements rooted in the owner's key; a bad entry fails closed to read-only | 6 |
| What is enforced, really? | Agents and the executor locally; signatures everywhere they travel; servers by verifying; never a person with the folder | 7 |
| What needs review (VC-11)? | `publish` ops and `broadcast` grants under a policy, for people and agents; N distinct reviewers, never the requester | team-review 8 |
| Where is the audit log? | Membership in the signed log (shared); sessions and approvals in the journal's `audit` record (per machine); a signed export | team-review 9 |
| Agencies? | A per-user registry of clients, one key, library set and credential set each; switching swaps all of them; nothing crosses | 10 |
| SSO, LB-07, hosted services? | A group map in the roster (admin and below); library roles are these roles; the client enforces for free | 11 to 13 |

## 2. Where roles apply

- **Per project.** The roster lives in the project document under `team/` (6.2), so it travels with the project
  (folder sync, git, backups), is journalled, and branches and merges like any other unit. One roster per project.
  Characters, scenes and libraries inside a project have no rosters of their own: one place to look, one thing to sign.
- **No roster means solo.** A project with no `team/` node behaves exactly as today: the local person holds
  `Grants::person()` and is the implicit owner. Creating a team (`team.create`) is how a project opts in.
- **Per library source.** A shared library source (LB-07) carries its own roster with the same types, because a team
  library outlives and spans projects. The source format is designed elsewhere; this design gives it the types and rules (11.2).
- **Per agency client.** An agency is a person or studio serving many clients. Which client is active, with which key,
  libraries and credentials, is per-user state in `<config dir>/omuzaki/agency.toml`, never in a project, because a
  project must not learn about the agency's other clients (section 10). The per-client role still comes from that
  client's project rosters.

## 3. The role table

`Role::grants()` returns these scopes, with no file roots (a person's `fs` is rooted per path, ADR 0013). The newest
scopes are `review` (comment and reply) and `script` (start a script run, save a script). The table
covers every scope in `Scope::ALL`, and tests assert each role's scopes by name, never a count. Every row is a subset
of `Grants::person()`, and an agent never exceeds its person, because the executor intersects the session ceiling with
`Principals::grants` (agent-sessions §3.3).

| Scope | Owner | Admin | Designer | Performer | Reviewer | Viewer | Why this split |
|---|---|---|---|---|---|---|---|
| `read` | ✓ | ✓ | ✓ | ✓ | ✓ | ✓ | Everyone on the team sees the project |
| `edit:character` | ✓ | ✓ | ✓ | — | — | — | Making the character is the designer's job; a performer changing the rig mid-show is how shows break |
| `edit:scene` | ✓ | ✓ | ✓ | ✓ | — | — | Performers set up their own stage (lights, props, cameras) |
| `edit:library` | ✓ | ✓ | ✓ | — | — | — | Library items are design assets |
| `render` | ✓ | ✓ | ✓ | ✓ | ✓ | — | Reviewers must see a turnaround to judge it; a viewer looks at what exists. Viewport drawing is not an op and needs no scope |
| `perform` | ✓ | ✓ | ✓ | ✓ | — | — | Designers test-drive what they build |
| `broadcast` | ✓ | ✓ | — | ✓ | — | — | Going live is the performer's call, under the broadcast policy (8) |
| `publish` | ✓ | ✓ | ✓ | — | — | — | Releases come from those who made them, under the publish policy (8); performers' clips go out through a designer |
| `spend` | ✓ | ✓ | ✓ | — | — | — | Paid generation is design work; money stays with few roles (session budgets still cap agents, agent-sessions §8) |
| `net` | ✓ | ✓ | ✓ | ✓ | — | — | Connectors and AI tools (designers), chat and stream platforms (performers); reviewers and viewers need none |
| `fs` | ✓ | ✓ | ✓ | ✓ | — | — | Importing references and exporting (designers), recordings (performers); every path still needs its root |
| `native` | ✓ | ✓ | — | — | — | — | Native plugins run with the person's full rights: only those who govern the team load them |
| `review` | ✓ | ✓ | ✓ | ✓ | ✓ | — | Comment and reply; a viewer who should comment is made a reviewer |
| `script` | ✓ | ✓ | ✓ | — | — | — | Scripts and macros are design tooling; every inner call is still checked against the runner's grants, so a script never exceeds its person |

- **Owner and admin hold the same scopes.** They differ in governance (6.3): only owners add or remove owners and
  admins, set the review policy, the group map and the trust root.
- **The viewer holds `read` only.** Commenting needs reviewer or higher, which supersedes "viewers comment" in the
  brief (cloud lead, 2026-10-13). A role that can only look is the one an agency hands a client's stakeholders.
- **An agent is capped by its person's role.** `agent_default()` keeps `review`, but the executor intersects every
  session with the role: an agent's grants are its ceiling (the agent default plus what a person granted) ∩
  `role.grants()`. A viewer's agent therefore ends with `read` only, and a reviewer's agent with `read` and `review`.
- **A scope added later** is held by owner and admin only until an ADR gives it a row, so a new capability never
  reaches the lower roles by accident.
- **Approving is not a scope.** It is the role rule of 8.3 (reviewer, admin or owner), so it cannot be delegated to an
  agent by a grant.
- **The table is fixed.** Custom roles were rejected for now: every custom role is a new security review, and the
  six cover VC-10. A later change adds rows to this table through an ADR, never per team.

## 4. The `team.*` ops and their scope

| Op | Kind | Scope | Who |
|---|---|---|---|
| `team.describe {}` | query | `read` | anyone with `read`, agents included: members (name, role, fingerprint), the caller's role, the policy, the status |
| `team.create {name}` | edit | `read` + person only | the local person of a solo project; becomes owner (6.2) |
| `team.invite {key, role}` / `team.accept {invite}` | edit | `read` + person only | invite: an admin or owner (6.4); accept: the invitee, on their machine |
| `team.add_member`, `team.set_role`, `team.remove_member`, `team.revoke_key` | edit | `read` + person only | an admin for designer, performer, reviewer and viewer; an owner for anyone (6.3) |
| `team.set_policy {publish, broadcast}`, `team.set_groups {map}` | edit | `read` + person only | an owner |
| `team.import_approval {file}` | edit | `read` + person only | any member (8.5) |
| `team.pin_root {team, key}` | user record | `read` + person only | the local person, on first open or after a team is founded again (6.2) |
| `team.audit {..}` / `team.export_audit {to}` | query / job | `read` (+ `fs` for export) | an owner or admin, and their agents (9.3) |

- **Why `read` and not a new scope:** membership is not a capability a person can lend to an agent, so a scope (which
  grants can carry) is the wrong tool. Every write calls `OpError::person_only("only a person can change who is on
  the team")`, never queued, then checks the caller's role through the roster; a wrong role is `Denied` naming the
  role needed ("only an owner can add an admin").
- **What an agent may read:** `team.describe` (display names, roles, fingerprints, policy). Agents of owners and admins
  may also read `team.audit`. Nothing team-related contains a secret: keys are public, credentials appear by label.
- **Every write appends one signed statement** through the `Signer` service after `ctx.require_person`; labels read
  "Team: add Sam as Designer", "Team: Sam from Designer to Reviewer".
- **`team/` is written only by `team.*` ops** (they refuse a generic edit under `team/`). That is a convenience, not
  the protection: the protection is that every reader verifies the log (6.2).

## 5. Identity: from a person actor to a key

- **`Actor` does not change.** `Actor::Person { name }` keeps its display name; the key is looked up, so nothing that
  builds an actor can name a key (the same reasoning as agent-sessions §3.2).
- **`PersonDirectory::key_of(&Actor) -> Option<KeyId>`** is a service the app registers. On a studio there is one local
  person, and the directory returns the machine identity's key (`Signer::identity()`) for that person's actor.
  It returns `None` for agents, plugins, `System` and any other name.
- **The principal of a session** is an `Actor::Person` (`on_behalf_of`), so an agent's role is its person's role
  through the same lookup. Remote people never appear as actors: their decisions arrive as signed statements (6.4, 8.5).
- **No key yet** (identity not set up): a team project treats the person as unknown (6.5) and the Team panel offers
  "Set up your identity" first.

## 6. Membership: the signed log

### 6.1 Snapshot or log

Two shapes were weighed. **One snapshot signed by the owner** is simpler, but every change needs the owner's key, so
an admin cannot do the job VC-10 names, and an agency owner becomes a bottleneck for every client. **An append-only
log of signed statements** lets each change be signed by whoever made it and checked against the roster as it stood
just before. Picked: **the log**. Its costs are bounded: a fold over at most a few hundred short entries, and forks
that are ordinary merge conflicts (6.6).

### 6.2 Format

- Entries live at `team/log/<seq>` (`seq` zero-padded to 6 digits), one TOML node each, so a new entry is a new file
  and git or a branch merge never edits an old one.
- Each entry is `Signed<Statement>` (identity's envelope `{ body, signer: Identity, signature }`) with
  `Statement { seq: u64, prev: Option<ContentHash>, change: Change }`, signed under `Domain::Membership`. `prev` is
  the canonical hash (ADR 0009 rule 3) of the previous signed entry.
- `Change { Create { team: TeamId, name: String, owner: Member }, Add { member: Member, invite: ContentHash },
  SetRole { key, role }, Remove { key }, Revoke { key, reason }, SetPolicy { policy }, SetGroups { groups },
  SetLicence { licence: ContentHash } }`.
- `Roster` is the fold of the log: `{ team, members, revoked, policy, groups, licence }`. It is computed, never
  stored, so it can't drift from the statements.
- **The trust root** is the `Create` entry's owner key. Each member's client pins it on first open in a team record
  keyed by `TeamId` (`<config dir>/omuzaki/teams.toml`, not identity's `TrustStore`), written only by the person-only
  op `team.pin_root` after the person confirms the fingerprint ("This team is owned by Ana, ab12 cd34 … Trust it?").
  A log whose root differs from the pin is `Invalid` ("the team's owner key changed").

### 6.3 Who may sign what (checked against the roster folded so far)

| Change | Signed by |
|---|---|
| `Create` | the owner it names (`seq` 0, no `prev`) |
| `Add`, `SetRole`, `Remove`, `Revoke` touching designer, performer, reviewer or viewer (before and after) | an admin or owner |
| the same touching owner or admin | an owner |
| `SetPolicy`, `SetGroups`, `SetLicence` | an owner |

Invariants after each entry: at least one owner; keys unique; no revoked key is a member; nobody changes their own
role; `approvals` is within `1..=eligible - 1` (8.3). A revoked key is revoked for good; a new key is a new member.

### 6.4 Invitations without a server

1. An admin runs `team.invite {key, role}`: a `Signed<Invite>` with `Invite { team, key, role }`
   (`Domain::Membership`), written to a file (`<name>.omuzakiinvite`, a few hundred bytes) the admin sends any way they
   like.
2. The invitee opens it in their studio; `team.accept` signs an `Acceptance { invite: ContentHash }` with their key,
   proving they hold it (no adding a mistyped key, or someone's key without their knowledge), and writes a reply file.
3. The admin imports the reply; `team.add_member` checks both signatures and appends `Add { member, invite }`.
4. The new member gets the project the usual way (sync, git, a copy) and pins the root (6.2).

### 6.5 Status and failure

`RosterStatus { Solo, Active { members }, Invalid { at: u64, reason } }`.

| Situation | The local person's grants | What the UI says |
|---|---|---|
| no `team/` | `Grants::person()` | nothing |
| a member | `role.grants()` | "You are a Designer" |
| a key not on the roster, or no key | `read` | "You're not on this team. Ask an admin for an invite" |
| a revoked key | nothing | "Your access to this team was revoked" |
| a bad signature, a broken chain, a failed invariant, a changed root | `read`, for everyone | "The team list was changed without a valid signature (entry 14). Everyone is read-only until it's restored" |

- **Fail closed.** An invalid log gives everyone `read` only, never the last good roster: a tampered entry might be
  the one that revoked someone.
- **The way back:** History (VC-01) shows the entry and when it arrived; restoring `team/` to before it is allowed to
  any key that was owner or admin in the last valid prefix, and that is the only edit an invalid team allows. Git
  users can revert the file.
- **A lost owner key:** another owner signs on. With no other owner, membership is frozen for good, and the team
  starts again with a new `Create` that every member must re-pin. The Team panel therefore nudges a sole owner to add a
  second one.
- **An unknown key reads** because the folder is readable anyway, and reading lets them see the project and ask to
  join; a revoked key holds nothing, so the refusal is explicit.

### 6.6 Forks

Two admins appending on two branches both write `team/log/000014`. Git and branch merge (branches.md) show a conflict
on that one file. A person keeps one; the other change is re-applied as `000015` by someone allowed to sign it. Until
then the merge isn't done, so the roster never folds a fork.

## 7. Enforcement, honestly

| Where | What is real | What isn't |
|---|---|---|
| **Locally, for agents** | The executor intersects every session with the role (`TeamPrincipals`); approvals need a person of the right role | — |
| **Locally, for people** | The studio's own UI, CLI and ops refuse what the role doesn't hold | A person with the folder can edit the TOML, or run a modified client that ignores roles |
| **On signed artifacts** | A roster, approval or published item carries signatures; teammates' clients and libraries verify them, so a forged change is seen as forged | A person can still change their own copy; they can't make it verify |
| **On servers** (hosted pieces, 13) | A team store or review host accepts only what verifies against the roster | Only exists if the maintainers decide to run one |

The sentence the Team panel shows: **"Roles decide what this team's Omuzaki and its agents will do, and teammates'
Omuzaki checks the signatures. They don't lock the files: anyone with a copy of the project can still change it."**

## 8. Review policy (VC-11)

In `team-review.md` §8: which requests queue as `Need::Review`, people's own requests, who approves and how many,
the approval card's evidence, and a reviewer on another machine.

## 9. The audit trail

In `team-review.md` §9: which facts live where, retention, reading and the signed export.

## 10. Agency accounts

- **The registry:** `<config dir>/omuzaki/agency.toml`, owner-only file permissions: `clients = [{ id, name, slot,
  roots, libraries, credentials }]` and `active`. `slot` names an identity slot (ADR 0042 D2), never a key; only
  `omuzaki-identity` touches `identity/<slot>/…` in the secret store.
- **Switching** (`agency.switch {client}`, person only, a menu in the title bar): closes open projects, selects the
  client's slot through `identity.use {slot}` (so the `Signer` and `PersonDirectory` follow), and swaps the library
  sources and caches, recent files, MCP and HTTP credentials, and the allowed roots. Opening a project under another
  client's root asks to switch first.
- **Nothing crosses:** one key per client (a client can't link the agency's work for others by key; identity lets a
  person hold several identities on one machine and pick one per project), per-client library caches,
  per-client credentials, and the library resolver sees only the active client's sources. The clipboard of items and
  parts is cleared on switch.
- **Per-client roles** come from each client's project rosters, where the agency's per-client key is a member.

## 11. Hooks

### 11.1 SSO (VC-12)

`GroupMap { rules: Vec<GroupRule { group, role }> }` lives in the roster (`SetGroups`, owner only). A rule may name
admin and below, never owner: the owner is a key, not a group. The roster stays the source of truth: a group never
grants anything by itself. Group membership becomes a role only when someone allowed to sign it appends `Add` or
`SetRole`: an admin's client after an OIDC sign-in shows the claimed groups and the suggested role, or a hosted
identity service with a delegated admin key (a maintainer decision). Names compare exactly.

### 11.2 LB-07

Library roles are these roles. A team library source carries a log (6.2) with the same types and rules, verified
the same way. In it, designer and above put items, reviewer and above approve them, and admin and above deprecate
other people's items (`Role::library_rights()`). There is no shared team key: an item is signed by a member's own
key, and the roster says that key may publish for the team. An item's review state is its approval records:
`Approved` means `ReviewRecord`s (signed under `Domain::Approval`) met the roster's
`library.approvals`. `Roster.licence` is the team's
licence text, by hash. The source format, index and sync are designed with the shared-library-sources design (`docs/design/library-sharing.md`, ADR 0057).

### 11.3 Seams accepted by the cloud lead

- **Identity:** the `Signed<T>` envelope and the `Roster`, `Membership` and `Approval` tags are identity's.
  The log uses `Membership`; `Roster` is left for a source's index to bind a log head. The roster's `revoked`
  list reaches `TrustStore` as a source of `Revoked` (a later change wires it); the team root pin is a team record, not a
  `TrustStore` entry (6.2). Identity also adds identity slots (10) and `Domain::AuditExport` (9.3).
- **Comments:** viewers read, reviewers and up comment (3). Resolving a comment needs a person and `review`,
  with no role check. "An admin may delete any comment" calls `Role::can_moderate()` (admin and owner). The
  `no_open_threads` policy reads `summarise` (8.1).
- **Git:** `git.push` needs `net` and `publish` (owner, admin, designer) and falls under the publish review
  policy (team-review §8.1).
- **Library sources:** `Roster.licence`, `ReviewPolicy.library` and the put, approve and deprecate rights of
  11.2 are requirements.

## 12. The UI contract (words; wireframes come separately)

- **Team panel** (Settings › Team). Solo: "Just you. Create a team to invite people" and Create. Active: members with
  name, role, short fingerprint and "you"; "You are a Designer: you can edit characters and scenes, render, publish
  with review" (the role in plain words from the table); Invite, change role and remove controls disabled with the
  reason when the role can't; the review policy as "Publishing needs 2 reviewers' approval". Invalid: the banner of
  6.5 with "Show in History". Not a member: "You're not on this team" and "Copy my key for an invite".
- **"Who can do what":** a read-only grid of the role table in plain words, opened from the panel.
- **The reviewer's card:** section 8.4; the requester's own card shows "Waiting for 2 reviewers (1 so far: Eve)",
  Withdraw, and Export for a reviewer elsewhere.
- **Agency:** the client name in the title bar and the switcher; nothing else changes per client.

## 13. Hosted services (§37)

The client enforces roles, review and audit; there is no entitlement check anywhere in it.
The only hosted pieces are a team store, review-link hosting, a collab relay and an identity service for SAML
and group sync. Everything in this design works without them (a folder, a file, git). The maintainers decide
whether to build, rent or defer each, who holds the accounts, the spend cap and data protection.

## 14. Tests

Under `FakeClock`, with `MemorySigner` keys and no network:
- **Role by scope matrix** through the real executor: 6 roles × every scope in `Scope::ALL` for the person and
  for an agent session whose ceiling is `Grants::person()`; results identical, and denials never queue.
- **Agent narrowing:** a viewer's agent holds `read` only; its `edit:character` is `Denied`, not queued.
- **Review:** self-approval refused; an agent can't resolve; 2 of 3 eligible approvers run the op exactly once with
  `by` and `also`; a rejection ends it; `approvals` outside `1..=eligible - 1` refused; a signed remote approval
  imports, a statement for another request or by the requester is refused; after a restart, an unchanged request
  re-queued gets a new `RequestId` and every earlier statement is refused; content changed between approvals discards
  them and re-queues with the new preview, publishing nothing; a remote reviewer whose copy re-plans to a different
  digest can't approve (only reject).
- **Audit retention:** a publish with no review policy still has its `Published` row after its archive is pruned;
  with two journal sessions pruned, every retained fact keeps its absolute date (`started + at`), and `since` and
  sorting give the same rows as before pruning.
- **Log:** every forged, reordered, replayed or root-swapped entry folds to `Invalid`, and grants are `read`; an
  admin promoting someone to admin is refused; a revoked key holds nothing.
- **Audit:** the worked example below produces exactly the rows shown.

## 15. Worked example

Team "Lumen", root key Ana. Members (fingerprints shortened): Ana owner `a1`, Ben admin `b2`, Cy designer `c3`, Dee
performer `d4`, Eve reviewer `e5`, Fay viewer `f6`. Policy: publish needs 2 reviewers; broadcast none.

| Member | `role.grants()` |
|---|---|
| Ana, Ben | every scope |
| Cy | read, edit:character, edit:scene, edit:library, render, perform, publish, spend, net, fs, review, script |
| Dee | read, edit:scene, render, perform, broadcast, net, fs, review |
| Eve | read, render, review |
| Fay | read |

**Trace A: a viewer's agent.** Fay connects Claude Code (ceiling `agent_default()`).

| Step | Result | Audit row |
|---|---|---|
| open session `claude-code#1` for Fay | effective = ceiling ∩ {read} = {read} | `SessionOpened` by `claude-code#1` for Fay, grants [read] |
| `face.set_profile` (`edit:character`) | `Denied` `MissingScope`; not queued (Fay lacks it) | none (refusals are not facts) |
| `team.describe` | members, roles, policy | none (queries are not logged) |
| `team.set_role` | `Denied` `PersonOnly` | none |
| disconnect | ended | `SessionEnded` Disconnected |

**Trace B: a publish under the two-reviewer policy.** Cy runs `io.export_vrm` (`publish`).

| Step | Result | Audit row |
|---|---|---|
| dry-run: licence gate passes | preview kept | none |
| queued | `Waiting`, `Queued { approval: 7, need: Review { policy: "publish" } }`, `own: true`, session `SessionId::NONE` | `ApprovalRequested` #7 by Cy, publish, 0 of 2 |
| Cy approves #7 | `Denied` `NotEligible` "you can't approve your own request" | none |
| Fay approves #7 | `Denied` `NotEligible` "viewers can't approve" | none |
| Eve approves #7 | counted, 1 of 2; statement signed `e5` | `Approval` #7 approve by Eve (1 of 2) |
| Dee approves #7 | `Denied` `NotEligible` "performers can't approve" | none |
| Ben approves #7 | 2 of 2: runs as Cy's one-op transaction; statements at `team/approvals/<hash>` | `Approval` #7 approve by Ben (2 of 2) |
| applied | `OpRecord.approval { id: 7, by: Ben, also: [Eve] }` | `Publish` `io.export_vrm` by Cy, approved by Ben, Eve |

## 16. The contract (names final; doc comments are the implementer's)

**`omuzaki-team-api`** (API; depends on `omuzaki-ops-api`, `omuzaki-doc-api`, `omuzaki-identity-api`):
- `Role { Owner, Admin, Designer, Performer, Reviewer, Viewer }` (serde snake_case, `ALL`, `rank()`, `can_approve()`,
  `can_moderate()`, `library_rights() -> LibraryRights { put, approve, deprecate_others }`),
  `Role::grants(self) -> Grants` (section 3); `TeamId(pub Slug)`; `Member { key: KeyId, name: String, role: Role }`.
- `Statement`, `Change`, signed as `Signed<Statement>` (6.2); `Invite { team, key, role }`, `Acceptance { invite:
  ContentHash }` (6.4); `MembershipLog { entries: Vec<Signed<Statement>> }` with `fold(&self, root: &KeyId) ->
  Result<Roster, LogError>`, where `LogError` names the entry and why (signature, chain, signer's role, invariant,
  root).
- `Roster { team, members, revoked: Vec<KeyId>, policy: ReviewPolicy, groups: GroupMap, licence: Option<ContentHash>
  }` with `role_of(&KeyId) -> Option<Role>` and `eligible_approvers()`; `ReviewPolicy`, `Requirement`, `LibraryPolicy`
  (8.1); `GroupMap`, `GroupRule` (11.1); `RosterStatus` (6.5).
- `ReviewRequest`, `RequestId { session: String, lsn: u64 }`, `ApprovalStatement`, `ApprovalDecision { Approve, Reject
  }` (team-review 8.5).
- `trait PersonDirectory: Send + Sync { fn key_of(&self, person: &Actor) -> Option<KeyId>; }`;
  `TEAM_ROOT = "team"`, `LOG_PATH = "team/log"`, `APPROVALS_PATH = "team/approvals"`.

**`omuzaki-ops-api`** (additive; folded into with the `review` scope, so the hub crate changes once in
Phase 5):
- `ReviewRequirement { policy: String, approvals: u8 }`; `trait ReviewPolicies: Send + Sync { fn requirement(&self,
  scope: Scope, grant: bool) -> Option<ReviewRequirement>; fn approver(&self, person: &Actor, requester: &Actor,
  policy: &str) -> Option<String>; fn verify_statement(&self, approval: &PendingApproval, statement: &str) ->
  Result<(Actor, String), OpError>; fn evidence(&self, approval: ApprovalId) -> Option<(DocPath, Value)>; }`
  (`approver` returns the approver's full key, 64 hex, which counts toward N, never a fingerprint; or `None`;
  `evidence` is the `team/approvals/` node the executor writes in the approved op's transaction, 8.5).
- `ApprovalRef.also: Vec<Actor>` (serde default); `PendingApproval.own: bool` (serde default; team-review 8.2);
  `SessionId::NONE` (`SessionId(0)`, never allocated by the executor);
  `Decision::ApproveSigned { statement: String }`; `ErrorDetail::NotEligible { reason: String }`.

## 17. Alternatives rejected

- **A roster snapshot signed by the owner** (6.1): admins couldn't do their job.
- **A new scope for team management** (4): a grant could lend it to an agent.
- **A key in `Actor`** (5): forgeable by anything that builds an actor, and it changes actor equality.
- **Keeping the last good roster on a bad entry** (6.5): the bad entry may be the revocation.
- **Custom roles** (3): a security review per team.
- **Session facts in the document** (9.1): every agent call would edit shared files; they stay in the journal.
- **An entitlement check in the free client** (13): Omuzaki is free software (§37, 2026-10-12).

## 18. Open decisions

Pending. The maintainers' answers go here: hosted services, accounts and keys,
data protection, spend, self-hosting and SSO.
