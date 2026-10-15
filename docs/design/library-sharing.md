# Shared library sources: the team library, the marketplace client and the signed index

**Status:** accepted in part: the crate placement and Q4 to Q9 of §6 are ruled (Runtime lead, on #1534); Q1 to Q3 stay open as the owner's. **Feature:** LB-07 (and the client half of LB-08). **Decision record:** `docs/decisions/0057-library-sharing.md`.
**Spec:** v3 §28 (LB-02, LB-06, LB-07, LB-08), §29 (VC-10, VC-11), §37.
**Builds on:** `docs/design/library.md` (D2 ids and publishers, D5 the record is the lock, D8 rights and the gate, D9 ops, D11 scopes, D16 version rules), `docs/design/identity.md` (D3 the envelope, D4 trust, D10 domains), `docs/design/team.md` (§3 roles, §6 the signed log, §11.2 library roles), `docs/design/team-review.md` (§8 review policy), ADR 0013 (file roots), ADR 0019, 0042, 0043.

`library.md` D11 says the team scope is "a remote source with a local cache" and the only way to share a person's items. This page says what that source is: how it is stored, why it can be untrusted, who may put what in it, what a team licence is, how review state is computed, how a client syncs it, and which parts of the same protocol a marketplace client reuses. It adds no signing scheme. Everything signed here is identity's `Signed<T>` under a domain tag identity already lists.

## 1. What this covers

| Asked for | Where |
|---|---|
| What a source is, the layout, the protocol | D1, D2 |
| Trust, roles, immutability, delete, write conflicts | D3 to D6 |
| The team licence and the gate | D7 |
| Review state and policy | D8 |
| Sync, cache, offline, limits | D9 |
| Ops and agents, publish preflight | D10 |
| Marketplace extension points | D11 |
| Privacy, threats, tests | D12 to D14 |
| Contract, crates, task cut | §5 |
| A worked example | §3 |
| Open questions (owner's) and decisions | §6 |

## 2. The shape of it

```
                     person's machine                                      shared place (untrusted)
┌──────────────────────────────────────────────┐              ┌─────────────────────────────────────┐
│ Library service (library.md D11 order)        │              │ folder | S3-compatible bucket | git  │
│   first-party → my library → team scopes      │   ItemStore  │ | marketplace (hosted, later)        │
│        ▲                                      │◀────────────▶│  team/log/…    items/…    store/…    │
│  SourceCache: verifies EVERYTHING, then       │  bytes only  │  reviews/…     index/…               │
│  keeps verified copies under teams/<id>/      │              └─────────────────────────────────────┘
└──────────────────────────────────────────────┘
        verify(log, index, manifests, reviews, assets)  =  pure functions in omuzaki-library-source-api
```

A store moves bytes and nothing else. It never decides anything, so a hostile or buggy store can only refuse to serve, or serve things that fail verification. All the checking is in one pure module (wasm-clean, no I/O) that a test can drive without a store.

## 3. Worked example (on disk)

A team `acme` with an owner (Ana), a designer (Ben), two reviewers (Cleo, Dev) and a policy `library.approvals = 2`. Keys and hashes are shown as `<64 hex>` here; the conformance task generates the real tree from test seeds (`MemorySigner::from_test_seed`) and checks it byte for byte.

```
acme-library/                          one folder: a NAS share, a synced folder or a bucket prefix
  team/log/000000.toml … 000004.toml   Signed<Statement>: Create, Add Ben, Add Cleo, Add Dev, SetPolicy{approvals 2}
  licence/<hash>.toml                  the team licence text (D7), named by its hash, set by SetLicence
  items/acme/hair/bob/1.0.0.toml       the manifest, signed by Ben (library.md D2 shape, D11 layout)
  reviews/acme/hair/bob/1.0.0/
      000-<keyhex8>.json               Ben: request                 (draft → in review; his seq 0)
      000-<keyhex8>.json               Cleo: approve                (1 of 2; her seq 0)
      000-<keyhex8>.json               Dev: approve                 (2 of 2: approved; his seq 0)
  store/ab/ab12…ef.bin                 assets, content-addressed (BLAKE3), as in the person's store/
  index/000003-9f2c1a7e.json           SignedIndex, seq 3, signed by Ben (the publisher), then 000004-… by Dev
```

**Draft, in review, approved.** Ben publishes `acme/hair/bob@1.0.0`. The index entry reads `Draft` (no review request yet). Ben signs a `request_review` record: `InReview`. Cleo signs `approve`: still `InReview` (1 of 2). Dev signs `approve`: `Approved { by: [cleo, dev] }`. Each step is a new record file and, for convenience, a new index snapshot; no existing file is edited.

**A tampered index is refused.** An attacker with write access to the folder takes `index/000004-…json` and (a) flips the entry's `hash`, (b) removes Dev's approval from it, (c) restores `000002-…` to hide the item. The client's answers: (a) `Tampered(IndexSignature)`: the snapshot hash changed, the signature no longer holds; (b) the same; (c) `Rollback { pinned: 4, served: 2 }`: the client pinned seq 4 the last time it synced and never goes down. In every case the client keeps its last verified copy and says "this source changed in a way it can't explain" with the reason. The full matrix is D14.

**A character published with a team item.** The team licence says `commercial: ok`, attribution "Acme Studio", `outside: as_licensed`. A character uses `acme/hair/bob@1.0.0` (applied, so its record at `characters/hero/applied/a1` holds the effective rights). `io.check_export` for a publish passes, and the export's attribution lists "Acme Studio". If the owner had set `outside: blocked`, the record would carry `team_only: true` and the gate answers: finding at `characters/hero/applied/a1`, "hair/bob 1.0.0 is for use inside team acme only", hint "apply an item that can be published, or remove what it made". Personal (`private`) use is never blocked, and neither is a share inside the team.

## 4. Decisions

### D1. A source is a bag of immutable files; one protocol, several stores

A **team source** is a place that holds the files below. Which place is a store detail:

1. **A folder first.** A NAS share, a synced folder (Syncthing, a cloud-drive client): no account, no server, nothing to run. It is the default because most teams of two to ten people already have one.
2. **An S3-compatible bucket second**, with the team's own credentials in the secret store (CN-05). Same files as object keys under a prefix.
3. **Later: a git repository** (the layout is already files, so it is a store over `omuzaki-git`, ADR 0045) **or a hosted store** (D11).

**One protocol.** A store implements `ItemStore` (§5.1), whose methods are "read these bytes" and "create this object if absent". Nothing in the protocol needs a listing with ordering, a lock, a rename or a server-side clock, because those are what folders, buckets and git do differently. The only atomic primitive asked of a store is **create-if-absent** (`O_EXCL`; S3 `If-None-Match: *`; a git fast-forward). Everything else is a read.

**The layout mirrors the person's library (D11).** `items/<publisher>/<name…>/<version>.toml` and the content-addressed `store/` are the same shapes `library.md` D11 gives "My library". So an install is a file copy plus verification, and a team folder can be read by eye or by `ls`. The additions are `team/log/`, `reviews/`, `licence/` and `index/`.

**The id namespace is the team's.** In a team source every id starts with the team's slug (`acme/hair/bob`); `omuzaki/` is refused (library.md D2). The signing key stays a member's key (identity D10: a team has no private key); the manifest's `publisher` is `{ name, key }` of that member. So "two publishers, one id" cannot happen inside a source, and the id says where an item lives.

### D2. The signed index is a rollback anchor and a listing, not the truth

`SignedIndex` is a snapshot: `{ team, seq, prev, items: Vec<IndexEntry>, roster, signer, signature }`, signed under `Domain::LibraryIndex` by the member who wrote it (the hash covers everything except the signature, identity D3). Additions to the outline's four fields: `seq` and `prev` give the chain, `signer` says who (any member with the put, approve or deprecate right may write a snapshot, D3), and the three together are what makes rollback visible.

- **`roster`** is the hash of the membership log's head entry. The client folds the log in the same source (team.md 6.2) against the root it pinned, and refuses a snapshot whose `roster` is not an entry of that log, or is older than the roster of any snapshot in its `prev`.
- **An `IndexEntry`** is `{ id, version, hash, publisher: KeyId, added_at: u64, assets: Vec<(ContentHash, u64)>, review: ReviewState, reviews: Vec<ContentHash>, listing: Option<Listing> }`. `added_at` is the log `seq` the snapshot's roster had when the entry first appeared (D3). `review` is the writer's summary for fast listing; **the client never trusts it**: it recomputes the state from the signed records (D8) before showing "approved" or applying anything. `listing` is the marketplace extension (D11) and absent for a team.
- **Snapshots are separate files:** `index/<seq:06>-<hash8>.json`, created if absent, never edited. A **head** is a snapshot that no other snapshot lists in `prev`. Because every change is another snapshot and the content is hash-named, a synced folder cannot lose an update to a last-writer-wins overwrite.
- **More than one head** (two people wrote offline, or a git merge; the heads may have different `seq`, e.g. one branch at 6 and another at 5) is a **fork**, not an error: the status is `Forked`, the items both agree on stay usable, and the next writer with put rights makes a **merge snapshot** (`prev` lists both). Merging is a union, since every file is immutable; a union that has one `id@version` with two hashes is refused for that item only (D4).
- **Why an index at all.** Listing a bucket or a share with thousands of files each sync is slow and, on a bucket, costs requests. The index also gives the client a number to pin (`seq`), which the files alone cannot. It carries no secret and no authority.

### D3. Trust: the storage is untrusted, the client verifies everything, and an item needs a roster key

The rule the whole page rests on: **a thing is used only if the client could re-derive its validity from the pinned team root.** The checks, in order, all in `omuzaki-library-source-api::verify`:

1. **The log.** Fold `team/log/` from the pinned root (team.md 6.2). A bad signature, chain, signer role or invariant makes the source `Invalid` and read-only, with team.md's message. The root is pinned on first use by a person, who is shown the owner's fingerprint (`team.pin_root`, person-only). Nothing in the source can pin itself.
2. **The index** chain, verified page by page in `(seq, hash8)` order (every snapshot not yet verified, found by `index(have, cursor)`, not only the highest `seq`): signature valid; the signer held a write right (below) **at the snapshot's own `roster`** and is not revoked at the head; `seq` above every `prev`'s (`Tampered(Seq)`); each `prev` already verified or earlier in the stream; `roster` an entry of the log, not older than its parents'. **Rollback is judged on the pinned ancestry, never on `seq`:** the pin holds the verified heads, and the client merges what is served into what it holds, so it never goes down. A snapshot below the pin's `seq` that is not an ancestor of a pinned head and verifies is a late fork, a second head (D2), not `Rollback`. `Rollback` is a store that no longer lists a pinned head (`IndexPage.missing`, an honest restore from backup): the client keeps its state and says so. The log follows the same rule: the served log must extend the pinned log head. **Every child preserves its parents:** each parent entry's immutable fields (`id`, `version`, `hash`, `publisher`, `added_at`, `assets`) appear unchanged in the child, and a merge holds the union of all its parents; an omitted entry is `Tampered(DroppedEntry)`, so a snapshot can never delete an item (D4).
3. **Each item used** (only when listed, applied, searched with details, or installed; the client does not verify what it never opens):
   - the manifest parses (size-limited), and its canonical hash (`ItemManifest::hash()`, library.md D2: it excludes `publisher` and `signature` and normalises the TOML, so file formatting does not matter) equals the entry's `hash`. `IndexEntry.hash` is that canonical hash, never a hash of the file bytes;
   - the id begins with the team slug and `publisher.key` equals the entry's;
   - the signature verifies under `Domain::LibraryItem` for that key (identity D3, the legacy item hash);
   - **the key was allowed to publish when the entry was added:** in the roster folded up to `added_at`, `publisher` is a member whose role has `library_rights().put` (designer, admin, owner; team.md 11.2), and the key is not in `revoked` at the head;
   - every asset the manifest references is present by hash with the entry's size; a mismatch is `Tampered(Asset)`.
4. **Reviews and deprecations** are checked on their own (D8).

**Who may write what** (team.md 11.2, `Role::library_rights()`):

| Act | Role | File |
|---|---|---|
| publish an item | designer and above | `items/…`, `store/…`, then an index snapshot |
| request review, withdraw it | the item's publisher | `reviews/…` record |
| approve or reject | reviewer and above; not the publisher of that hash | `reviews/…` record |
| deprecate one's own item | its publisher | `reviews/…` record |
| deprecate another's | admin and above | `reviews/…` record |
| write an index snapshot | anyone with one of the rights above | `index/…` |

**`Remove` and `Revoke` differ.** A removed member's earlier items stay accepted, because the roster at their `added_at` allowed them. A **revoked** key (compromise, team.md 6.3) is rejected for everything it signed, past and future, as identity D4 does ("there are no timestamps to tell old from new"). A snapshot may only give *new* entries `added_at` equal to its own roster's `seq`, and the signer must hold put at that seq, so a stolen removed key cannot backdate an entry to when it was still a member.

**What this cannot do, said plainly** (team.md §7 says the same): a store can withhold. It can serve nothing, or an old but valid state the client has never seen (a first sync has no pin). The client shows the snapshot's `seq` and the sync time, and a person can compare `seq` with a teammate. A member with put rights who is not yet revoked can publish a bad item; the answer for that is review policy (D8), not cryptography.

### D4. Versions are immutable, delete means deprecate, and writes never overwrite

- **Create-if-absent for every file.** `put` of `items/<id>/<version>.toml` that already exists is `SourceError::Exists`, unless the bytes hash the same (then it succeeds, so a retried publish is harmless). A second publish of one `id@version` with different bytes is refused by the store and, if it arrives anyway (a merged copy), by the client as `Conflict { id, version, hashes }`: **both are ignored for that item** and the item shows as "conflicting versions; ask a designer to bump the version". The remaining items work.
- **No delete in the protocol.** `ItemStore` has no `delete`. "Delete" is a signed `Deprecate` record (D8). A deprecated item stays resolvable for projects that pinned it (D5 of `library.md`: the record is the lock) and is hidden from search by default. Removing bytes is the storage owner's decision outside Omuzaki (§6, Q5: decided).
- **A newer version, not an edit.** D16's rules apply: a patch is values only, a minor adds, a major breaks. `put` is preceded by the preflight diff (D10) so a team cannot publish a breaking change as a patch.
- **Write conflicts in a synced folder.** The sync tool may make a "conflicted copy" of a file two machines created at the same path. Item and record paths include a content hash or a key (`000-<keyhex8>.json`, `<seq>-<hash8>.json`), so two machines almost never create the same path with different bytes. When they do, the client sees two verifying files where one was expected and treats them as above (item `Conflict`, index `Forked`). Nothing is overwritten, so nothing is lost, and the cure is one more snapshot.

### D5. Review records are files, not fields

A `ReviewRecord` is `Signed<ReviewBody>` under `Domain::Approval` (identity D10 already lists it for item reviews): `ReviewBody { team, item, version, hash, roster: ContentHash, seq: u32, decision, note: Option<String> }`, `decision` one of `Request`, `Withdraw`, `Approve`, `Reject`, `Deprecate { reason }`. Its fields are `deny_unknown_fields` with a required `decision` and `item`/`hash`, so it can never parse as team-review's `ApprovalStatement` (identity D3's rule for types that share a tag). It binds **team, id, version and hash**, and `roster` (the hash of the membership-log entry the signer saw, which must be an entry of the log), so a record cannot be replayed on another team, another version, or a manifest with different content. **Ordering is signed, per key.** `seq` is the signer's own counter for that `id@version@hash`, starting at 0 and rising by one with each record that key writes; it is inside the signed body, so no store, listing order or file name can change it. A key's **standing** is its record with the highest `seq`. Two different records from one key at the same `seq` are equivocation: that key's records are ignored for that hash and the client warns (`Tampered(Record)`). The fold (D8) never compares records of different keys, so it needs no global order and gives the same answer for any iteration order. Files live at `reviews/<id>/<version>/<seq:03>-<keyhex8>.json`; the name is a convenience and the fold reads `seq` from the body.

### D6. The cache is a verified copy; a project never needs the source

(Details in D9.) The decision here: **applying an item copies its manifest and assets into the project's store** (`library.md` D5), so once applied, the project builds and opens without the source, on any machine, offline or after the team folder is gone. The source is needed to apply something *new*, to update, and to see review state.

### D7. The team licence

**What it is.** `Roster.licence` is a `ContentHash` (team.md 6.2, `SetLicence`, owner-signed) naming a file `licence/<hash>.toml`:

```toml
schema    = 1
name      = "Acme Studio internal"
spdx      = "LicenseRef-acme-internal"   # or a real SPDX id the owner picks
commercial = "ok"                         # ok | no | unknown, as io-api D10
attribution = "Acme Studio"
grant     = "licensed"                    # licensed | owned
outside   = "as_licensed"                 # as_licensed | blocked
terms_url = ""                            # optional
```

**Who chooses the text:** the team's **owners**, by `SetLicence`. Library items cannot override it upward: the owner owns what the team will vouch for.

**What `rights` an item carries in a team scope.** The item's declared `rights` joined **strictest-first** with the team licence (the same join D8 uses for assets), recomputed at apply time and written into the application record (D5/D8), together with the licence hash. So:
- `commercial` is the stricter of the item's and the team's; `attribution` lists both when they differ;
- `owned` follows `grant`: `licensed` means the applier does not own the part (rows do not carry `owned`); `owned` means the team assigns the result to whoever applies, and the row's `asserted_by` is "team <name> licence <hash8>" (the team's owners made that statement by signing the log; no one else asserts it);
- `outside = blocked` writes `team_only: true` on the record's rights;
- `private_test` items are never accepted into a team source (D10).

**The gate.** `licence.derive` already joins records as well as rows (D8), so `commercial: no|unknown` needs no new rule. The one additive rule is `team_only`: `io.check_export` for a `publish` target fails a character whose records carry it, like `private_test`. **"Outside the team" is any export whose target is `publish`**; sharing a project with teammates is not an export and is never gated. Because the record carries the effective rights and the licence hash, the answer does not depend on the source being reachable (D6). `team_only` is an additive field on `ItemRights` and the record (a contract change, human lane, task T6 in §5.4).

**Later changes.** A new `SetLicence` affects *future* applies. An existing record keeps the rights it was applied with, as the lock principle says; the Library panel marks records whose licence hash is older than the roster's current one ("licence has changed since you applied this"). A team cannot recall silently (§6, Q6: decided).

### D8. Review state

`ReviewState { Draft, InReview, Approved { by: Vec<KeyId> }, Deprecated }`, computed per `id@version@hash` by a pure fold over its records, never read from the index:

| State | When |
|---|---|
| `Draft` | published, no `Request` from its publisher yet, or the publisher's standing is `Withdraw` (visible to designers and above; hidden from others' search) |
| `InReview` | the publisher's standing is `Request`, and fewer than `library.approvals` valid approvals |
| `Approved { by }` | at least `LibraryPolicy.approvals` approvals from distinct keys that are eligible approvers (reviewer, admin, owner) **both at the record's own `roster` and at the roster head** (both read from the `RosterHistory` that `review_state` takes, §5.1) (so a viewer's pre-signed `Approve` does not start counting if that key is promoted later; the key must sign a new record, with a higher `seq`, after the promotion), not revoked, and **not the publisher of that hash** ("N other people", VC-11), with no eligible key whose standing is `Reject`, and no `Deprecate` |
| `Deprecated` | a valid `Deprecate`: its signer is the publisher of that hash or an admin or owner at the record's own `roster`, and not revoked at the head. Checked first, outside per-key standing |

Rules: apart from `Deprecate`, the fold uses each key's standing only (D5), so an `Approve` counts only while it is that key's latest record, and a key changes its mind by signing a higher `seq`. An approval by a key that is later revoked stops counting. A `Reject` standing from an eligible key puts the state back to `InReview` until that key signs a `Withdraw` or a new `Approve`. `Approved` needs the hash, so a new version starts again at `Draft`. `Deprecated` is terminal: a valid `Deprecate` is exempt from standing and from equivocation, and no later record, from any key, revives the version. Review records are authenticated one by one, so one forged record ignored does not hide the real ones.

**Policy** is `ReviewPolicy.library { approvals: u8, apply_only_approved: bool }` from the roster (team-review.md §8.1). With `apply_only_approved`, `library.apply` and `library.update` on a team item that is not `Approved` return a located refusal (`NotApproved`, with the state and what is missing) for **everyone, agents included**; the check lives in the op, not only in search. `library.search {usable_for}` gains the value `"apply"`, which also hides items the policy would refuse, so the list a person sees matches what will work; `ItemInfo` gains a `review` badge. With the policy off, unapproved items apply with a visible badge. The policy never applies to "my library" or first-party scopes.

### D9. Sync, cache and size limits

- **Where.** `<data dir>/omuzaki/library/teams/<team-id>/` holds `source.toml` (kind, location, a secret-store label for bucket credentials), `state.toml` (pinned root, highest `seq` and snapshot hash, last sync time), the verified `log/`, `index/` head, `reviews/` and `items/` copies in D1's layout. Assets go into the person's content-addressed `store/`, shared with everything else (dedupe, library.md D11).
- **`library.team.sync {team}`** pulls the log, the index snapshots not yet held (in pages, resumable: a sync that stops at a limit keeps what it verified) and the review records, verifies them (D3), and replaces the cached copies only if everything passed, atomically. Items and assets are **fetched on demand**: describing or applying fetches and verifies that manifest and its assets, nothing else. A "make available offline" action fetches a whole section.
- **When.** Manual by default; a Machine setting `library.team.sync_on_open` (default off) syncs when the app opens or the source is reached. Never in the background without it, and never for an agent session that has no `net` or `fs` grant (a folder needs only a file root under ADR 0013).
- **Offline.** Everything cached and verified keeps working; the panel says "last synced <time>, seq N". A failed or invalid sync keeps the previous good state and reports why.
- **Limits** (proposals, set as constants in the API crate and tested): index at most 20 000 entries and 8 MiB per snapshot, 64 snapshots and 16 MiB per `IndexPage`, 16 heads, and per sync at most 4 096 snapshots and 1 GiB (the client keeps only the heads' entries and each snapshot's `seq` and `prev` while streaming); the log at most 10 000 entries and 16 MiB; at most 1 000 review records and 4 MiB per `id@version`; manifest at most the library's manifest limit; asset at most 256 MiB each; a record at most 16 KiB; a cache budget `library.team.cache_mb` (default 2 048) with least-recently-used eviction of unreferenced assets only. Everything is checked before allocation, as `omuzaki-gltf` does. Stores stop reading at the limit plus one, so an untrusted store cannot make a call allocate more. Over a limit is `SourceError::TooLarge`, not a truncated read.
- **After apply** the project holds what it pinned (D6). Removing a team from the app deletes the cache and nothing in any project.

### D10. Ops and agents

| Op | Kind and scope | Agent | Person |
|---|---|---|---|
| `library.team.list {team?}` | query, `read` | yes | yes |
| `library.team.sync {team}` | job, `fs`/`net` by store | yes, within its grants | yes |
| `library.team.add_source {kind, location}` / `remove_source {team}` | edit, `person_only` (`add_source` also the store scope, to read the log it pins) | no | yes (shows the root fingerprint, then `team.pin_root`) |
| `library.publish {item, to: team}` | job, `publish` + `edit:library` + the store scope, `person_only` | `needs_person` | yes |
| `library.team.review {item, version, decision, note?}` | edit, `review`, `person_only` | no | authorised by `decision`: `Request` and `Withdraw` by the item's publisher (designer and above); `Approve` and `Reject` by reviewer and above, never the publisher of that hash; needs the store's `fs`/`net` scope like `sync`. The scope is the person's machine grant for the source they added; the role only decides what they may sign |
| `library.team.deprecate {item, version, reason}` | edit, `person_only` + the store's `fs`/`net` scope like `review` | no | yes (own, or admin; the signer-role check is separate from the scope) |

**Scope on every op:** an op that reads or writes a store (an uncached fetch for `library.apply` too) needs the store's `fs`/`net` scope on top of its own; roles decide what a person may sign, scopes what the machine may reach. **Agents propose, people decide** (library.md D9). An agent prepares candidates with `library.save_as_item` and may describe, search, sync and apply team items; it never signs, publishes, approves or deprecates. Signing needs the person's key, which no agent op can reach (identity D8), so the refusal is structural, not a policy.

**`library.publish` preflight** (a dry-run first, one finding list, nothing leaves the machine until the person confirms):
1. the manifest passes `ItemManifest::check` and has all the rights fields;
2. `rights` has no `private_test`, and no asset's provenance is `private_test`; previews are rendered from the item's recipe on the kit, so no user character is in them (D12 of `library.md`; the preflight checks the preview provenance too);
3. the id begins with the team slug and the version is greater than any published, with the bump kind matching the diff against the latest (D16: a removed param under a patch is refused);
4. the person's identity slot has a key that the roster allows to put, and the roster is fresh (a sync happened within the session or the preflight syncs first, with the person's consent);
5. the signature is made by `Signer::sign(Domain::LibraryItem, hash)` and verified before upload;
6. the effective rights under the team licence (D7) are shown, with "this will block publishing outside the team" when `team_only`.

Then `put` (manifest, assets by hash, then the new index snapshot). The ordering matters: assets first, manifest second, index last, so a reader never sees an entry whose files are missing; a crash leaves unlisted files that the next publish of the same bytes completes.

### D11. The marketplace is the same protocol with a hosted store

LB-08's client is `MarketStore: ItemStore`, an HTTPS implementation. Everything above holds unchanged: bytes untrusted, verification in the client, immutable versions, deprecation instead of deletion, signed review records. What differs:

| Concern | Client-only (specified here) | Hosted (the owner's marketplace decisions, §6) |
|---|---|---|
| Listing metadata | `IndexEntry.listing: Option<Listing>`: `{ creator: KeyId, title, summary, licence_template: ContentHash, commerce: Option<Opaque> }`; the client displays and verifies what it can | what `commerce` holds, if anything |
| Trust | there is no roster; the creator key is shown through identity's trust list: `Valid { trust: Unknown }` offers "Trust this key…" (identity D4); items install into **My library** by `library.install {from}`, person-only with the acknowledgement (D9 of `library.md`) | whether a hosted key attests creators |
| Free items | install = fetch, verify `LibraryItem` signature, run the licence gate on the item's rights, copy; no entitlement | — |
| Entitlements | `Signed<Entitlement>` under `Domain::Entitlement` (reserved by identity) verified against a pinned hosted key; install requires it only when `commerce` says so | the key, issuance, receipts, refunds |
| Reviews and ratings by users | a `Listing` slot to display them; not signed by the client | storing, abuse handling |
| Moderation and takedown | the client honours a hosted-key-signed `Deprecate { reason: takedown }` like any deprecation | policy, who signs, appeals |

Nothing here sets what is sold, for how much, or whether anything is. The point of the table is that the client's trust code is written once for both cases and a `commerce` value changes no verification rule. The hosted backend, payments and pricing are open questions for the owner (§6, Q1 to Q3). Until they are decided `MarketStore` is not built; whether it shares a base store contract with the team-only methods (log, licence, roster) is decided when its design is written.

### D12. Privacy

- **No telemetry.** The client contacts only a source the person added. Nothing is sent to us.
- **Nothing uploads except on `library.publish`**, which names what it sends in the preflight and needs a person. A sync is a download.
- **Previews contain no private test material** (D10, check 2). Candidate previews from a user's character are never uploaded; previews are kit renders.
- **Credentials** live in the secret store; the source file holds a label, never a key (CN-05). Logs and op records name the source and the item, never the credential.
- **A teammate sees what is published**, including the signer's name and key. Identity slots (identity D2) let a person publish under a client-specific key.

### D13. Threats the design answers

| Attacker | Does | Result |
|---|---|---|
| storage | swaps a manifest or asset | hash mismatch: `Tampered` |
| storage | serves an older index | `Rollback`, keeps last good state |
| storage | withholds | not detectable on a first sync; shown as last-synced and `seq` |
| storage | forges a review or roster entry | signature fails or the signer lacks the role |
| storage / teammate | replays a record on another version, team or tag | the body binds team, id, version, hash; the domain tag differs |
| removed member | publishes with their old key | the key lacks put at the snapshot's roster |
| revoked key | any signature | rejected everywhere |
| member with put | publishes a poor or malicious item | review policy (D8); the item is data, never code (library.md D1) |
| two writers | race | create-if-absent and a fork status (D2, D4) |

Items are recipes of ops, never code (library.md D1), so the worst a trusted-but-wrong item does is make an undoable document change that the apply preview shows.

### D14. Tests (no network)

- **`FakeStore`** (`omuzaki-library-source-mock`): in-memory, with switches for fail-after-N writes, serve-stale, drop, corrupt-bytes and a hook run before each create-if-absent (to race a second writer).
- **The store conformance suite** (`omuzaki_library_source_mock::conformance::<S: ItemStore>(factory)`): put/fetch round trip, create-if-absent and same-bytes idempotence, `Exists` on different bytes, `NotFound`, `TooLarge`, a missing asset, listing after a crashed publish (no index entry), concurrent index snapshots (fork), reviews round trip. Runs against `FakeStore`, the folder store (in a temp dir), and later the bucket store against an in-process fake object client.
- **The tamper matrix**, a table-driven test over the worked example: every row mutates one byte or one field and names the expected outcome:

| Mutation | Expected |
|---|---|
| index entry `hash`, `roster`, `seq`, `signer`, an entry added or removed | `Tampered(IndexSignature)` |
| index signed by a viewer or any key with no write right (a reviewer may write: approve is a write right, D3) | `NotAllowed` |
| store no longer lists a pinned head / a valid snapshot below the pin, not an ancestor of a pinned head | `Rollback`, cached state kept / `Forked`, a second head |
| child omits a parent's entry / `seq` not above a `prev`'s / over a D9 limit | `Tampered(DroppedEntry)` / `Tampered(Seq)` / `TooLarge` |
| log entry edited, reordered, root changed | `Invalid` with team.md's reasons |
| manifest byte flipped / id outside the team slug / signature of another item | `Tampered(Content)` / `BadId` / `Tampered(ItemSignature)` |
| asset replaced or truncated | `Tampered(Asset)` |
| item signed by a removed member (valid at `added_at`) / a revoked key / entry backdated with a removed key | accepted / `RevokedKey` / `BackdatedEntry` |
| review record for another hash/version/team, replayed | `Tampered(Record)` |
| approval signed by a viewer, key promoted later (`review_state` given the history) | not counted until re-signed at a roster where it is eligible |
| `Deprecate`, then a higher-`seq` `Request`/`Approve`/`Withdraw` / `deprecate` without the store scope | still `Deprecated` / refused before any I/O |
| manifest reformatted (same canonical hash) | accepted |
| review by the publisher, a viewer, a revoked key / two approvals by one key | not counted / counted once |
| second manifest for one `id@version` | `Conflict`, item ignored, others usable |
| two snapshots at one seq | `Forked`, merge snapshot resolves |
| entry for a `private_test` item | refused at preflight; a signed one that arrives anyway is shown with a warning, never applied |

- **Gate tests** (in the team-licence task): the three licence cases of §3, `commercial: no`, and `team_only`.
- **Determinism:** snapshots, records and the worked example are canonical JSON with signed ed25519 (deterministic), so a golden test compares bytes.

## 5. The contract and the task cut

### 5.1 The contract (names are final; doc comments belong to the implementer)

**Settled: a new crate, `omuzaki-library-source-api`**, not an addition to `omuzaki-library-api`. Reasons: the source types need `omuzaki-identity-api` and `omuzaki-team-api` (`Roster`, `Role`, `Signed`), which the library contract does not and should not depend on; the library contract is already a large hub that many crates depend on; and the verification module should compile for `wasm32` (a web player can then check a shared source) without pulling in the library's recipe code. `omuzaki-library-api` changes only additively and later (D7's `team_only`, `ItemInfo.review`, `TeamId` in `ItemScope` already exists).

```rust
pub trait ItemStore: Send + Sync {                       // bytes in, bytes out: never decides
    fn index(&self, have: &[ContentHash], cursor: Option<Cursor>) -> Result<IndexPage, SourceError>;  // one page of snapshots not in `have`, by (seq, hash8); the client computes heads from `prev`
    fn fetch(&self, id: &ItemId, version: &Version) -> Result<Vec<u8>, SourceError>;   // the manifest bytes
    fn fetch_asset(&self, hash: &ContentHash) -> Result<Vec<u8>, SourceError>;         // assets by hash
    fn put(&self, item: &SignedItem) -> Result<PutReceipt, SourceError>;              // create-if-absent, manifest then assets
    fn put_index(&self, index: &SignedIndex) -> Result<(), SourceError>;               // index/<seq>-<hash8>.json, create-if-absent
    fn reviews(&self, id: &ItemId, version: &Version) -> Result<Vec<Signed<ReviewBody>>, SourceError>;
    fn put_review(&self, record: &Signed<ReviewBody>) -> Result<(), SourceError>;
    fn log(&self) -> Result<Vec<Signed<Statement>>, SourceError>;                    // team.md 6.2
    fn put_log(&self, entry: &Signed<Statement>) -> Result<(), SourceError>;         // for the owner's client
    fn licence(&self, hash: &ContentHash) -> Result<Vec<u8>, SourceError>;
    fn put_licence(&self, hash: &ContentHash, text: &[u8]) -> Result<(), SourceError>;   // licence/<hash>.toml, create-if-absent, for the owner's client
}
pub struct IndexPage { pub entries: Vec<SignedIndex>, pub next: Option<Cursor>, pub missing: Vec<ContentHash> }   // a page is inside D9's limits; `missing` is the part of `have` the store no longer lists
pub enum ReviewState { Draft, InReview, Approved { by: Vec<KeyId> }, Deprecated }
pub struct SignedIndex { pub team: TeamId, pub seq: u64, pub prev: Vec<ContentHash>, pub items: Vec<IndexEntry>,
                         pub roster: ContentHash, pub signer: Identity, pub signature: Signature }   // Domain::LibraryIndex
pub struct IndexEntry { pub id: ItemId, pub version: Version, pub hash: ContentHash, pub publisher: KeyId, pub added_at: u64,
                        pub assets: Vec<(ContentHash, u64)>, pub review: ReviewState, pub reviews: Vec<ContentHash>,
                        pub listing: Option<Listing> }
pub struct ReviewBody { pub team: TeamId, pub item: ItemId, pub version: Version, pub hash: ContentHash, pub roster: ContentHash, pub seq: u32,
                        pub decision: ReviewDecision, pub note: Option<String> }
pub enum ReviewDecision { Request, Withdraw, Approve, Reject, Deprecate { reason: DeprecateReason } }
pub struct SignedItem { pub manifest: Vec<u8>, pub assets: Vec<(ContentHash, Vec<u8>)> }   // the manifest carries its signature
pub enum SourceError { NotFound, Exists, Forbidden, Conflict { id: ItemId, version: Version }, Unavailable(String),
                       TooLarge { what: String, limit: u64 }, Tampered(Tamper), Rollback { pinned: u64, served: u64 },
                       Forked, NotAllowed(String), Malformed(String) }
pub struct VerifiedSource { /* log fold, index head, per-item verified view */ }
pub fn verify_source(store: &dyn ItemStore, pin: &SourcePin, trust: &dyn TrustStore) -> Result<VerifiedSource, SourceError>;
pub struct RosterHistory { /* the verified log fold: the Roster at every entry hash, and the head */ }
pub fn review_state(history: &RosterHistory, records: &[Signed<ReviewBody>], entry: &IndexEntry) -> ReviewState;   // the D8 fold, pure
pub fn effective_rights(item: &ItemRights, licence: &TeamLicence) -> ItemRights;   // D7 join, pure
pub struct TeamLicence { /* schema, name, spdx, commercial, attribution, grant, outside, terms_url */ }
pub struct Listing { pub creator: KeyId, pub title: String, pub summary: String, pub licence_template: ContentHash,
                     pub commerce: Option<serde_json::Value> }                 // opaque until the owner decides (D11)
```

Notes: `index(have, cursor)` pages through the snapshots the client has not yet verified (by file name, `<seq>-<hash8>`, so known ones are not read; `Cursor` is opaque to the client), not a chosen head and not filtered by `seq`: a branch that arrives later with a lower `seq` than the pin is still returned, so a fork is always visible, and a source with any number of updates first-syncs in bounded pages. `state.toml` keeps the hashes of verified snapshots. `reviews` and `log` stop at their D9 limits. `put_licence` rejects bytes whose hash is not `hash`. `ItemStore` is `Send + Sync` and synchronous like `Store`; the job wrapper does the waiting. There is no `delete`. `SourcePin { team, root: KeyId, log: (u64, ContentHash), heads: Vec<(u64, ContentHash)> }` is the local `state.toml`.

### 5.2 Crates

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-library-source-api` | API | §5.1, the limits, `verify_source`, `review_state`, `effective_rights`; depends on `omuzaki-library-api`, `omuzaki-identity-api`, `omuzaki-team-api`, `omuzaki-store-api`, serde; wasm-clean |
| `omuzaki-library-source-mock` | Mock | `FakeStore`, the conformance suite, the worked-example builder |
| `omuzaki-library-source` | Implementation | `FolderStore`, `SourceCache`, the `library.team.*` ops and the publish job; the bucket store goes in `omuzaki-library-source-s3` (own crate, so the network dependency stays out of this one) |

### 5.3 What changes elsewhere

`library.md` D9 and D11 get one-line pointers to this page (in this PR). `omuzaki-library-api` (additive): `ItemInfo.review`, `search.usable_for: "apply"`, and `ItemRights.team_only` with the record field. `omuzaki-team-api`: nothing new if team.md's contract lands as written (`Roster.licence`, `LibraryPolicy`, `library_rights`). `docs/design/identity.md` and `team.md` still call this design "0046" or "planned"; they should say `0057`.

### 5.4 The task cut (file when the design is accepted)

Every task below is docs-complete here: a fence, its contract, its acceptance numbers and a `Blocked by` line. Models are the lead's estimates from the issue.

| # | Task | Model, lane | Fence | Acceptance | Blocked by |
|---|---|---|---|---|---|
| T1 | `omuzaki-library-source-api` (types, limits, `verify_source`, `review_state`, `effective_rights`) and `omuzaki-library-source-mock` (`FakeStore`, conformance suite) | sonnet, human lane | the two new crates, `crates/README.md` | the tamper matrix of D14 passes (every row has a test); the D8 fold has tests for each state transition, for approver rules (an approval signed before promotion does not count) and for terminal `Deprecate`; a source with 1 000 index updates first-syncs in pages inside D9's limits; a late lower-`seq` fork becomes a second head; `FakeStore` passes the conformance suite; wasm32 check green | #1053 (`omuzaki-identity-api`), the team contract (`omuzaki-team-api`) |
| T2 | the folder store and `SourceCache` | sonnet | `omuzaki-library-source` | the conformance suite green on a temp dir; a crash after the manifest and before the index leaves no listed broken entry; a conflicted-copy file is a `Forked` status, not a failure; sync atomically replaces or keeps; no network | T1 |
| T3 | signed index writer and review record writer | sonnet | `omuzaki-library-source` | the §3 example is generated from test seeds and matches goldens byte for byte; draft → in review → approved with two reviewers; the publisher's own approval does not count; a merge snapshot resolves a fork | T1, T2 |
| T4 | `library.publish` to a team, preflight, and `library.team.review`/`deprecate` | sonnet | `omuzaki-library-source` (op registrations), `omuzaki-library-ops` | all six preflight checks have a failing case; `review` and `deprecate` refused without the store scope; under-bumped version refused; `private_test` refused; agent call returns `needs_person`; one op record | T3, the library ops crate |
| T5 | `library.team.sync`/`list`/`add_source`/`remove_source` ops | haiku | `omuzaki-library-source` | `add_source` shows the root fingerprint and needs `team.pin_root`; sync offline reports the cache age; limits tested at and over | T2 |
| T6 | team licence in the gate (`team_only`, `effective_rights`, record fields, `licence.derive`) | sonnet, human lane | `omuzaki-library-api` (additive), `omuzaki-io-ops` (`licence.derive`, `io.check_export`) | the three §3 gate answers as tests (`ok`, `commercial: no`, `team_only`); personal use never blocked; a project opens with the source gone | T1; the Director's approval of the additive contract change on its issue |
| T7 | bucket store against a fake object client | sonnet | `omuzaki-library-source-s3` | the same conformance suite and tamper matrix green with no network; create-if-absent maps to `If-None-Match`; credentials only from the secret store | T1, T2 |
| T8 | Library panel Team tab (sources, review badges, approve/reject, sync status, licence change marker) | sonnet, visual | `omuzaki-ui` library panel | rendered at the three themes, review states and the five `Check` outcomes shown as identity D4 words; keyboard and AccessKit labels | T4, T5, T6 |

The marketplace store, entitlements and any hosted piece are not in this cut: they wait for the owner's decisions.

## 6. Open questions

Q1 to Q3 are the owner's and stay open. Q4 to Q9 were ruled by the Runtime lead on #1534 and are decisions now (also recorded in ADR 0057).

### Open (the owner's; options, no recommendation)

**Q1. Does Omuzaki run a hosted marketplace backend, and who operates it?** Options: (a) none: sharing stays team sources and self-hosted stores, and the marketplace client points at a third party; (b) the maintainers operate one; (c) a partner operates one to a published protocol. The protocol is the same in all three (D11). Any task that needs a hosted backend is blocked on this decision.

**Q2. Payments and pricing: whether the marketplace has paid items, who handles money, how receipts are issued.** Options: (a) free items only; (b) paid items through an outside payment provider with the hosted key issuing `Entitlement`s; (c) another model the owner defines. The client reserves `Listing.commerce` and `Domain::Entitlement` and verifies nothing about money.

**Q3. Moderation and takedown policy for hosted items.** Options: (a) none, listings are the creator's responsibility; (b) the operator can deprecate by signed record (D11); (c) a report-and-review process. Who signs takedowns and what appeals exist are the owner's.

### Decided

**Q4. Drafts are immutable.** Every published version is immutable (D4); editing a draft is a new patch version. A mutable `drafts/` area is not part of this design.

**Q5. Deprecate, and the storage owner removes files.** The protocol has no delete and no purge. Files removed by hand show as `Unavailable`, not `Tampered`, for entries already deprecated. A signed `Purge` record that tells clients to drop cache copies is a later addition with its own security review.

**Q6. No silent recall.** An applied record keeps the rights it had (D7). A tightened licence shows the "licence changed" marker, and the person runs `library.update`. There is no `recall` flag.

**Q7. Storage access controls.** The design protects integrity, not confidentiality; whoever can read the shared place reads the items. Per-team content keys need a key-agreement design first, which is not started.

**Q8. Sync on open is off everywhere,** with a one-click toggle on the Team tab (D9).

**Q9. The git store's shape is decided when T7 is done.** The layout already works as files, so the protocol is the same either way (one repository per team, or the team folder inside a project repository).

**Crate placement: accepted.** The new `omuzaki-library-source-api` (§5.1). The task that adds it (T1) touches the root `Cargo.toml`, which needs the Director's hub sign-off on that PR.

## 7. Sources

Read on 2026-10-15: `docs/design/library.md` (D2, D5, D8, D9, D11, D16, §8), `docs/design/identity.md` (D1 to D5, D10), `docs/design/team.md` (§3, §6, §7, §11.2, §16), `docs/design/team-review.md` (§8.1), `docs/decisions/0042-identity.md`, `0043-team.md`, `0019-library-items.md`, `crates/README.md`, issue #1052.
