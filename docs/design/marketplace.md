---
title: "The marketplace client: browse, install, publish, receipts and licences"
summary: "the marketplace client: listings, signed receipts verified offline, install order and licence gate, publish preflight, reports, privacy (ADR 0063)."
area: design
---

# The marketplace client: browse, install, publish, receipts and licences

**Status:** proposed. **Feature:** LB-08 (and SE-07 for what travels in an export). **Decision record:** `docs/decisions/0063-marketplace.md`. **Wire detail:** `docs/design/marketplace-protocol.md` (signed documents, freshness, HTTP, limits).
**Spec:** v3 §28 (LB-08), §31 (SE-07), §37, §38 (open question 5).
**Builds on:** `docs/design/library-sharing.md` (D2 to D4, D7, D9, D11, D14; ADR 0057), `docs/design/library.md` (D2 ids, D5 the record is the lock, D8 rights and the gate, D9 install), `docs/design/identity.md` (D3 envelope, D4 trust, D5 key statements, D6 signed exports, D10 domains).

This page fixes what the **client** does and what it needs from a backend, so a `FakeMarket` can stand in for tests. It adds no signing scheme and no verification rule: a listing is advice, the signed item is the truth, and a receipt is one more signature checked against a key the person pinned. Who runs a backend, payments and moderation are the owner's (§8) and are not decided here. This design covers **single items only**; packs are a later extension (D8).

## 1. Prior art, and where this design beats it

Read on 2026-10-15 (search results, not source audits; the claims below are the ones the sources support).

| Client or store | Licence | What it does well |
|---|---|---|
| **Open VSX** (Eclipse; registry, web UI and the `ovsx` CLI; used by Theia and VSCodium) | EPL-2.0 | Vendor-neutral and self-hostable; one small publish/search/download API any editor can implement; namespaces owned by a verified publisher; a managed service exists. |
| **Flatpak / Flathub** (the `flatpak` client over OSTree) | LGPL-2.1-or-later (client) | Refuses an unsigned remote by default; the signing key is pinned per remote; content is addressed by hash; apps install and run offline once fetched. |

**Best to learn from: Open VSX** (the closest match: a store of third-party add-ons with a thin open client). Its client trusts the registry's answer: the user must trust whoever runs the registry, and a 2026 report describes malicious extensions distributed through it. Flatpak's one-key-per-remote signature protects the transport, but says nothing about who made an app or what it may be used for.

**Where this design beats them** (each is tested, §7):
1. **Per-creator signatures, verified in the client.** A hostile or buggy store can only refuse to serve, never swap an item (reuse of ADR 0057). Open VSX trusts the registry; Flatpak trusts one remote key.
2. **Licence is data the client enforces.** The same gate that blocks a publish blocks an install in publish-ready mode, and the listing can't misdescribe the item (D2, D5).
3. **A receipt is verified offline** and bound to the holder's key, so a stolen file is useless and a lost connection costs nothing; a replayed old revocation list cannot clear a paid install (D3, protocol §2).
4. **No tracking by construction:** a listing carries paths, never hosts; the client contacts one pinned base address and sends a fixed header set (D10, protocol §3).
5. **What was installed stays installed:** the project holds what it pinned, with its rights and receipt hash, and runs with the store gone or the receipt expired.

## 2. The shape of it

```
 person's machine                                         market (hosted or fake; untrusted bytes)
┌─────────────────────────────────────────┐   HTTPS   ┌────────────────────────────────────────┐
│ Browse panel / ops (library.market.*)    │◀────────▶│ search  listing  manifest  assets      │
│ Market (omuzaki-market-api)              │  JSON +   │ receipt  head  revocation/takedown     │
│   verify: listing↔manifest, item sig,    │  streams  │ pages  key changes  report  reviews    │
│   receipt, freshness, rights, gate       │           └────────────────────────────────────────┘
│ installs into My library (library.install)│   pure verification is shared with
└─────────────────────────────────────────┘   omuzaki-library-source-api::verify
```

A market moves bytes and text. The client decides everything that matters.

## 3. Worked example (on disk)

The project's own fixtures only: market key `fake-market` and keys for Ben (creator) and Cleo (buyer) come from test seeds (`MemorySigner::from_test_seed`); item `ben/hair/bob@1.0.0` is the hair item of the sharing example; the client's clock is a fixed test `Clock`. `<64 hex>` stands for a real hash; the T1 mock generates this tree and the tests check it byte for byte.

```
market-fixture/
  market.toml                       base = "http://127.0.0.1:<port>", key = "<fake-market key>", name = "FakeMarket", retired = []
  head.json                         Signed<MarketHead>: seq 1, issued = fixture clock, 1 revocation page, 0 takedown pages
  listings/lst-0001.json            ben/hair/bob@1.0.0, premium (commerce present), custom terms
  listings/lst-0002.json            ben/hair/rough@1.0.0, free, manifest says commercial: no
  items/ben/hair/bob/1.0.0.toml     signed manifest (Ben); rights carry attestation and terms_hash; asset_info for its one decal
  items/ben/hair/rough/1.0.0.toml
  terms/<64 hex>.txt                the custom terms text of bob
  receipts/lst-0001.json            Signed<EntitlementBody> by the market key, holder = Cleo
  pages/revocations/<64 hex>.json   Signed<MarketPage>, index 0, no entries
```

`listings/lst-0001.json` (a `Listing`; every text is data to display, never to trust):

```json
{ "id": "lst-0001", "item": "ben/hair/bob", "version": "1.0.0", "manifest_hash": "<64 hex>",
  "creator": "<Ben key>", "title": {"en": "Bob hair"}, "summary": {"en": "A short bob for the kit characters."},
  "section": "parts", "kind": "part", "target": "body",
  "licence": { "spdx": "LicenseRef-fixture-terms", "commercial": "ok", "attribution": "Ben", "grant": "licensed", "terms_hash": "<64 hex>" },
  "previews": ["/v1/listings/lst-0001/preview/0"], "stats": {"count": 0, "mean_x10": null},
  "access": "required", "commerce": {"kind": "fixture", "ref": "order-0001"} }
```

`receipts/lst-0001.json` (`Signed<EntitlementBody>`, `Domain::Entitlement`, signed by the market key):

```json
{ "body": { "doc": "receipt", "market": "<fake-market key>", "listing": "lst-0001", "item": "ben/hair/bob", "versions": ">=1.0.0, <2.0.0",
            "holder": "<Cleo key>", "order": "order-0001", "issued": 1, "terms_hash": "<64 hex>" },
  "signer": { "name": "FakeMarket", "key": "<fake-market key>" }, "signature": "<128 hex>" }
```

**Install that passes.** Cleo runs `library.install {from: market, listing: lst-0001}`. The client fetches the head and the new revocation page (freshness holds, protocol §2), then the manifest, and checks in D5's order: manifest hash equals the listing's; Ben's `LibraryItem` signature holds; compatibility, deprecation and takedown are clear; the receipt verifies (market key pinned, `holder` is Cleo's identity key, item and version in range, `terms_hash` equals the manifest's, not in the revocation chain); the asset sizes fit the item limits; the effective rights (declared joined with the decal's) equal the listing's licence; the custom terms text is fetched and its hash matches; the gate passes. The key is `Unknown`, so Cleo sees "Trust this key…", the attestation, the terms and the per-item acknowledgement (library.md D8). Only then is the decal streamed into a staging folder, hashed as it arrives, and everything is promoted into My library in one step. Cleo's user data holds the item, its signed receipt and the terms text; nothing of the order is stored in a project.

**Install the licence gate refuses.** `lst-0002` is free, but its manifest says `commercial: no`. Two refusals:
- With the Machine setting `library.market.publish_ready = true` (off by default), `library.install` fails with a located finding `listing lst-0002`: "ben/hair/rough 1.0.0 is not cleared for published work (commercial: no)", hint "turn off publish-ready to install it for personal use". This is `licence.derive` unchanged (library.md D8) run on the item's effective rights, before any download (D5 step 5).
- If the listing had said `commercial: ok` while the manifest says `no`, the install is `Refused(LicenceMismatch)` always: the store misdescribes the item, whatever the setting. Both are rows in §7's matrix.

A purchase refused for no receipt (`lst-0001` without `receipts/lst-0001.json`, or with one for another holder) is `Refused(NoEntitlement)`; on a machine that has never reached the market (no cached head) it is `Refused(Stale)`. Both happen before any asset is requested.

## 4. Decisions

### D1. Position: `Market` is the listing layer; bytes verify through the shared pure code

`MarketStore: ItemStore` (ADR 0057 D11) is **refined**: a market has no log, no roster, no licence file and no index, so most of `ItemStore` would be dead methods. Instead `Market` (§5.1) returns listings, a manifest's bytes, a bounded stream per asset and the signed documents of protocol §1; the checks on those bytes are `omuzaki-library-source-api::verify` (manifest hash, `LibraryItem` signature, asset hash and size, limits), called on both paths. One verifier, two transports. This is the "store split" ADR 0057 left open. Asset bytes are verified as a stream (D5); the source API gains an additive reader-based asset check for it (T1's dependency).

### D2. The listing is advice; the signed item is the truth

A `Listing` is data the backend serves. The client treats every field as a claim to be checked against the signed manifest before it affects anything:
- `manifest_hash` must equal `ItemManifest::hash()` of the fetched manifest, `creator` its `publisher.key`, `item`/`version` its id and version (`Tampered(Listing)` otherwise);
- `licence` must equal the item's **effective rights**: the manifest's declared `rights` joined strictest-first with every asset's rights in the signed manifest (library.md D8; D5 step 5), including `terms_hash`. Any difference is `LicenceMismatch`. (The team-licence join of sharing D7 is not used: a market has no team licence);
- `section`, `kind`, `target` are display and filters only; the install re-derives them from the manifest. There is deliberately **no app-version field**: the signed manifest has none (`compat.requires` lists part kinds), and adding one needs a manifest contract change for little gain, since an item that needs an op this app lacks already fails `ItemManifest::check`. A store-supplied version requirement would be a claim nobody could verify (Q12 keeps the option).
Text is plain: `title` at most 80 characters, `summary` 500, per language, control characters and markup stripped on parse, never rendered as links. Previews are paths on the pinned base (never hosts, shape rules in protocol §3), at most 5 and 5 MiB each, decoded by the existing image path and shown labelled "from the store, not checked".

### D3. The receipt: a signature the client verifies offline

An entitlement (the receipt) is `Signed<EntitlementBody>` under `Domain::Entitlement` (identity reserved it). `EntitlementBody { doc, market, listing, item, versions: VersionReq, holder: KeyId, order: String, issued: u64, terms_hash: Option<ContentHash> }` is `deny_unknown_fields` with required fields no other body has (identity D3); protocol §1 lists its siblings under the same tag. It is checked in this order, with no network:
1. signed by the **pinned market key**, or by a **retired** pin of that market (protocol §4: a rotation needs both keys' signatures and the person's confirmation);
2. `holder` equals the key of the identity slot the person installs with (a receipt is useless to anyone else; the market that issued it knows the holder and order, nobody else is sent them, D8);
3. `item` and `listing` equal the listing's, the version is in `versions`, and `terms_hash` equals the **manifest's** `rights.terms_hash` (both absent for an SPDX licence);
4. its hash is not in the verified **revocation chain** (protocol §1: append-only signed pages, no size cap, current as of the freshness rule of protocol §2).
`issued` is the market's claim and only displayed (identity D4). A refund revokes: it stops **new installs and updates**, never an applied record (sharing Q6: no silent recall). A receipt can also be imported from a file (`library.market.import_receipt`), so a gift or an offline machine works; an imported receipt passes the same four checks.

**Freshness (summary; the rule is protocol §2).** A paid install or update needs a market head fetched this attempt and recent (default 24 h), or a cached one that was fresh when fetched and is under 14 days old. A fresh machine offline, or a market serving an old signed list, gives `Stale`: the client refuses rather than guess. Free items install without a receipt; they are labelled "takedowns not checked" when the head is unavailable.

### D4. Entitlement is the listing's, the client only checks it

`access` is `free` or `required`. For `free`, install needs no receipt. For `required`, install without a valid receipt is `NoEntitlement`. What `commerce` means, how a receipt is earned and what is charged are not the client's: it is opaque JSON, never parsed, never used in a decision, and never written into a project. Prices and currencies, if a backend has them, are inside `commerce`.

### D5. What an install checks, and in which order

`library.install {from: market, listing}` is person-only with the per-item acknowledgement (library.md D8, D9) and needs the market's `net` scope. Order: cheapest and least-trusting first. **Steps 1 to 5 use signed metadata only: no asset byte is requested before every cheap refusal has passed**, and nothing is written until step 7.
1. `Listing` parses within limits (16 KiB, path shapes); the item is not on the person's local hide-list.
2. **Freshness:** fetch the head and sync the new revocation and takedown pages (protocol §2). A paid item with no established freshness is `Stale`.
3. Fetch the manifest (size-limited); `manifest_hash`, id and creator match (D2); the `LibraryItem` signature verifies for `creator`; identity `Check` is not `Revoked` (`Unknown` offers "Trust this key…", `Tampered` refuses).
4. **Cheap refusals, in this order:** *compatibility* (`ItemManifest::check`: targets, templates, required parts and every op available; a failure says which is missing); *status* (the manifest is not `deprecated`, unless a project already pinned it; no takedown for this `manifest_hash`, `TakenDown`); *entitlement* (D3) only if `access = required`; *item limits* (the signed manifest's `asset_info`: at most 64 assets, 1 GiB in total, 256 MiB each; `TooLarge`; enough free disk).
5. **Rights, computed once:** `effective = declared rights ⋈ every asset's rights` from the manifest (the strictest join, library.md D8). Then, in order: `effective` equals the listing's licence (`LicenceMismatch`); `private_test` is refused (`PrivateTest`); a market item has its creator attestation (D7); a `LicenseRef-` licence has `terms_hash` and the text is fetched (≤ 64 KiB) and its hash matches (`Tampered(Terms)`); then the **item-level gate**: with `publish_ready` on, `derive_item(&effective)` must pass (`NotCleared`). There is no target document at install time, so the gate runs `licence.derive`'s join over one synthetic source row built from `effective`, not over a project (T4 adds that entry point in `omuzaki-io-ops`; the join is unchanged). The person then acknowledges.
6. **Stage the assets** (streaming, below). Each asset not already in the store is fetched by hash through a bounded sink that hashes while it writes into a staging folder (`<data dir>/omuzaki/library/staging/<install>/`); the stream stops at the declared size plus one byte; a hash or size mismatch is `Tampered(Asset)` and the staging folder is deleted.
7. **Promote:** only when every asset has verified, the staged files are moved into the content-addressed store (idempotent, first write wins), then the manifest, the **entitlement** and the terms text are written into My library, the manifest last, so the item is visible only when complete. A crash leaves staging files only, which the next start removes.

Each refusal is a located finding with a hint and an op record, with nothing partial in My library (steps 1 to 5 touch memory only; step 6 only the staging folder).

**Where the entitlement lives.** `library.install` has no document and so no application record. The signed receipt is stored **with the installed market item in user data**, as a sidecar of its manifest in My library (library.md D11 layout), with the market's identity (base, key) and the terms text beside it. `library.update` re-checks it for the new version without the network (the item keeps the receipt's `versions` range). When the item is applied (`library.apply`, a later and separate op) only the **receipt hash** is copied into that application record's `receipt` field; apply never contacts the market and never re-checks the receipt, so a refund cannot disable an applied record or a later apply of an installed item.

### D6. Licence enforcement is provenance and the gate, not copy protection

An item is data; once on a machine it can be copied. The design says so and does not pretend otherwise. What it does:
- **Provenance.** Rights travel in the item and into the record: `author` (creator), `licence` (SPDX or `LicenseRef-`), `attribution`, `commercial`, `grant`, and the `receipt` hash. A purchased item is `grant: licensed` unless its own rights transfer ownership (the team-licence rule); the market can't upgrade it.
- **The gate** (`io.check_export`, library.md D8) blocks a `publish` export of a character whose records are `commercial: no|unknown` or `private_test`. Nothing new: the market adds no rule to the gate.
- **Signed exports (SE-07).** The export statement already commits to the licence claims; this design adds only that the claims name the creator's key and licence, so a third party can verify who made a part and under what terms. The receipt hash and the order are not exported (D8).
- **Honest limit.** A person who strips records or edits a file is outside this system's reach; the design gives honest people clear answers and gives anyone else's claims a verifiable origin.

### D7. Publish: the creator's preflight, and the signed rights it needs

`library.market.publish {item, meta}` is person-only, needs `publish`, `edit:library` and the market's `net` scope, and agents get `needs_person`. A dry-run returns one finding list; nothing leaves the machine until the person confirms and sees exactly what is sent. It extends sharing D10's six checks (valid manifest, signature made and verified before upload, version bump matches the diff) with:
1. rights complete, **`commercial: ok`** (a market listing for published work can't be a trap), no `private_test` on the item or any asset, previews rendered from the item's recipe on the kit (no user character);
2. every asset has provenance with a licence, and none contradicts another record in the person's store for the same hash. The preflight copies each asset's size and rights into the manifest's signed `asset_info`, and the item's `rights` must already be at least as strict as every asset's (D5 step 5 recomputes it);
3. **the attestation.** The client cannot detect third-party IP it has not been told about, so the creator attests, in the signed manifest, "I made this or hold the right to licence it". The signed `ItemRights` gains `attestation: Option<Attestation { by: KeyId, claim: String }>`, where `claim` is the id of a shipped, versioned text (`own-or-licensed-v1`) shown to the person before signing, so the signature names exactly what was said. `by` must equal `publisher.key`; a market item without it is refused at install;
4. a **licence template** chosen from the shipped list (`CC0-1.0`, `CC-BY-4.0`; templates that forbid commercial use are not offered) or a custom `LicenseRef-` text. A custom text is stored by hash: `ItemRights.terms_hash: Option<ContentHash>` is signed with the manifest, so a store cannot swap the terms. The text (≤ 64 KiB) is sent with `publish`, and installs fetch it from `GET /v1/terms/{hash}`, check the hash, keep it beside the installed item and show it before the acknowledgement;
5. title, summary and language tags within D2's limits; the listing `licence` is generated from the manifest, never typed twice.
Both new rights fields and the `asset_info` map are additive and skipped when absent, so every existing manifest keeps its hash and signature; they are a contract change to `omuzaki-library-api`, a task of its own (T2) that needs the Director's approval. The order of upload is assets (streamed), then manifest, then the listing metadata, so a crash leaves unlisted bytes the next publish completes (sharing D10).

### D8. Reviews, ratings and reports are client-side requests, not trust

- **Ratings and reviews** are shown as `stats` and a page of `Review { author_label, rating 1..5, text, lang }`, all labelled "reported by the store, not checked". They never enter a decision. Posting (`library.market.review`) is person-only, allowed only for an item the person has installed, and sends the listing id, a rating and at most 1 KiB of text; the backend decides the rest.
- **Report** (`library.market.report`) sends the listing id, a `ReportReason` code (`Rights`, `Safety`, `Spam`, `Broken`, `Other`) and at most 1 KiB of note, after showing the person exactly that. No key, no machine details. The client then offers a local **hide** (the listing or the creator's key) that contacts nothing.
- **Free and paid items** differ only in `access`; nothing else in the flow changes. **Packs are out of this design.** A listing, a receipt and an install each name one item and one version; a pack needs a collection type, a receipt that covers a set, and an atomic multi-item install, and is a later extension (Q11).
- **What travels where:** the order and holder key are known to the market that issued the receipt and are never sent anywhere else; the receipt is used on the machine, kept with the installed item, and never written into a project or an export (only its hash is, D5). `commerce` is never read by a decision.

### D9. What is in the free client and what is hosted

| In the client (this repository) | Hosted (the owner's, §8) |
|---|---|
| `omuzaki-market-api` (types, formats, verification), `FakeMarket` and its conformance suite, the HTTP client, install, preflight, the browse panel, the report and review requests | search index, listings storage, payments, receipts issuance and refunds, revocation and takedown pages, ratings storage, abuse handling, the backend itself |
Everything on the right is behind the `Market` trait and the HTTP shape of `docs/design/marketplace-protocol.md`; swapping a real backend for `FakeMarket` changes no client code. The ops are `library.market.add`, `.search`, `.install` (= `library.install {from: market}`), `.import_receipt`, `.rekey`, `.publish`, `.report`, `.review`, `.hide`; all that write, pin or spend a key are person-only (D7, D8, D10) and agents get `needs_person`.

### D10. Privacy

- **No telemetry** and no tracking of browsing: no request carries a client id, a cookie, a referrer, or an install count, and the header set is fixed and enumerated (protocol §3). The only contact is the pinned base address; a listing holds paths only, so it can't make the client fetch from another host. Heads, pages and takedowns need no token, so checking them identifies no account.
- **A market is added by the person** (`library.market.add`, person-only, shows the base address and the key fingerprint, then pins both). Nothing in a response can pin itself; a key change needs the person's confirmation (protocol §4).
- **Searches go to that market only**, are never logged by the client beyond the op record's market and query length, and `library.market.search` is off in an agent session without a `net` grant.
- **Credentials** are in the secret store; logs name the market and the listing, never the token (CN-05).
- **Cache:** listings and previews are cached under the data dir within `library.market.cache_mb` (default 256, least-recently-used); clearing it is one action. Verified revocation and takedown pages are kept for good (they are append-only protection, 32 bytes an entry) and are not part of that budget.

### D11. The panel: accessibility and the six languages

The browse panel (T6) is a search field, a filter row, a result listbox and a detail pane. Results use the design-system Listbox pattern (one Tab stop, arrow keys, AccessKit `ListBoxOption` with title, creator and state in the name), the detail pane is a labelled form, and every state (free, needs receipt, owned, deprecated, taken down, stale, refused) is said in words, not only by colour or icon. Interface strings go through the l10n pipeline in all six languages. Listing text is the creator's: `title` and `summary` are maps by language tag, the panel uses the interface language, then `en`, then the first available, and marks "not in your language". The client never machine-translates and never sends a listing to a translation service.

## 5. The contract and the task cut

### 5.1 The contract (names are final; doc comments belong to the implementer)

New API crate **`omuzaki-market-api`**: it needs identity and library types plus listing and receipt formats, none of which belong in the library contract, and its verifier must compile for wasm. The signed documents (`EntitlementBody`, `MarketHead`, `MarketPage`, `Takedown`) are in protocol §1.

```rust
pub trait Market: Send + Sync {                          // the client's view of a backend, as data
    fn search(&self, q: &MarketQuery) -> Result<ListingPage, MarketError>;
    fn listing(&self, id: &ListingId) -> Result<Listing, MarketError>;
    fn head(&self) -> Result<Signed<MarketHead>, MarketError>;                          // protocol §2
    fn page(&self, doc: PageDoc, hash: &ContentHash) -> Result<Signed<MarketPage>, MarketError>;   // revocations / takedowns
    fn key_changes(&self) -> Result<Vec<KeyChange>, MarketError>;                       // protocol §4
    fn manifest(&self, id: &ListingId, version: &Version) -> Result<Vec<u8>, MarketError>;   // bytes, verified by the client
    fn terms(&self, hash: &ContentHash) -> Result<Vec<u8>, MarketError>;                // ≤ 64 KiB
    fn asset(&self, hash: &ContentHash, max: u64, sink: &mut dyn AssetSink) -> Result<u64, MarketError>;   // streamed; stops at max + 1
    fn receipt(&self, id: &ListingId) -> Result<Option<Signed<EntitlementBody>>, MarketError>;
    fn put_asset(&self, hash: &ContentHash, size: u64, src: &mut dyn std::io::Read) -> Result<(), MarketError>;   // publish, step 1
    fn publish(&self, manifest: &[u8], terms: Option<&[u8]>, meta: &ListingMeta) -> Result<ListingId, MarketError>;   // after the assets
    fn report(&self, id: &ListingId, report: &Report) -> Result<(), MarketError>;
    fn reviews(&self, id: &ListingId, page: Option<Cursor>) -> Result<ReviewPage, MarketError>;
    fn post_review(&self, id: &ListingId, review: &ReviewPost) -> Result<(), MarketError>;
}
pub trait AssetSink { fn write(&mut self, chunk: &[u8]) -> Result<(), MarketError>; }   // the client's staging writer: hashes, counts, refuses past `max`
pub struct MarketPin { pub base: String, pub key: KeyId, pub retired: Vec<KeyId> }     // market.toml: base and key shown, then pinned by a person
pub struct Listing { pub id: ListingId, pub item: ItemId, pub version: Version, pub manifest_hash: ContentHash, pub creator: KeyId,
    pub title: BTreeMap<LangTag, String>, pub summary: BTreeMap<LangTag, String>, pub section: String, pub kind: String, pub target: String,
    pub licence: ListingLicence, pub previews: Vec<String>, pub stats: Option<Stats>,
    pub access: Access, pub commerce: Option<serde_json::Value> /* opaque, never read */ }
pub enum Access { Free, Required }
pub enum InstallRefusal { Malformed, Tampered(Tamper), NoEntitlement, Revoked, Stale, TakenDown, MarketKeyChanged, LicenceMismatch,
    PrivateTest, NotCleared, Incompatible(String), Deprecated, TooLarge { what: String, limit: u64 }, NotAllowed(String) }
pub fn verify_install(market: &dyn Market, plan: &InstallPlan, trust: &dyn TrustStore, state: &MarketState, clock: &dyn Clock)
    -> Result<VerifiedPlan, InstallRefusal>;   // D5 steps 1 to 5: metadata only, no asset bytes; the implementation crate stages and promotes
pub fn verify_receipt(r: &Signed<EntitlementBody>, pins: &MarketPin, holder: &KeyId, listing: &Listing,
    manifest_terms: Option<&ContentHash>, revoked: &RevocationSet) -> Result<(), InstallRefusal>;
pub fn effective_item_rights(m: &ItemManifest) -> ItemRights;   // D5 step 5: declared ⋈ asset_info, pure
pub enum MarketError { NotFound, Unavailable(String), Forbidden, TooLarge { what: String, limit: u64 }, Malformed(String), Rejected(String) }
```

`MarketState` is the local record of the verified pages and the last head with its local fetch time; `Clock` is injected so freshness tests are deterministic. `Domain::Entitlement` is already in identity; the bodies are distinct types under it, and identity D10's row is widened to say so (this PR). **`omuzaki-library-api` changes only additively, in one task (T2):** `ItemRights.attestation` and `ItemRights.terms_hash` (D7), `ItemManifest.asset_info: BTreeMap<String, AssetInfo { size, commercial, licence, private_test }>` keyed like `assets` (what the join and the limits read before download), and the application record's `receipt: Option<ContentHash>` (D5). All are skipped when absent, so existing hashes and signatures are unchanged.

### 5.2 Crates

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-market-api` | API | §5.1, limits, `verify_receipt`, `verify_install`, `effective_item_rights`; wasm-clean |
| `omuzaki-market-mock` | Mock | `FakeMarket` (in-process and over a loopback HTTP server, no network), conformance suite, the §3 builder |
| `omuzaki-market` | Implementation | the HTTP client, staging, the `library.market.*` ops, install, publish preflight |

### 5.3 The task cut (file when the design is accepted)

| # | Task | Model, lane | Fence | Acceptance | Blocked by |
|---|---|---|---|---|---|
| T1 | `omuzaki-market-api` and `omuzaki-market-mock` (`FakeMarket`, loopback server, conformance suite) | sonnet, human lane | the two crates, root `Cargo.toml` (hub sign-off), `docs/design/identity.md` D10 row (done in this PR) | every row of §7's matrix has a test; the §3 tree matches goldens byte for byte; loopback serves with no outside network; wasm32 check green; page chains tested at 100 000 entries | T2 (types); library-sharing T1 (incl. an additive reader-based asset check, asked on its issue); the Director's approval of the new API crate on its issue |
| T2 | **contract:** `omuzaki-library-api` additive: `ItemRights.attestation` and `terms_hash`, `ItemManifest.asset_info`, record `receipt` | sonnet, human lane | `omuzaki-library-api` only | existing manifests keep their hash and signature byte for byte; new fields round-trip and are skipped when absent; unknown-field behaviour unchanged; a manifest with assets but no `asset_info` is flagged for market use only | the Director's approval of the contract change on its issue (lands before T1, T4, T5) |
| T3 | the HTTP client (`Market` over `ureq`/`reqwest` as the workspace allows) | sonnet | `omuzaki-market` (client module) | conformance suite green against `FakeMarket` over loopback; a recording server sees exactly the header set of protocol §3 on every route; same-origin redirect followed twice, cross-origin and downgrade refused; response over its limit `TooLarge`; asset stream stops at max + 1 and never buffers an asset; stall timeout tested | T1 |
| T4 | install with receipt, freshness and gate (`library.install {from: market}`; staging and atomic promote; entitlement sidecar; `receipt` hash copied on `library.apply`; `derive_item`) | sonnet | `omuzaki-market` (install), `omuzaki-library-ops` (apply copies the receipt hash), `omuzaki-io-ops` (item-level derive, join unchanged) | the §3 pass and refusals reproduce; D5's steps each have a failing case in order and no asset request precedes steps 1 to 5; nothing in My library on any refusal and staging cleaned after a crash; imported receipt passes the same checks; offline and replayed-head cases per protocol §2; key change and takedown handled | T1, T2, T3, library-sharing T6 (gate), the Director's approval of the `derive_item` entry point if it changes a public signature |
| T5 | publish preflight (`library.market.publish` dry run, attestation, terms, `asset_info`) | haiku | `omuzaki-market` (preflight) | all D7 checks and sharing D10's six have a failing case; `commercial: no` and `private_test` refused; a missing attestation refused; agent gets `needs_person` | T1, T2 |
| T6 | the browse panel (search, filters, detail, install, report, hide, rekey card) | sonnet, visual | `omuzaki-ui` market panel | rendered at the three themes; every state in words; keyboard and AccessKit labels; six languages; check Listbox pattern | T3, T4 |

## 6. Threats

| Attacker | Does | Result |
|---|---|---|
| market / network | swaps a manifest, asset or terms text | hash mismatch, `Tampered` |
| market | lies in a listing (licence, creator, hash, a path) | `LicenceMismatch` / `Tampered(Listing)` / `Malformed` |
| market / network | serves an old signed head or lists (first use, replay) | `issued` outside the window, or no cached head: paid install `Stale`; free labelled |
| market | omits a held revocation or takedown page | kept and warned (`MissingRecord`); a different page at a held index is `Tampered(Record)` |
| market | grows the revocation list | append-only pages, no cap, 64 per call (protocol §1) |
| someone | reuses a receipt file | `holder` is not the person's key: `NoEntitlement` |
| market | rotates its key, or forges a rotation | needs both keys' signatures and the person's confirmation; else `MarketKeyChanged` |
| creator | lists a `private_test` item, or hides a restricted asset | refused at preflight and at install; the asset's own rights join into the item's |
| creator / store | swaps custom terms, drops the attestation | `terms_hash` and `attestation` are in the signed manifest |
| market | huge or many assets, endless stream | limits from the signed manifest before download; stream stops at size + 1; staging only |
| market | tracks browsing | one pinned address, paths only, fixed header set, no ids (D10) |
| market | hostile text, huge fields, control characters | limits and stripping at parse (D2) |
| market | redirects to another host | refused |

## 7. Tests (no network)

`FakeMarket` has switches: tamper manifest/asset/terms, wrong holder, revoked receipt, stale or replayed head, dropped page, key change (valid, one-signature, unconfirmed), takedown, hostile listing text, oversize body, endless asset stream, redirect, slow/failing responses. The client takes a `Clock`. Matrix over §3:

| Mutation | Expected |
|---|---|
| manifest byte flipped / listing `manifest_hash` changed / terms text edited | `Tampered` |
| receipt for another holder / other item / out of range / unsigned by the pinned key / terms differ | `NoEntitlement` |
| receipt in the revocation chain, including a page 1 000 pages back | `Revoked` |
| old signed head on a fresh machine / cached head older than 14 days / head `issued` over 24 h old | paid `Stale`; free installs, labelled |
| offline with a cached fresh head | paid install passes; "checked <time>" shown |
| a held revocation page omitted / a different page at a held index | kept and warned / `Tampered(Record)` |
| takedown of the manifest hash (paid and free) / applied record afterwards | `TakenDown` / unchanged |
| market key changed: both signatures / one signature / not confirmed | accepted after confirmation / refused / `MarketKeyChanged` |
| receipt signed by a retired key | passes |
| listing `commercial: ok`, an asset `commercial: no` in `asset_info` | `LicenceMismatch` (manifest and listing both say ok; the join says no) |
| manifest `commercial: ok`, an asset `no`, the listing honestly says `no`, `publish_ready` on / off | `NotCleared` / installs |
| `private_test` on the item or an asset | `PrivateTest` |
| missing attestation / custom licence without `terms_hash` | `Malformed` |
| incompatible, deprecated, no receipt, over item limits | refused with **no asset request recorded** |
| more than 64 assets / over 1 GiB total / one asset 256 MiB + 1 / endless stream | `TooLarge`, staging deleted |
| one of three assets tampered | nothing in My library, staging deleted |
| crash after staging, before promote | no item, no partial store entry; staging cleaned on start |
| title over 80, markup, control chars | stripped; over limits `TooLarge` |
| preview path with `..`, `//`, a scheme | `Malformed` |
| redirect to another host / header set on every route | refused / exactly the fixed set |
| free item with `commerce` set | installs; `commerce` never read |
| entitlement stored with the item; `library.apply` | sidecar present; record holds only the hash |

The suite is `omuzaki_market_mock::conformance::<M: Market>(factory)`; goldens are canonical JSON with deterministic ed25519.

## 8. Open questions (the owner's; options, no recommendation)

Carried from library-sharing Q1 to Q3, now with what this design needs:
- **Q1. Who runs a backend** (none, the maintainers, or a partner) and whether a market key ships built in. The client needs only a base address and a pinned key.
- **Q2. Payments:** whether any item needs an entitlement, who issues receipts and handles refunds. The client verifies receipts and ignores `commerce`.
- **Q3. Moderation and takedown:** who signs a takedown, what appeals exist, how reports are handled. The client sends reports and honours a takedown page signed by the pinned key.
- **Q10. Which licence templates ship** beyond `CC0-1.0` and `CC-BY-4.0`, and whether custom `LicenseRef-` text is allowed.
- **Q11. Packs:** whether a later extension adds them (a collection type, a receipt that covers a set, an atomic multi-item install). Not in this design.
- **Q12. App-version requirement:** whether items should declare the app version they need (a manifest contract change; D2 explains why it is left out).

## 9. Sources

Read on 2026-10-15: `docs/design/library-sharing.md` (D2 to D4, D7, D9 to D11, D14, §5), `docs/design/library.md` (D8, D9), `docs/design/identity.md` (D1, D3 to D6, D10), `crates/omuzaki-library-api/src/{manifest,rights}.rs` (fields), `docs/design/ux/design-system.md` (Listbox), `docs/design/l10n-ops.md`; ADR 0057; Open VSX (Eclipse, EPL-2.0) and Flatpak documentation via web search.
