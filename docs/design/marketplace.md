---
title: "The marketplace client: browse, install, publish, receipts and licences"
summary: "the marketplace client: listings, signed receipts verified offline, install order and licence gate, publish preflight, reports, privacy (ADR 0063)."
area: design
---

# The marketplace client: browse, install, publish, receipts and licences

**Status:** proposed. **Feature:** LB-08 (and SE-07 for what travels in an export). **Decision record:** `docs/decisions/0063-marketplace.md`.
**Spec:** v3 §28 (LB-08), §31 (SE-07), §37, §38 (open question 5).
**Builds on:** `docs/design/library-sharing.md` (D2 to D4, D7, D11, D14; ADR 0057), `docs/design/library.md` (D2 ids, D5 the record is the lock, D8 rights and the gate, D9 install), `docs/design/identity.md` (D3 envelope, D4 trust, D6 signed exports, D10 domains).

This page fixes what the **client** does and what it needs from a backend, so a `FakeMarket` can stand in for tests. It adds no signing scheme and no verification rule: a listing is advice, the signed item is the truth, and a receipt is one more signature checked against a key the person pinned. Who runs a backend, payments and moderation are the owner's (§8) and are not decided here.

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
3. **A receipt is verified offline** and bound to the holder's key, so a stolen file is useless and a lost connection costs nothing (D3).
4. **No tracking by construction:** a listing carries paths, never hosts; the client contacts one pinned base address (D10).
5. **What was installed stays installed:** the project holds what it pinned, with its rights and receipt hash, and runs with the store gone or the receipt expired.

## 2. The shape of it

```
 person's machine                                         market (hosted or fake; untrusted bytes)
┌─────────────────────────────────────────┐   HTTPS   ┌────────────────────────────────────────┐
│ Browse panel / ops (library.market.*)    │◀────────▶│ search  listing  manifest  assets      │
│ Market (omuzaki-market-api)              │  JSON +   │ receipt  revocations  report  reviews  │
│   verify: listing↔manifest, item sig,    │  bytes    └────────────────────────────────────────┘
│   assets, receipt, gate, compatibility   │
│ installs into My library (library.install)│   pure verification is shared with
└─────────────────────────────────────────┘   omuzaki-library-source-api::verify
```

A market moves bytes and text. The client decides everything that matters.

## 3. Worked example (on disk)

The project's own fixtures only: market key `fake-market` and keys for Ben (creator) and Cleo (buyer) come from test seeds (`MemorySigner::from_test_seed`); item `ben/hair/bob@1.0.0` is the hair item of the sharing example. `<64 hex>` stands for a real hash; the T1 mock generates this tree and the tests check it byte for byte.

```
market-fixture/
  market.toml                       base = "http://127.0.0.1:<port>", key = "<fake-market key>", name = "FakeMarket"
  listings/lst-0001.json            ben/hair/bob@1.0.0, premium (commerce present)
  listings/lst-0002.json            ben/hair/rough@1.0.0, free, manifest says commercial: no
  items/ben/hair/bob/1.0.0.toml     signed manifest (Ben)
  items/ben/hair/rough/1.0.0.toml
  receipts/lst-0001.json            Signed<EntitlementBody> by the market key, holder = Cleo
  revocations/000001.json           Signed<RevocationList>, empty
```

`listings/lst-0001.json` (a `Listing`; every text is data to display, never to trust):

```json
{ "id": "lst-0001", "item": "ben/hair/bob", "version": "1.0.0", "manifest_hash": "<64 hex>",
  "creator": "<Ben key>", "title": {"en": "Bob hair"}, "summary": {"en": "A short bob for the kit characters."},
  "kind": "hair", "targets": ["vrm1"], "requires": ">=0.9",
  "licence": { "spdx": "CC-BY-4.0", "commercial": "ok", "attribution": "Ben", "grant": "licensed", "terms_hash": "<64 hex>" },
  "previews": ["/v1/listings/lst-0001/preview/0"], "stats": {"count": 0, "mean_x10": null},
  "access": "required", "commerce": {"kind": "fixture", "ref": "order-0001"} }
```

`receipts/lst-0001.json` (`Signed<EntitlementBody>`, `Domain::Entitlement`, signed by the market key):

```json
{ "body": { "listing": "lst-0001", "item": "ben/hair/bob", "versions": ">=1.0.0, <2.0.0", "holder": "<Cleo key>",
            "order": "order-0001", "issued": 1, "terms_hash": "<64 hex>" },
  "signer": { "name": "FakeMarket", "key": "<fake-market key>" }, "signature": "<128 hex>" }
```

**Install that passes.** Cleo runs `library.install {from: market, listing: lst-0001}`. The client fetches the manifest and checks, in D5's order: manifest hash equals the listing's; Ben's `LibraryItem` signature holds; assets match; compatibility holds; no deprecation; the receipt verifies offline (market key pinned, `holder` is Cleo's identity key, item and version in range, not in the cached revocation list); the effective rights equal the listing's licence; the gate passes. The key is `Unknown`, so Cleo sees "Trust this key…" and the per-item acknowledgement (library.md D8), then the item lands in My library. Its record holds the rights (`CC-BY-4.0`, `commercial: ok`, attribution "Ben", `grant: licensed`) and `receipt: <hash8 of the receipt>`. Nothing else of the order is stored in the project.

**Install the licence gate refuses.** `lst-0002` is free, but its manifest says `commercial: no`. Two refusals:
- With the Machine setting `library.market.publish_ready = true` (off by default), `library.install` fails with a located finding `listing lst-0002`: "ben/hair/rough 1.0.0 is not cleared for published work (commercial: no)", hint "turn off publish-ready to install it for personal use". This is `licence.derive` unchanged (library.md D8), run early.
- If the listing had said `commercial: ok` while the manifest says `no`, the install is `Refused(LicenceMismatch)` always: the store misdescribes the item, whatever the setting. Both are rows in §7's matrix.

A purchase refused for no receipt (`lst-0001` without `receipts/lst-0001.json`, or with one for another holder) is `Refused(NoEntitlement)`, before anything is written.

## 4. Decisions

### D1. Position: `Market` is the listing layer; bytes verify through the shared pure code

`MarketStore: ItemStore` (ADR 0057 D11) is **refined**: a market has no log, no roster, no licence file and no index, so most of `ItemStore` would be dead methods. Instead `Market` (§5.1) returns listings, a manifest's bytes, assets and receipts; the checks on those bytes are `omuzaki-library-source-api::verify` (manifest hash, `LibraryItem` signature, asset hash and size, limits), called on both paths. One verifier, two transports. This is the "store split" ADR 0057 left open.

### D2. The listing is advice; the signed item is the truth

A `Listing` is data the backend serves. The client treats every field as a claim to be checked against the signed manifest before it affects anything:
- `manifest_hash` must equal `ItemManifest::hash()` of the fetched manifest, `creator` its `publisher.key`, `item`/`version` its id and version (`Tampered(Listing)` otherwise);
- `licence` must equal the manifest's `rights` after the same join the team licence uses (sharing D7): any difference is `LicenceMismatch`;
- `kind`, `targets`, `requires` are display and filters only; the install re-derives them from the manifest.
Text is plain: `title` at most 80 characters, `summary` 500, per language, control characters and markup stripped on parse, never rendered as links. Previews are paths on the pinned base (never hosts), at most 5 MiB each, decoded by the existing image path and shown labelled "from the store, not checked". A pack is a listing whose `items` names several ordinary items; one receipt covers the listing and each item verifies on its own.

### D3. The receipt: a signature the client verifies offline

An entitlement (the receipt) is `Signed<EntitlementBody>` under `Domain::Entitlement` (identity reserved it). `EntitlementBody { listing, item, versions: VersionReq, holder: KeyId, order: String, issued: u64, terms_hash: ContentHash }` is `deny_unknown_fields` with required fields no other body has (identity D3). It is checked in this order, with no network:
1. signed by the **pinned market key** (`market.toml`; a key rotation is an identity `KeyStatement::Rotate` signed by the old key);
2. `holder` equals the key of the identity slot the person installs with (a receipt is useless to anyone else; the order and holder never leave the machine, D8);
3. `item` and `listing` equal the listing's, the version is in `versions`, `terms_hash` equals the listing's licence terms;
4. its hash is not in the newest verified **revocation list** (`Signed<RevocationList { seq, revoked: Vec<ContentHash> }>`, same domain, own body type; at most 10 000 entries, 1 MiB). `seq` never goes down. If the client is offline the cached list is used and the panel says "revocations checked <time>"; there is no expiry, so an old receipt keeps working offline.
`issued` is the market's claim and only displayed (identity D4: no timestamps decide trust). A refund revokes: it stops **new installs and updates**, never an applied record (sharing Q6: no silent recall). A receipt can also be imported from a file (`library.market.import_receipt`), so a gift or an offline machine works; an imported receipt passes the same four checks.

### D4. Entitlement is the listing's, the client only checks it

`access` is `free` or `required`. For `free`, install needs no receipt. For `required`, install without a valid receipt is `NoEntitlement`. What `commerce` means, how a receipt is earned and what is charged are not the client's: it is opaque JSON, never parsed, never used in a decision, and never written into a project. Prices and currencies, if a backend has them, are inside `commerce`.

### D5. What an install checks, and in which order

`library.install {from: market, listing}` is person-only with the per-item acknowledgement (library.md D8, D9) and needs the market's `net` scope. Order: cheapest and least-trusting first, nothing written until the end.
1. `Listing` parses within limits (16 KiB); the item is not on the person's local hide-list.
2. Fetch the manifest (size-limited); `manifest_hash`, id and creator match (D2).
3. `LibraryItem` signature verifies for `creator`; identity `Check` is not `Revoked` (`Unknown` offers "Trust this key…", `Tampered` refuses).
4. Every asset is fetched by hash and matches its size.
5. **Compatibility:** `ItemManifest::check`, `requires` against the app version, every `target` and op available. A failure says which op or version is missing.
6. **Status:** not deprecated, and no takedown record (a `Deprecate` signed by the pinned market key, sharing D11); a deprecated item can still be installed when a project already pinned it.
7. **Entitlement** (D3), only if `access = required`.
8. **Licence:** the manifest's rights equal the listing's licence (`LicenceMismatch`); `private_test` is refused (`PrivateTest`); then the gate: with `publish_ready` on, `licence.derive` must pass (`NotCleared`).
9. Write: manifest and assets into My library; the application record's rights are the declared rights joined with each asset's (library.md D8); `receipt` holds the receipt's hash.

Each refusal is a located finding with a hint, an op record, and nothing partial on disk (steps 1 to 8 touch memory only).

### D6. Licence enforcement is provenance and the gate, not copy protection

An item is data; once on a machine it can be copied. The design says so and does not pretend otherwise. What it does:
- **Provenance.** Rights travel in the item and into the record: `author` (creator), `licence` (SPDX or `LicenseRef-`), `attribution`, `commercial`, `grant`, and the `receipt` hash. A purchased item is `grant: licensed` unless its own rights transfer ownership (the team-licence rule); the market can't upgrade it.
- **The gate** (`io.check_export`, library.md D8) blocks a `publish` export of a character whose records are `commercial: no|unknown` or `private_test`. Nothing new: the market adds no rule to the gate.
- **Signed exports (SE-07).** The export statement already commits to the licence claims; this design adds only that the claims name the creator's key and licence, so a third party can verify who made a part and under what terms. The receipt hash and the order are not exported (D8).
- **Honest limit.** A person who strips records or edits a file is outside this system's reach; the design gives honest people clear answers and gives anyone else's claims a verifiable origin.

### D7. Publish: the creator's preflight

`library.market.publish {item, meta}` is person-only, needs `publish`, `edit:library` and the market's `net` scope, and agents get `needs_person`. A dry-run returns one finding list; nothing leaves the machine until the person confirms and sees exactly what is sent. It extends sharing D10's six checks (valid manifest, signature made and verified before upload, version bump matches the diff) with:
1. rights complete, **`commercial: ok`** (a market listing for published work can't be a trap), no `private_test` on the item or any asset, previews rendered from the item's recipe on the kit (no user character);
2. every asset has provenance with a licence; none contradicts another record in the person's store for the same hash. The client cannot detect third-party IP it has not been told about, so the creator **attests** in the signed manifest rights (`asserted_by` is the creator): "I made this or hold the right to licence it", shown to the person before signing;
3. a **licence template** chosen from the shipped list (`CC0-1.0`, `CC-BY-4.0`) or a custom `LicenseRef-` text with `terms_hash`; templates that forbid commercial use are not offered;
4. title, summary and language tags within D2's limits; the listing `licence` is generated from the manifest, never typed twice.
The order is assets, then manifest, then the listing metadata, so a crash leaves unlisted bytes the next publish completes (sharing D10).

### D8. Reviews, ratings and reports are client-side requests, not trust

- **Ratings and reviews** are shown as `stats` and a page of `Review { author_label, rating 1..5, text, lang }`, all labelled "reported by the store, not checked". They never enter a decision. Posting (`library.market.review`) is person-only, allowed only for an item the person has installed, and sends the listing id, a rating and at most 1 KiB of text; the backend decides the rest.
- **Report** (`library.market.report`) sends the listing id, a `ReportReason` code (`Rights`, `Safety`, `Spam`, `Broken`, `Other`) and at most 1 KiB of note, after showing the person exactly that. No key, no machine details. The client then offers a local **hide** (the listing or the creator's key) that contacts nothing.
- **Free items and premium packs** differ only in `access`; nothing else in the flow changes.
- **What travels where:** the order, the holder key and the receipt are used on the machine and never written into a project or an export; `commerce` is never read by a decision.

### D9. What is in the free client and what is hosted

| In the client (this repository) | Hosted (the owner's, §8) |
|---|---|
| `omuzaki-market-api` (types, formats, verification), `FakeMarket` and its conformance suite, the HTTP client, install, preflight, the browse panel, the report and review requests | search index, listings storage, payments, receipts issuance and refunds, ratings storage, abuse handling, the backend itself |
Everything on the right is behind the `Market` trait and the HTTP shape below; swapping a real backend for `FakeMarket` changes no client code.

**The HTTP shape** (JSON, fixed paths so the loopback fake and any backend agree; at most 1 MiB per response except assets): `GET /v1/search?q&kind&target&page`, `GET /v1/listings/{id}`, `GET /v1/listings/{id}/items/{version}/manifest`, `GET /v1/assets/{hash}`, `GET /v1/receipts/{listing}` (authentication is the backend's, a token in the secret store, CN-05), `GET /v1/revocations`, `POST /v1/listings` (publish), `POST /v1/listings/{id}/report`, `GET`/`POST /v1/listings/{id}/reviews`. Redirects to another host are refused.

### D10. Privacy

- **No telemetry** and no tracking of browsing: no request carries a client id, a cookie, a referrer, or an install count. The only contact is the pinned base address; a listing holds paths only, so it can't make the client fetch from another host.
- **A market is added by the person** (`library.market.add`, person-only, shows the base address and the key fingerprint, then pins both). Nothing in a response can pin itself.
- **Searches go to that market only**, are never logged by the client beyond the op record's market and query length, and `library.market.search` is off in an agent session without a `net` grant.
- **Credentials** are in the secret store; logs name the market and the listing, never the token (CN-05).
- **Cache:** listings and previews are cached under the data dir within `library.market.cache_mb` (default 256, least-recently-used); clearing it is one action.

### D11. The panel: accessibility and the six languages

The browse panel (T5) is a search field, a filter row, a result listbox and a detail pane. Results use the design-system Listbox pattern (one Tab stop, arrow keys, AccessKit `ListBoxOption` with title, creator and state in the name), the detail pane is a labelled form, and every state (free, needs receipt, owned, deprecated, refused) is said in words, not only by colour or icon. Interface strings go through the l10n pipeline in all six languages. Listing text is the creator's: `title` and `summary` are maps by language tag, the panel uses the interface language, then `en`, then the first available, and marks "not in your language". The client never machine-translates and never sends a listing to a translation service.

## 5. The contract and the task cut

### 5.1 The contract (names are final; doc comments belong to the implementer)

New API crate **`omuzaki-market-api`**: it needs identity and library types plus listing and receipt formats, none of which belong in the library contract, and its verifier must compile for wasm.

```rust
pub trait Market: Send + Sync {                          // the client's view of a backend, as data
    fn search(&self, q: &MarketQuery) -> Result<ListingPage, MarketError>;
    fn listing(&self, id: &ListingId) -> Result<Listing, MarketError>;
    fn manifest(&self, id: &ListingId, version: &Version) -> Result<Vec<u8>, MarketError>;     // bytes, verified by the client
    fn asset(&self, hash: &ContentHash) -> Result<Vec<u8>, MarketError>;
    fn receipt(&self, id: &ListingId) -> Result<Option<Signed<EntitlementBody>>, MarketError>;
    fn revocations(&self) -> Result<Signed<RevocationList>, MarketError>;
    fn publish(&self, item: &SignedItem, meta: &ListingMeta) -> Result<ListingId, MarketError>;
    fn report(&self, id: &ListingId, report: &Report) -> Result<(), MarketError>;
    fn reviews(&self, id: &ListingId, page: Option<Cursor>) -> Result<ReviewPage, MarketError>;
    fn post_review(&self, id: &ListingId, review: &ReviewPost) -> Result<(), MarketError>;
}
pub struct MarketPin { pub base: String, pub key: KeyId }            // market.toml: both shown, then pinned by a person
pub struct Listing { pub id: ListingId, pub item: ItemId, pub version: Version, pub manifest_hash: ContentHash, pub creator: KeyId,
    pub title: BTreeMap<LangTag, String>, pub summary: BTreeMap<LangTag, String>, pub kind: String, pub targets: Vec<String>,
    pub requires: VersionReq, pub licence: ListingLicence, pub previews: Vec<String>, pub stats: Option<Stats>,
    pub access: Access, pub commerce: Option<serde_json::Value> /* opaque, never read */ }
pub enum Access { Free, Required }
pub struct EntitlementBody { pub listing: ListingId, pub item: ItemId, pub versions: VersionReq, pub holder: KeyId,
    pub order: String, pub issued: u64, pub terms_hash: ContentHash }          // Signed<_> under Domain::Entitlement
pub struct RevocationList { pub seq: u64, pub revoked: Vec<ContentHash> }      // Signed<_> under Domain::Entitlement
pub enum InstallRefusal { Malformed, Tampered(Tamper), NoEntitlement, Revoked, LicenceMismatch, PrivateTest,
    NotCleared, Incompatible(String), Deprecated, TooLarge { what: String, limit: u64 }, NotAllowed(String) }
pub fn verify_install(market: &dyn Market, plan: &InstallPlan, trust: &dyn TrustStore) -> Result<VerifiedInstall, InstallRefusal>;  // D5, pure given the market
pub fn verify_receipt(r: &Signed<EntitlementBody>, pin: &MarketPin, holder: &KeyId, listing: &Listing, rev: &Signed<RevocationList>) -> Result<(), InstallRefusal>;
pub enum MarketError { NotFound, Unavailable(String), Forbidden, TooLarge { what: String, limit: u64 }, Malformed(String), Rejected(String) }
```

`Domain::Entitlement` is already in identity; the two bodies are distinct types under it. `omuzaki-library-api` changes only additively and later: the application record's `receipt: Option<ContentHash>` (T3).

### 5.2 Crates

| Crate | Kind | Holds |
|---|---|---|
| `omuzaki-market-api` | API | §5.1, limits, `verify_receipt`, `verify_install`; wasm-clean |
| `omuzaki-market-mock` | Mock | `FakeMarket` (in-process and over a loopback HTTP server, no network), conformance suite, the §3 builder |
| `omuzaki-market` | Implementation | the HTTP client, the `library.market.*` ops, install, publish preflight |

### 5.3 The task cut (file when the design is accepted)

| # | Task | Model, lane | Fence | Acceptance | Blocked by |
|---|---|---|---|---|---|
| T1 | `omuzaki-market-api` and `omuzaki-market-mock` (`FakeMarket`, loopback server, conformance suite) | sonnet, human lane | the two crates, root `Cargo.toml` (hub sign-off) | every row of §7's matrix has a test; the §3 tree matches goldens byte for byte; loopback serves with no outside network; wasm32 check green | library-sharing T1; the Director's approval of the new API crate on its issue |
| T2 | the HTTP client (`Market` over `ureq`/`reqwest` as the workspace allows) | sonnet | `omuzaki-market` (client module) | conformance suite green against `FakeMarket` over loopback; redirect to another host refused; response over 1 MiB `TooLarge`; no header beyond the fixed set; at limit and over tested | T1 |
| T3 | install with receipt and gate (`library.install {from: market}`, record `receipt`) | sonnet | `omuzaki-market` (install), `omuzaki-library-api` (additive `receipt`), `omuzaki-io-ops` (none unless needed) | the §3 pass and refusal reproduce; D5's nine steps each have a failing case in order; nothing written on any refusal; imported receipt passes the same checks | T1, T2, library-sharing T6 (gate), the Director's approval of the additive contract change |
| T4 | publish preflight (`library.market.publish` dry run) | haiku | `omuzaki-market` (preflight) | all D7 checks and sharing D10's six have a failing case; `commercial: no` and `private_test` refused; agent gets `needs_person` | T1 |
| T5 | the browse panel (search, filters, detail, install, report, hide) | sonnet, visual | `omuzaki-ui` market panel | rendered at the three themes; every state in words; keyboard and AccessKit labels; six languages; check Listbox pattern | T2, T3 |

## 6. Threats

| Attacker | Does | Result |
|---|---|---|
| market / network | swaps a manifest or asset | hash mismatch, `Tampered` |
| market | lies in a listing (licence, creator, hash) | `LicenceMismatch` / `Tampered(Listing)` |
| market | serves an old revocation list | `seq` never goes down; cached list kept |
| someone | reuses a receipt file | `holder` is not the person's key: `NoEntitlement` |
| creator | lists a `private_test` item | refused at preflight and at install |
| market | tracks browsing | one pinned address, paths only, no ids (D10) |
| market | hostile text, huge fields, control characters | limits and stripping at parse (D2) |
| market | redirects to another host | refused |

## 7. Tests (no network)

`FakeMarket` has switches: tamper manifest/asset, wrong holder, revoked receipt, stale revocations, hostile listing text, oversize body, redirect, slow/failing responses. Matrix over §3:

| Mutation | Expected |
|---|---|
| manifest byte flipped / listing `manifest_hash` changed | `Tampered` |
| receipt for another holder / other item / out of range / unsigned by the pinned key / revoked | `NoEntitlement` or `Revoked` |
| revocation `seq` lower than cached | list kept |
| listing `commercial: ok`, manifest `no` | `LicenceMismatch` |
| manifest `commercial: no`, `publish_ready` on / off | `NotCleared` / installs |
| `private_test` | `PrivateTest` |
| title over 80, markup, control chars | stripped; over limits `TooLarge` |
| redirect to another host | refused |
| free item with `commerce` set | installs; `commerce` never read |
| offline with cached revocations | install passes; "checked <time>" shown |

The suite is `omuzaki_market_mock::conformance::<M: Market>(factory)`; goldens are canonical JSON with deterministic ed25519.

## 8. Open questions (the owner's; options, no recommendation)

Carried from library-sharing Q1 to Q3, now with what this design needs:
- **Q1. Who runs a backend** (none, the maintainers, or a partner) and whether a market key ships built in. The client needs only a base address and a pinned key.
- **Q2. Payments:** whether any item needs an entitlement, who issues receipts and handles refunds. The client verifies receipts and ignores `commerce`.
- **Q3. Moderation and takedown:** who signs a takedown, what appeals exist, how reports are handled. The client sends reports and honours a `Deprecate` signed by the pinned key.
- **Q10. Which licence templates ship** beyond `CC0-1.0` and `CC-BY-4.0`, and whether custom `LicenseRef-` text is allowed.

## 9. Sources

Read on 2026-10-15: `docs/design/library-sharing.md` (D2 to D4, D7, D10, D11, D14, §5), `docs/design/library.md` (D8, D9), `docs/design/identity.md` (D1, D3, D4, D6, D10), `docs/design/ux/design-system.md` (Listbox), `docs/design/l10n-ops.md`; ADR 0057; Open VSX (Eclipse, EPL-2.0) and Flatpak documentation via web search.
