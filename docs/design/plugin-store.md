---
title: "Plugin store: the package, the signed index, revocation, install and update (PL-04)"
summary: "the plugin store (ADR 0065): prior art, the signed package, a store-signed index with replay and freeze defences, a separate revocation list, install, update, pin and rollback ops with the permission diff, the store licence rule, a review service's interface, privacy, the tests and the task cut."
area: design
---

# Plugin store: the package, the signed index, revocation, install and update (PL-04)

**Status:** proposed. **Feature:** PL-04. **Decision record:** [ADR 0065](../decisions/0065-plugin-store.md). **Spec:** v3 §30
(PL-04), §28 (LB-08), §31 (SE-04, SE-07), §6. **Builds on:** [ADR 0058](../decisions/0058-plugins.md) and
[`plugins.md`](plugins.md) (the host, the package of §7, the ceiling of §3.2, revocation of §3.4, the ops of §9),
[ADR 0057](../decisions/0057-library-sharing.md) and [`library-sharing.md`](library-sharing.md) (a bag of signed
files the client verifies), [`identity.md`](identity.md) (ADR 0042: `Signed<T>`, `Domain`, `Signer`, `TrustStore`,
`Check`; read at the head of PR #1544, `9de492b`) and [`updates.md`](updates.md) §4 (epoch, sequence, expiry).
**Out of scope:** hosting, a reviews service's operation, money and business terms (the owner's; §14), and the
extension points (#1083, ADR 0064) and bridges (#1085, ADR 0066), which this design does not touch.

## 1. The decisions in brief

1. **Two signers.** The publisher signs the package (`Domain::Plugin`); the store signs the index and the
   revocation list (two new domains). Both use the identity contract's `Signed<T>`; no second scheme (§3).
2. **The package is `plugins.md` §7 unchanged;** the signed body adds id and version so a signature cannot be
   relabelled (§4).
3. **The index is one signed file** with a pinned `(epoch, sequence)`, an expiry and immutable versions. Its display
   data is never trusted: install recomputes it from the package (§5).
4. **Revocation is a second small signed file.** `yanked` stops new installs; revoked disables what is installed (§6).
5. **Install and update are person-only jobs that show the permission summary or the diff first;** an update that asks
   for more keeps the old version running until accepted; one version is kept for rollback (§7).
6. **Nothing is fetched until the person adds a store;** no installed list, id or account is ever sent (§10).
7. **Licence refusal in the store and the client (§8); "Not reviewed" without a service, signed records with one (§9).**
8. **Three new crates (§11) and nine tasks (§15).** A store is files: a folder, a static host or a bucket.

## 2. Prior art

Read on 2026-10-15: the python-tuf, Open VSX and Flathub website repositories for their licences; the TUF
specification, RFC 2789 and the Cargo index reference for their formats. The other rows are from prior knowledge of
the projects' documentation and were not re-read for this design.

| Project | Licence | What it gets right | Where ours must do better | What we take |
|---|---|---|---|---|
| **Open VSX** (Eclipse; a VS Code extension registry) | EPL-2.0 | An open, self-hostable registry; namespaces whose owners are verified; each registry signs what it serves and `ovsx` can verify against a key | The registry's own key is the only signature: a compromised registry can ship a package. Extensions run with full Node access, and an installed extension is not disabled when a registry pulls it | Namespace ownership as a key, not a claim in a public issue (§3); we add the publisher's own signature, a revocation that bites at once and a permission diff |
| **TUF** (The Update Framework; python-tuf is the reference) | spec: not checked†; python-tuf: MIT or Apache-2.0 | The threats named and defended: rollback, freeze (expiry), mix-and-match, compromised online keys; a client that verifies in a fixed order | Four roles with thresholds and delegations are heavy for a store run by a few people; it knows nothing of permissions, licences or what a person sees | Replay, freeze and key-rotation rules (§5, §6) and the "verify the metadata, then the target, in order" flow (§7), without the role machinery: the publisher signature plays the targets role |
| **Sigstore** (cosign, Fulcio, Rekor) | Apache-2.0 | Signatures that outlive a key through a public transparency log; no key custody for the signer | Keyless signing needs an OIDC account and online services at sign and verify time; the issue demands no account to install and offline use | Nothing in the base design. `Signed<T>` leaves room for a later log (a store that publishes an append-only log of its snapshots), listed as an open question (§14) |
| **crates.io sparse index** (Cargo, RFC 2789) | Cargo: MIT or Apache-2.0 | Plain files over HTTP with conditional GET; `cksum` and `yanked` per version; versions immutable; a yanked version still resolves for existing lockfiles | The index is not signed (TLS and the registry are trusted) and yank is advice only: nothing disables an installed crate | The listing shape, immutability and the yank semantics (§5); we sign it and add a revocation list that does disable |
| **Flathub / Flatpak** | Flatpak: LGPL-2.1-or-later†; the Flathub website: Apache-2.0 | Apps declare sandbox permissions that are shown to the person; verified-publisher badges; the repository is signed, so mirrors need no trust; permission increases are surfaced at update† | Verification is by domain or forge account, not a key the person can pin; no per-build revocation | Show permissions before install and the difference at update (§7); we bind approvals to a hash and key |
| **VS Code Marketplace** | proprietary service | One place to browse; a malware scan on publish† | Closed: its terms bar other products from using it; no offline use; no permission model | Contrast only |

† Not re-read for this design; confirm before relying on it.

**What ours must do better, in one list** (the design is judged on it): (1) a store compromise ships no code, because
every package carries the publisher's signature and a changed publisher key needs the person's confirmation; (2) a
mirror or an old copy cannot roll the listing back or freeze it (watermark, expiry), and says so visibly; (3) a
revocation disables a running plugin and cancels its requests at once, offline-safe and with no telemetry; (4) the
person sees what a plugin may do before it is installed and exactly what changes at an update; (5) nothing is sent
that identifies the person or their plugins; (6) a store is just files, so a studio, a school or the maintainers can
run one.

## 3. Roles and keys

| Role | Key | Signs | Domain (tag) | Where the person pins it |
|---|---|---|---|---|
| Publisher | the author's ed25519 key (ADR 0042) | `plugin.sig`: `Signed<PackageStatement>` | `Domain::Plugin` (`omuzaki-plugin-v1\0`, ADR 0058) | `identity.trusted` (identity D4), by explicit confirmation |
| Store | one key per store, offline when possible | `index.json` and `revoked.json`: `Signed<IndexBody>`, `Signed<RevocationBody>` | `Domain::PluginIndex` (`omuzaki-plugin-index-v1\0`), `Domain::PluginRevocation` (`omuzaki-plugin-revocation-v1\0`) | `plugin.stores`, with the store's URL, when the person adds it |
| Review service (later) | its own key, listed in the index | `Signed<ReviewStatement>` | `Domain::Approval` (identity D10) | named in the index; the client checks the list (§9) |

- **Why two new domains and not `Domain::LibraryIndex`:** a signature for one use must fail under every other
  (identity D3). An index signed for the library must never verify as a plugin index. The two tags join
  `Domain::Plugin` in one `omuzaki-identity-api` task (T1), `non_exhaustive` as the enum already is, with the tag
  tests of ADR 0042. This is an ADR 0042 amendment like `Domain::Plugin` (Director sign-off, ADR 0065).
- **A name is never trusted without its key.** The index says `Example Studio`; the card shows the person's own
  label for the key, or the fingerprint and "unknown publisher" (identity D4). **An update must be signed by the key
  that signed the installed version.** A different key, even a valid one the store lists, is shown as "Publisher
  changed" and needs a typed confirmation; a `Rotate` statement from the old key (identity D5, once its types
  exist) makes it a one-click "the publisher rotated their key".
- **The store key is not a trust anchor for code.** It decides what is listed; it cannot make a package acceptable.
  Rotating it is a `Rotate` statement signed by the old key, or the person re-adds the store (T4).
- **Seeds and signing** go through `Signer` (the private key never leaves it). The pack tool (T7) asks the person's
  signer; the static store builder (T7) asks the store's, and signing is deterministic (ed25519), so a rebuilt
  index with the same inputs is byte-identical.

## 4. The package

`plugins.md` §7 already fixes the container (a folder or `.zip`), the hash and the extraction refusals; the store
adds nothing to them, so the install task's package work needs no re-cut. Restated with the limits that matter here:

- **Container: `.zip`** (the format `plugins.md` §7 and the install ops already read). A tar adds nothing: the hash is
  over staged files, not archive bytes, so zip's non-determinism does not matter, and zip has one central directory
  to check before any entry is read.
- **Limits:** 16 MiB compressed (the index states the size, so the client refuses early), 32 MiB and 256 entries
  uncompressed, `plugin.toml` at most 256 KiB, a per-entry expansion ratio of at most 100 to 1, no entry that is not
  a regular file (every refusal of §7, plus the ratio).
- **Hashed:** every regular file except `plugin.sig`, in `plugins.md` §7's exact preimage (sorted NFC `/` paths, each
  path and content `u64`-LE length-prefixed, BLAKE3). The manifest, the component and the resources (`l10n/`,
  `LICENSE`) are all inside, so the signature covers what loads and what is shown.
- **Signed:** `plugin.sig` is the JSON of `Signed<PackageStatement>` under `Domain::Plugin`:

```rust
pub struct PackageStatement { pub id: PluginId, pub version: Version, pub hash: ContentHash }
```

  Signing the hash alone (ADR 0058's wording) would let a store relabel a valid signed 0.1.0 as 0.2.0, or a plugin
  with another id. `Signed::hash` binds the body and the signer, so the statement binds id, version, hash and key.
  Install refuses unless the statement's id and version equal the manifest's and its hash equals the staged hash.
- **A required `LICENSE` file** in a store package (a local install does not need one): the licence text the credits
  line shows (§8) is the one inside the signed bytes.
- **Determinism:** the same files give the same hash on every platform (`package_hash_golden`, `plugins.md` §7); the
  same statement gives the same signature bytes. The zip itself may differ between builds and is never hashed.

```rust
pub struct PluginPackage {                       // omuzaki-plugin-store-api; built by parse, never by hand
    pub manifest: Manifest, pub component: Vec<u8>, pub resources: Vec<(String, Vec<u8>)>,
    pub hash: ContentHash,                       // recomputed from the staged files
    pub check: Check,                            // identity's Check on the statement; Unsigned when no plugin.sig
}
```

`check` replaces the outline's `publisher: PublisherCheck`: that type is the library's earlier shape, and identity's
`Check` carries the same facts plus `Trust`.

## 5. The index

**Where a store lives.** A store is a base URL and a pinned key, added by the person in Settings → Plugins →
Stores (`plugin.stores`: a list of `{ name, url, key, schedule }`, scope `Machine`, agent `Never`, profile
`Excluded`, **empty by default**). There is no built-in store. When the owner's hosting exists (§14) the Omuzaki store
is an entry the person adds with one button; the key is compiled in, the same custody as the updater's root. Layout:

```
<base>/v1/index.json            Signed<IndexBody>        <= 8 MiB, <= 10,000 entries
<base>/v1/revoked.json          Signed<RevocationBody>   <= 4 MiB, <= 50,000 entries (§6)
<base>/v1/packages/<id>-<version>.zip
<base>/v1/reviews/<hash>.json   Signed<ReviewStatement>  (optional, §9)
```

```rust
pub struct IndexBody { pub format: u32 /* 1 */, pub store: String, pub epoch: u32, pub sequence: u64,
                       pub issued: u64, pub expires: u64,           // unix seconds, UTC
                       pub reviewers: Vec<KeyId>, pub entries: Vec<IndexEntry> }
pub struct IndexEntry { pub id: PluginId, pub version: Version, pub hash: ContentHash, pub url: String /* relative */,
                        pub size: u64, pub publisher: Identity, pub licence: String /* SPDX */,
                        pub name: String, pub summary: String /* <= 200 B */, pub permissions: PermissionSummary,
                        pub min_app: Version, pub yanked: bool, pub review: Option<ContentHash> }
pub type SignedIndex = Signed<IndexBody>;
```

- **`SignedIndex` is `Signed<IndexBody>`,** not a bespoke struct with its own signature field: the signature sits
  outside the hash (identity D3), the key is bound with the body, and verification is `Signed::check(Domain::
  PluginIndex, …)` followed by "the signer is the key pinned in `plugin.stores`". The outline's `issued` and
  `signature` move into the body and the envelope.
- **One file, not one per plugin.** At 10,000 entries of about 600 bytes it is 6 MB, fetched with a conditional GET
  (`ETag`) so an unchanged index costs a 304; it verifies in one pass and gives one `(epoch, sequence)` to pin. A
  store that outgrows it adds sharding under a new `format` (a per-id file listed by hash), not a different trust model.
  A store keeps at most 20 versions per id in the list; older ones stay downloadable at their `url` for pins.
- **Versions are immutable.** A second entry for one `id@version` with another hash makes the whole index
  `Tampered(Duplicate)`; the same bytes are idempotent. A fix is a new version. Delete is `yanked: true`, never removal,
  so a pin or a rollback still resolves (crates.io's rule).
- **Replay and freeze are the updater's rules** (`updates.md` §4): the client stores the highest accepted
  `(epoch, sequence)` per store in the plugin state file and refuses a lower pair (`Replay`). The `expires` is 14
  days and a store re-signs daily. An expired index is "out of date" in the panel, never "no new plugins", and offers
  nothing new. An expired index does not stop installed plugins or pinned reinstalls from cache.
- **Display data is never trusted.** `name`, `summary`, `permissions` and `licence` exist so the panel can list
  without downloading. Install recomputes them from the signed package; a mismatch (a manifest asking for more than
  the listing showed, or another licence) refuses with `ListingMismatch` and names the field. So a store that lies in
  the browser cannot get a plugin installed under a lie.
- **`yanked` against revoked.** Yanked: the author or store says "do not choose this version"; hidden from the
  panel's default list, refused as an update target, still installable by exact hash or pin, nothing already installed
  changes. Revoked (§6): the store says "this must not run"; it disables installs. A revoked version is also yanked,
  but the client never relies on that.
- **`min_app`** hides entries the running app cannot run and says so ("needs Omuzaki 1.4"); it is a convenience, as
  the host's API window (plugins.md §1.1) is what actually refuses an incompatible component.

## 6. Revocation

```rust
pub struct RevocationBody { pub format: u32, pub epoch: u32, pub sequence: u64, pub issued: u64, pub expires: u64,
                            pub revoked: Vec<Revocation> }
pub struct Revocation { pub target: Target /* Hash(ContentHash) | Key(KeyId) */, pub class: RevokeClass,
                        pub note: String /* <= 200 B, plain text */ }
pub enum RevokeClass { Malware, Compromised, Vulnerable, Withdrawn, Policy }   // the first two are blocking
pub type RevocationList = Signed<RevocationBody>;
```

- **A separate file, by content hash.** It is small (an entry is about 150 bytes), fetched with the index and
  alone, and has its own sequence and expiry, so a fix to the list never waits on a re-signed listing. A target is a
  **package hash** (one build) or a **publisher key** (everything that key signed, identity D4's "no timestamps to
  tell old from new"). Content hashes, not names, so re-signing or renaming the same bytes does not escape it.
- **Fetch, with no telemetry.** One GET each of `revoked.json` (conditional) whenever the store is contacted for any
  reason; plus once a day while the store's `schedule` is `daily` (opt-in; default `manual`, which never fetches by
  itself). No query string, no cookie, no installed list, `User-Agent: omuzaki-store/1` (no app version). The store
  learns an IP address and that the file was asked for, and the help text for the setting says so.
- **Applying a list** (verified, `(epoch, sequence)` above the pin): the client intersects it with the installed
  hashes and publisher keys and with the cache, then for each match runs `plugins.md` §3.4 unchanged: `Disabled {
  reason: StoreRevoked { class } }`, the session ends `Revoked`, in-flight `fetch`es are cancelled, the entry's
  transaction rolls back. The same check runs on **every enable and every load at start-up** against the last
  accepted list, so a plugin revoked while the app was closed never starts.
- **The person is told why:** a badge on the plugin manager and a finding in the Theatre (never a modal, never during a
  live show) with the class text, the note, the store's name and the list's sequence: "Disabled: revoked by <store> as
  malware (list 42): <note>". Data and settings stay.
- **What a person may override.** `Malware` and `Compromised` are blocking: no override, only `plugin.remove`. The
  other classes disable by default and the person may confirm `plugin.keep {id, hash}` (person-only, `Undo::None`,
  confirmed, with the note shown): that hash runs again, the confirmation is stored against the hash and the list
  sequence, and a later list that names a *blocking* class for it ends the override. A development build of the same
  bytes does not escape a blocking revocation (same content hash); changing one file makes a different plugin.
- **A stale list is not an "all clear".** Past `expires` the panel shows "revocation list out of date (last checked
  N days ago)"; installed plugins keep running and nothing is disabled on a clock. A show never fails closed because a
  server is down; the cost is that a live-stream machine that never connects learns of a revocation late, which the
  `daily` schedule and the badge exist to reduce (§14, question 2).
- **Replay.** A lower `(epoch, sequence)` is refused. A raised `epoch` from a rotated store key resets the pin, as in
  `updates.md` §4. The pinned pair never goes down, so a replayed older list cannot un-revoke a hash.
- **Unrevoking.** A later list simply omits the entry; the plugin is re-enabled only by the person (the disabled
  state does not clear itself).

## 7. Install, update, pin, rollback

`plugin.install` of `plugins.md` §9 takes `{path, watch?}`. This design widens it to a source, and adds `plugin.update`,
`plugin.pin`, `plugin.rollback`, `plugin.keep` and the store ops. All mutating ones are person-only, never queued,
and agents cannot call them (ADR 0058 decision 8); `plugin.store.list` and `plugin.store.describe` are `read` queries
over the cached verified index and agents may call them.

| Op | Kind | What it does |
|---|---|---|
| `plugin.install {source}` | Job, `Undo::None`, confirmed (as 0058) | `source` is `Path { path, watch? }` or `Store { store, id, version?, hash? }`. The flow below; does not enable |
| `plugin.update {id}` | Job, `Undo::None`, confirmed | installs the newest non-yanked, higher-semver, non-revoked entry from the same store beside the installed one; the diff card decides |
| `plugin.pin {id, pinned}` | Edit, `Undo::Explicit` (sets a value; the undo is the other value) | a pinned plugin is never an update target and shows "pinned to <hash8>" |
| `plugin.rollback {id}` | Job, `Undo::None`, confirmed | re-activates the previous version kept on disk (swap and reload, as 0058's reload); refuses a revoked hash |
| `plugin.keep {id, hash}` | Edit, `Undo::None`, confirmed | the advisory-revocation override (§6) |
| `plugin.store.refresh {store}` | Job, `net` | fetches `index.json` and `revoked.json`, verifies, updates the cache and pins |
| `plugin.store.add / remove {…}` | Edit, `Undo::Explicit` | edits `plugin.stores` |

**The install flow** runs in `work` off the document lock (ADR 0036), with a `JobCtx::checkpoint` between steps, and
every step refuses before the next begins:

1. **Resolve.** Pick the entry in the cached verified index (refresh first if the person asked or it has expired).
   Refuse a revoked hash/key, and a yanked entry unless `hash` was given. With `hash`, the entry's hash must equal it.
2. **Download** `url` (resolved against the store base; https, or `http://localhost` for a dev store; no redirect
   followed; 120 s; at most `entry.size` ≤ 16 MiB bytes read) into a staging file.
3. **Stage** with `plugins.md` §7's refusals (zip-slip, links, duplicates, count, size) plus the ratio and
   `plugin.toml` limits above. Nothing is executed or compiled yet.
4. **Verify:** staged hash = `entry.hash`; `plugin.sig` present and `Check::Valid` under `Domain::Plugin`;
   `statement.{id,version,hash}` = manifest and staged; `signer.key` = `entry.publisher.key`; for an update, the
   same key as the installed version (or the `Rotate` path of §3).
5. **Policy:** the manifest is valid; no never-granted scope (plugins.md §3.2); licence rule (§8); listing matches
   package (§5); not revoked.
6. **Card** (only now). Fields: name, publisher label or fingerprint, "Not from Omuzaki" line, licence and credits,
   permission summary (scopes, each host on its own line, data quota, worlds), the review line (§9), size, store. For
   an unknown key: "Trust this key…" with the full fingerprint is a separate, required step. Decline: staging is
   deleted, nothing is registered.
7. **Commit:** copy into `plugins/<id>/<hash>/`, compile into the cache, register the record under the lock, as in
   `plugins.md` §6.3. A cancelled or capped job deletes its staging and registers nothing.

**A local install** (`Path`) runs steps 3 to 7 (no index, no entry): a signed package gets the same card; a package
signed by an unknown key needs the trust step; an **unsigned** package is refused unless `plugin.developer_mode` is
on (`Machine`, agent `Never`, default off, a one-time explanation when it is enabled). With it on, the card says
"Unsigned: no one vouches for this code", approvals cover the one hash (plugins.md §3.2), and a watch folder reloads
on a changed hash. This tightens ADR 0058 (which let any unsigned package install) so that a zip from a chat message
is not one click away; it is a plugin-area amendment flagged in ADR 0065. A package signed with the person's own dev
key (0058 decision 12) installs with a "development build" line and no developer mode.

**Update and the permission diff.** `PermissionDiff` = new requests against what the person approved for this key:
`added` (scopes, hosts, data on, a larger quota, a new world or command), `removed`, `unchanged`, and flags for
`licence_changed` and `publisher_changed`. The rules:

- Nothing added and the same publisher key: the card is a one-line confirm ("Update Wave 0.1.0 → 0.2.0").
- Anything added: the card shows **only the difference** (added lines marked, unchanged collapsed), plain-language
  per line, and no tick is pre-set. **The old version keeps running with its old approval until the person accepts**;
  declining leaves the plugin exactly as it was, and the update stays offered. Accepting installs, swaps with a
  reload, and `plugin.grant` is set to the new approval in the same step.
- A changed publisher key or licence is a blocking line of its own (typed confirmation for the key).
- An update may never be a lower semver than the installed version (the store cannot downgrade a person); only
  `plugin.rollback` goes back, and only to the one version kept.
- **No automatic updates.** The panel shows "update available" from the cached index; applying is the person's.
  Third-party code that acts unattended does not get silently replaced.

**Rollback** keeps exactly one previous `plugins/<id>/<hash>/` after an update (deleted when the next update commits
or the plugin is removed). It uses that version's recorded approval, still checks the revocation list, and says that
the plugin's data folder is not migrated back (the plugin is responsible for reading data written by a newer version).

## 8. The licence rule and credits

- **The store lists only an SPDX expression whose every id `docs/licensing/policy.md` allows,** with a `LICENSE` file
  in the package. A non-commercial, research-only or unknown licence is refused at listing (`listing::check`, T2, run
  by the static builder and any hosted store's CI). A person's own local install of such a plugin stays allowed and
  shown (`plugins.md` §8).
- **The client enforces it too:** a store entry that fails the check is refused with `StoreError::Licence`, so a lax
  store is not a route to a non-commercial plugin the app fetched. (The person can install the same files from a path:
  the app did not fetch or ship them.)
- **Credits** on the card and the plugin's page: "Wave 0.2.0 by Example Studio (key 3fa9…), MIT. From <store>. Not from
  Omuzaki: this plugin is made by someone else." with the packaged `LICENSE` text one tap away. The licence record
  (VF-08) keeps `plugin wave@0.2.0, MIT, <hash>` (`plugins.md` §8).

## 9. What a review service adds, and the minimum without it

**The minimum:** the store's build runs this design's automated checks (§4 limits and refusals, §5 immutability,
signature and key continuity, the licence rule, the never-granted scopes, and that the component's WIT imports agree
with `api`), refuses a publish on any failure, and the listing says **"Not reviewed"**. Nothing claims safety.

**What a hosted service adds** (out of the cloud lane): malware scanning, a human look at permissions against stated
purpose, abuse reports, takedowns, ratings. This design fixes only the interface, so the client can show its output
without trusting it:

```rust
pub struct ReviewStatement { pub id: PluginId, pub version: Version, pub hash: ContentHash,
                             pub outcome: ReviewOutcome /* Passed | Failed */, pub scope: ReviewScope /* Automated | Human */,
                             pub checks: Vec<String>, pub issued: u64 }
pub trait ReviewService { fn submit(&self, package: &ContentHash) -> Result<Ticket, ReviewError>;
                          fn status(&self, hash: &ContentHash) -> Result<Option<Signed<ReviewStatement>>, ReviewError>; }
```

A statement is `Signed<ReviewStatement>` under `Domain::Approval`, signed by a key in the index's `reviewers`, fetched
from `entry.review`; it binds id, version and hash and, with `deny_unknown_fields`, cannot parse as a library
`ReviewRecord` (identity D3). The client shows "Passed human review by <label>, <date>"; an unverifiable one is "Not
reviewed". A takedown is a revocation entry (§6), so the service needs no new channel into the client.

## 10. Privacy

- **No telemetry, no account.** The client downloads the whole index and diffs it locally, so the store learns only
  what a file server learns (IP, time, file). No cookie, identifier, app version or installed list.
- **Nothing at launch** (SE-04): no store, no request; with one, a request happens only when the person refreshes or
  installs, or on `daily`. Defaults: schedule `manual`, no auto-update, no review fetch until a plugin's page opens.
- **Offline works:** installed plugins and the cached index and list need no network; a failed fetch keeps the last
  good state and says why.

## 11. Contract and crates

| Crate | Layer | Holds |
|---|---|---|
| `omuzaki-plugin-store-api` (new) | API | the types of §4 to §9, `parse_index`/`parse_revocations`/`parse_package`, verification, `listing::check`, `diff`, the `PluginStore` and `ReviewService` traits; builds for wasm |
| `omuzaki-plugin-store-mock` (new) | mock | `MemoryStore`: a store served from memory with scripted faults, a hostile-package builder |
| `omuzaki-plugin-store` (new) | implementation | the client over `omuzaki-http-api`: `plugin.stores`, cache, watermarks, `plugin.store.*` ops, revocation apply |
| `omuzaki-plugin` | implementation | the install, update, pin, rollback and keep ops call the store crate's checks (extends 0058's install job) |

`omuzaki-plugin-store-api` depends on `omuzaki-core`, `omuzaki-store-api` (`ContentHash`), `omuzaki-plugin-api`
(`Manifest`, `PluginId`, `Requests`; the `PermissionSummary` of #1081, or, if #1081 does not define it, the store-api
task defines it as a read-only rendering of `Requests`: scopes, `net_hosts`, data, quota, worlds), `omuzaki-library-api`
(`Version`) and `omuzaki-identity-api` (`Signed`, `Domain`, `Identity`, `Check`, `verify`). Its own `plugin.stores`
setting is a static registration in the new store crate. `omuzaki-identity-api` gains `Domain::PluginIndex` and
`Domain::PluginRevocation` (T1). `omuzaki-plugin-api` gains nothing. Crates are added to `crates/README.md` by the
task that creates each.

```rust
pub trait PluginStore: Send + Sync {
    fn refresh(&self, store: &StoreId) -> Result<StoreStatus, StoreError>;      // index + revocations, verified, pinned
    fn entries(&self, store: &StoreId) -> Vec<IndexEntry>;                       // cached, verified
    fn revoked(&self, hash: &ContentHash, key: &KeyId) -> Option<Revocation>;
    fn fetch(&self, entry: &IndexEntry, into: &Path) -> Result<PluginPackage, StoreError>;  // steps 2 to 4 of §7
}
```

`StoreError`: `Replay`, `Expired`, `BadSignature`, `Tampered(Duplicate|Fields)`, `ListingMismatch(field)`, `Licence`,
`Revoked`, `Package(refusal)`, `Offline`, `TooLarge`. Every variant derives its text from the constants it names.

## 12. Tests

All against `MemoryStore` on a `FakeClock`; no test touches the network. The hostile corpus is generated, not committed.

| Test | Proves |
|---|---|
| `hostile_packages_refused` | zip-slip (`../x`, `/x`, `C:\x`), symlink, hardlink, two names equal after NFC and case-fold, non-UTF-8 name, 101:1 expansion, 257 entries, 33 MiB, 17 MiB archive, 257 KiB `plugin.toml`, truncated zip and a `component` outside the package each refuse the whole package with nothing written outside staging |
| `swapped_component_fails_hash` | a valid `plugin.sig` with one changed byte in `plugin.wasm` is `hash mismatch` |
| `relabelled_signature_refused` | 0.1.0's valid statement served as 0.2.0 or as another id fails the statement check |
| `signature_outside_hash` | listing `plugin.sig` as content is refused; changing only `plugin.sig` leaves the hash unchanged |
| `wrong_domain_and_key` | a signature over the same hash under `Domain::Export`, `LibraryIndex` or `Plugin` fails as an index signature (and the reverse); an index signed by any key but the pinned one is `BadSignature` |
| `publisher_key_continuity` | a package signed by a valid key other than `entry.publisher.key` is refused; an update signed by a different key needs the typed confirmation, and declined changes nothing |
| `index_replay_expiry_epoch` | a lower `(epoch, sequence)` is `Replay`; past `expires` is `Expired` and offers nothing; a raised epoch resets the pin; one `id@version` with two hashes is `Tampered(Duplicate)` |
| `listing_lies_refused` | an entry that understates `permissions` or shows another licence is `ListingMismatch` at install |
| `nc_licence_refused` | `listing::check` refuses a non-commercial or unknown SPDX id; the client refuses a store entry with one |
| `mid_run_revocation` | a running plugin with a `fetch` waiting; the list names its hash: `Disabled { StoreRevoked }`, the fetch is cancelled, the entry rolls back (the assertions of `plugins.md` §3.4) |
| `revocation_scope` | a revoked key disables every plugin it signed and refuses its installs; revoked while the app was closed, the next start does not load it; an older list is `Replay` and cannot un-revoke |
| `stale_list_does_not_disable` | an expired list shows the badge and disables nothing |
| `keep_rules` | `plugin.keep` on `Malware` is refused; on `Vulnerable` the hash runs again until a blocking list arrives |
| `update_diff_adds_net` | §13's update: the diff shows exactly the added scope and host; declining leaves 0.1.0 running and approved |
| `no_downgrade_via_update` | a lower-semver entry is never an update target; `rollback` goes only to the kept version |
| `nothing_unsolicited` | no store: a launch makes zero requests; with one on `manual`, still zero; `refresh` makes exactly two GETs with no identifying header |
| `store_faults` | scripted 500, stall, truncated body, redirect and wrong `Content-Length` end as `Offline` or `Package`, keep the last good cache and leave staging empty |
| `deterministic_index` | the static builder run twice over the same inputs gives byte-identical files |

## 13. A worked example

The synthetic plugin `wave` (a placeholder component, not real wasm; a throwaway test key; hashes computed with
`plugins.md` §7's preimage, BLAKE3, by the script that made this page).

```
wave-0.1.0.zip  (package hash b18ffbbe2b21c3b9d19eb9b65d8eaa0ae0c4d5627618a287bdced017aa21c446)
  LICENSE              37 B  811e3d256afad4b1…
  l10n/en-US/wave.ftl  24 B  a3a117f74893d48d…
  plugin.toml         308 B  078aa67802187661…
  plugin.wasm          35 B  68e4c1eb8233a83a…
  plugin.sig                 (not hashed)  Signed<PackageStatement { wave, 0.1.0, b18ffb… }>, Domain::Plugin
wave-0.2.0: same LICENSE and .ftl; plugin.toml 347 B 978949608b292252…; plugin.wasm 35 B b13c392810291538…
  package hash 6a721714e7b1df758ea502d97426cb6411b11a5038e6a66a0f92fd8e393e2f55
```

One index entry (in `index.json`, epoch 1, sequence 57, expires 14 days after `issued`):

```json
{ "id": "wave", "version": "0.2.0", "hash": "6a721714e7b1df758ea502d97426cb6411b11a5038e6a66a0f92fd8e393e2f55",
  "url": "packages/wave-0.2.0.zip", "size": 1204, "publisher": { "name": "Example Studio", "key": "<64 hex>" },
  "licence": "MIT", "name": "Wave", "summary": "Waves when the project opens.", "min_app": "0.9.0", "yanked": false,
  "permissions": { "scopes": ["read","perform","net"], "net_hosts": ["api.example.com"], "data": true,
                   "data_quota_mb": 16, "worlds": ["plugin"] }, "review": null }
```

One revocation entry (in `revoked.json`, sequence 12), naming the old build after a vulnerability:

```json
{ "target": { "hash": "b18ffbbe2b21c3b9d19eb9b65d8eaa0ae0c4d5627618a287bdced017aa21c446" },
  "class": "vulnerable", "note": "0.1.0 reads files outside its folder; update to 0.2.0." }
```

The update from 0.1.0 to 0.2.0 adds `net` and a host. The person runs `plugin.update wave`; the steps of §7 pass (same
key as 0.1.0, hash and statement match, licence MIT) and the card shows only the difference; 0.1.0 keeps running:

```
Update Wave 0.1.0 → 0.2.0                  Example Studio   key 3fa9… (same key as 0.1.0)   MIT (unchanged)
 + May use the network            NEW   Wave asks to send and receive data
 + api.example.com                NEW   only this address, never others
   Read your project              unchanged
   Perform actions as itself      unchanged
   Its own data folder, 16 MB     unchanged
 Not from Omuzaki: this plugin is made by someone else.   Not reviewed.
 [ Update ]   [ Keep 0.1.0 ]
```

Declining leaves 0.1.0 approved for `read`, `perform` and `data` only (plugins.md §3.2); accepting installs 0.2.0 beside
it, sets the approval to the new set and reloads. Since 0.1.0 is revoked as `vulnerable` in `revoked.json`, the
person who declines sees it disabled with the note and may confirm `plugin.keep` to run it anyway.

## 14. Questions for the owner (none blocks a folder or static-host store)

1. Hosting of the Omuzaki store, its signing-key custody (offline key, rotation drill) and who may publish.
2. Default revocation cadence: `manual` with a badge (this design, SE-04) against `daily` once a store is added; the
   cost of `manual` is that a machine that never connects learns late.
3. A review service: who runs it, what it promises, moderation and takedown. This design needs only §9.
4. A transparency log for the store (Rekor's idea) as a later additive layer.
5. Whether to ship the unsigned-package tightening (`plugin.developer_mode`, §7) or keep ADR 0058's rule.

## 15. Tasks (filed by the lead after review)

One PR each, about 400 lines or fewer. `R` is the reference machine of `plugins.md` §5.2.

- **T1. Identity: store domains** (contract; Director approves on its issue first). *Fence:* `crates/omuzaki-identity-api`.
  *Contract:* `Domain::PluginIndex` (`omuzaki-plugin-index-v1\0`), `Domain::PluginRevocation`
  (`omuzaki-plugin-revocation-v1\0`), in `Domain::ALL`. *Acceptance:* the all-pairs tag-prefix test passes with the 2
  new tags; 2 new vectors; a signature under each fails under the other 10 domains. *Blocked by:* #1053 (PR #1544) and
  the `Domain::Plugin` task of ADR 0058. *Model:* haiku.
- **T2. Store types, parsers, verification, listing check, diff** (`omuzaki-plugin-store-api`). *Fence:* the new crate
  and its workspace line (hub sign-off). *Contract:* §4 to §9's types, `parse_*`, `listing::check`, `PermissionDiff`,
  the traits. *Acceptance:* every §12 row that needs no client (package, signature, index, listing, diff) passes; an
  8 MiB, 10,000-entry index parses and verifies in under 100 ms on `R`; builds for `wasm32`; every error text derives
  from its constant. *Blocked by:* T1, #1081. *Model:* haiku.
- **T3. Store mock and hostile corpus** (`omuzaki-plugin-store-mock`). *Fence:* the new crate. *Contract:* `MemoryStore`
  with scripted faults; a package builder for every hostile shape of §12. *Acceptance:* each shape builds in under 50
  ms; the mock passes T2's conformance suite; no fixture file is committed. *Blocked by:* T2. *Model:* haiku.
- **T4. Store client: setting, fetch, cache, watermarks** (`omuzaki-plugin-store`). *Fence:* the new crate.
  *Contract:* `plugin.stores`, `plugin.store.refresh/list/describe/add/remove`, cache and pinned pairs in the state
  file. *Acceptance:* `nothing_unsolicited`, `store_faults`; an unchanged index costs 1 conditional GET and 0 parses;
  a restart keeps the pins; a 6 MB index refreshes within 2 s on a loopback fake. *Blocked by:* T2, T3,
  `omuzaki-http-api`. *Model:* sonnet.
- **T5. Revocation fetch and apply** (`omuzaki-plugin-store`, `omuzaki-plugin` revocation modules). *Contract:* §6's
  apply, `DisabledReason::StoreRevoked`, `plugin.keep`, the badge data. *Acceptance:* `mid_run_revocation` cancels an
  in-flight `fetch` within 50 ms; `revocation_scope`, `keep_rules`, `stale_list_does_not_disable`; a 50,000-entry list
  applied to 200 installed plugins takes under 20 ms on `R`. *Blocked by:* T4, ADR 0058's disable path. *Model:* sonnet.
- **T6. Install, update, pin, rollback with the permission diff** (`omuzaki-plugin` install and lifecycle modules).
  *Contract:* the `source` widening of `plugin.install`, `plugin.update`, `plugin.pin`, `plugin.rollback`, the diff
  cards, `plugin.developer_mode`. *Acceptance:* §7's flow on `MemoryStore`; `update_diff_adds_net`,
  `publisher_key_continuity`, `no_downgrade_via_update`, `listing_lies_refused`; a declined update leaves files and
  approval byte-identical; a 16 MiB install stages and verifies within 1.5 s on `R` without the edit lock. *Blocked
  by:* T4, T5, ADR 0058's install task. *Model:* sonnet.
- **T7. Pack, sign and static store builder** (`crates/omuzaki-cli`). *Contract:* `omuzaki plugin pack|sign|verify`,
  `omuzaki plugin-store build <dir>` writing §5's layout with the store's `Signer`. *Acceptance:*
  `deterministic_index`; a built folder store refreshes and installs through T4 and T6 in an integration test; `pack`
  refuses every hostile shape T3 builds. *Blocked by:* T2, T3. *Model:* sonnet.
- **T8. Store browser panel** (`omuzaki-ui` plugin panel; visual sign-off). *Contract:* the list, search over the cached
  index, §7's card, the revocation badge and the "out of date" states. *Acceptance:* the card for §13's update matches
  a golden screenshot; every `StoreError` has a string and a test; keyboard reachable. *Blocked by:* T6 and the owner's
  hosting decision (question 1) for the default entry. *Model:* sonnet.
- **T9. Owner task: hosting and key custody** (not a cloud task): questions 1 to 3 of §14.
