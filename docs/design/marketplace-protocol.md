---
title: "The marketplace protocol: signed documents, freshness, HTTP shape and limits"
summary: "the marketplace wire detail: market-signed documents (receipt, head, append-only revocation and takedown pages, key changes), the freshness rule, the HTTP shape, the fixed header set, redirects, limits (ADR 0063)."
area: design
---

# The marketplace protocol: signed documents, freshness, HTTP shape and limits

**Status:** accepted. **Part of:** `docs/design/marketplace.md` (decisions, contract, task cut) and ADR 0063. This page holds only the wire-level detail so the main design can stay about decisions. **Builds on:** `docs/design/identity.md` (D3 envelope, D4 trust, D5 key statements, D10 domains), `docs/design/library-sharing.md` (D9 limits, D11).

## 1. Market-signed documents

Every document the market signs is `Signed<_>` under `Domain::Entitlement`; the table says which keys each accepts (the **current pinned key**, or a **retired** key connected to it by confirmed rotations, §4). The domain set stays closed (identity D1); identity D3 allows several body types under one tag when each is `deny_unknown_fields` and has a required field no other has. Here that field is `doc`, a fixed string (`"receipt"`, `"head"`, `"revocations"`, `"takedowns"`), plus `market: KeyId`: the key that signed this document, which must equal the signature's key, so a document can't be lifted to another market that reuses text and the verifier knows which key to check (§4). A test proves no document parses as another.

| `doc` | Body | Purpose | Accepted signer (all `Domain::Entitlement`) |
|---|---|---|---|
| `receipt` | `EntitlementBody { doc, market, listing, item, versions, holder, order, issued, terms_hash: Option<ContentHash> }` | one buyer's right to one item (main D3) | current or retired |
| `head` | `MarketHead { seq: u64, issued: u64, revocations: Tip, takedowns: Tip }`, `Tip { pages: u32, tip: Option<ContentHash> }` | the market's current state, small and cheap to fetch (§2) | **current only** |
| `revocations` | `MarketPage { index: u32, prev: Option<ContentHash>, entries: Revoked(Vec<ContentHash>) }` | receipt hashes that no longer count | current or retired, and only when reached from a current-key head |
| `takedowns` | `MarketPage { …, entries: Taken(Vec<Takedown>) }`, `Takedown { listing, item, version, manifest_hash, reason: TakedownReason }` | operator removals; `TakedownReason`: `Rights`, `Safety`, `Fraud`, `Legal`; an entry is at most 400 bytes | same as `revocations` |

`KeyChange` statements are the one exception: they are `Signed<KeyStatement>` under `Domain::KeyStatement` (§4), not market documents.

**Pages are an append-only hash chain with no lifetime cap.** A page holds 1 to 4 096 entries (at most 2 MiB; 4 096 hash entries are about 280 KB) and is never edited, so a page is never empty; `prev` is the `Signed::hash()` of the page before it and `index` is `prev.index + 1` (0 for the first). The head's `Tip` names the newest page and how many there are. **An empty chain is `pages = 0` with `tip = None`**; `pages >= 1` always has a `tip`, and any other combination is `Malformed`. No empty page exists, so a market with no revocations serves none. The market appends a new page for each batch; there is no compaction, so a refund stays revoked forever (receipts never expire) at 32 bytes per entry. The client walks from the head's tip back until it reaches a page it already holds, verifying each (signature under the key its `market` field names, which must be an accepted key (§4); `doc`; `index` and `prev` agree; the chain reaches index 0 or a held page, `pages` equals the tip's `index + 1`), then keeps every page it verified. It fetches at most 64 pages per call and resumes where it stopped. A held page the market no longer serves is kept and warned (`MissingRecord`, as sharing D9); a different page at a held `index` is `Tampered(Record)`. Takedown pages are the same, so the one verifier covers both; the key-change list (§4) is paged the same way.

**What a takedown does:** once the client has synced the page, it blocks **new installs and updates** of that `manifest_hash`, paid or free, with `TakenDown`. It never removes or disables an applied record (sharing Q6). The client matches an entry by `manifest_hash` only; `listing`, `item`, `version` and `reason` are shown to the person, never used in a decision. A creator's own deprecation stays in the signed manifest (`deprecated`) and needs no page.

## 2. Freshness: how the client knows the lists are current

A cached `seq` cannot tell a fresh machine which list is newest, so freshness comes from the head. The client **establishes freshness** when, during this attempt, it fetched `GET /v1/head`, the head verifies under the pinned key, its `seq` is at least the cached head's, and its `issued` is within `library.market.head_max_age_h` (default 24) of the client's clock, after which it has synced and verified every page the head names (a page that fails verification means freshness is not established). `issued` is the market's claim and is used only as a **replay bound on a market-signed head**; it never accepts a key, a statement or a receipt (identity D4: no timestamps decide trust). A skewed clock gives `Stale` with the words "check your clock".

| Situation | Paid item (`access = required`), **new install or update** | Free item |
|---|---|---|
| Head fetched in this attempt and fresh | proceeds | proceeds |
| Offline, head fetch fails, head `issued` too old (a replayed list), or `seq` below the cached one | refused: `Stale`, and the panel says why ("can't check that this purchase is still valid; connect to the market and try again") | proceeds from the pages already held, labelled "takedowns not checked" |
| No cached head (fresh machine, first use) | refused: `Stale` (same words) | proceeds, labelled "takedowns not checked" |

**A paid install or update never uses a cached head.** The 24 h age rule applies to the head fetched in that same attempt, so a refund the market has published is seen by the next paid install or update, and a head up to `head_max_age_h` old is the only window in which a replayed list can precede it. With no network a paid item cannot be newly installed or updated. **Items already installed keep working offline**: nothing is revoked on the machine retroactively; a refund or takedown affects new installs and updates only. A free item can be installed before a takedown the client has not yet synced; the label says so. There is no `offline_days` setting.

## 3. HTTP shape

JSON bodies, fixed paths so the loopback fake and any backend agree. The scheme is `https`; `http` is accepted only for a loopback address (the fixture).

| Request | Response | Auth |
|---|---|---|
| `GET /v1/search?q&kind&target&page` | `ListingPage` (≤ 50 listings) | none |
| `GET /v1/listings/{id}` | `Listing` | none |
| `GET /v1/listings/{id}/preview/{n}` | image, `n` < 5 | none |
| `GET /v1/listings/{id}/items/{version}/manifest` | manifest bytes | none |
| `GET /v1/terms/{hash}` | custom licence text, UTF-8 | none |
| `GET /v1/assets/{hash}` | asset bytes (streamed) | none |
| `GET /v1/head` | `Signed<MarketHead>` | none |
| `GET /v1/pages/{doc}/{hash}` | `Signed<MarketPage>`, `doc` is `revocations` or `takedowns` | none |
| `GET /v1/key-changes?before={n}` | `KeyChangePage` (≤ 64 links, §4) | none |
| `GET /v1/receipts/{listing}` | `Signed<EntitlementBody>` or 404 | token |
| `PUT /v1/assets/{hash}`, `POST /v1/listings` (publish) | `ListingId` | token |
| `POST /v1/listings/{id}/report`, `GET`/`POST /v1/listings/{id}/reviews` | report / review page (≤ 50) | token for `POST` |

Everything the install and freshness checks need is unauthenticated, so checking for takedowns never ties a browsing session to an account. `{id}` is `[a-z0-9-]{1,64}`, `{hash}` 64 lowercase hex; a response that names any other path (a preview, a link) must start with `/v1/` and contain no `..`, `//`, `?`, `#` or scheme, or the listing is `Malformed`.

**The fixed request-header set.** A request carries exactly these and no others:

| Header | When | Value |
|---|---|---|
| `Host` | always | the pinned base host |
| `Accept` | always | `application/json` for JSON routes; `application/octet-stream` for assets, manifests, terms and previews |
| `Accept-Encoding` | always | `identity` (no compression, so limits count wire bytes) |
| `User-Agent` | always | the constant `omuzaki-market` (no version, OS or machine) |
| `Authorization` | only on the `token` rows | `Bearer <token>` from the secret store (CN-05) |
| `Content-Type`, `Content-Length` | `PUT`/`POST` only | `application/json` (`application/octet-stream` for `PUT /v1/assets`), and the body length |

Never sent: `Cookie`, `Referer`, `Origin`, `Range`, `If-*`, `Accept-Language`, any `X-*`, or anything naming the client, install, order or holder. Transport-level headers a library adds itself (`Connection`) carry no identifier. A response with `Content-Encoding` other than `identity` is `Malformed`; `Set-Cookie` is ignored and nothing is stored from it. T3's tests run against a recording server that asserts the exact header names per route.

**Redirects.** A `301/302/307/308` to a path on the same scheme, host and port is followed at most twice, with the same fixed set (the `Authorization` header is sent again only on a `token` route to that same origin). A redirect to another host, port or scheme, an `https` to `http` downgrade, or a third hop is refused (`Rejected`).

**Statuses:** 404 `NotFound`; 401/403 `Forbidden`; 413 `TooLarge`; 429 and 5xx `Unavailable`; any other 4xx `Rejected`.

## 4. Market key changes

A market key rotates by identity's own rule (D5): `KeyStatement::Rotate { old, new }` is valid only when **both** keys sign the same body (two `Signed` values, one body). The market publishes them at `GET /v1/key-changes` as `KeyChange { old: Signed<KeyStatement>, new: Signed<KeyStatement> }`: an **append-only list** paged like the page chains, with at most 64 links (64 KiB) per call, newest first, and a `next` cursor (`?before={n}`) the client resumes from. Link `i` is the `i`-th rotation; there is no cap on the number of rotations. The client never swaps a pin by itself:
1. a **forward** change is accepted only when `old` is the currently pinned key, both signatures verify under `Domain::KeyStatement`, and the bodies are equal;
2. a document signed by a key that is not the pin makes the client look at `key-changes`; a valid change shows the person "<name> changed its key (old fingerprint → new fingerprint)" and the panel offers `library.market.rekey` (person-only), which writes the new pin and keeps the old as **retired** in `market.toml`;
3. until the person confirms, paid installs from that market fail `MarketKeyChanged`; free installs and everything installed keep working;
4. a **retired** key (identity D5: a rotated key is retired, not revoked) still verifies the documents it signed while it was current, so no page or receipt changes its hash. **Receipts** are accepted from the current key or any retired key in the chain. **Revocation and takedown pages** are accepted when signed by the current key or by any retired key connected to the pin through confirmed rotations, using the key named by the page's own signed `market` field (the key id is the choice, not the issue time, which only the market claims). A **head** must be signed by the current key, and a page is trusted only when it is reached by hash from such a head's tip, so a retired key cannot append pages by itself;
5. **a fresh client** pins the current key when the person adds the market, then learns the older keys from `GET /v1/key-changes`. A **backward** link is accepted when its `new` is the pin or an already accepted retired key, both signatures verify under `Domain::KeyStatement` and the bodies are equal. It walks the list page by page from the pin back to the first link, however many rotations there have been. The panel shows the whole chain with fingerprints and `library.market.rekey` (person-only) writes it to `retired` after the person confirms. Until then, paid installs fail `MarketKeyChanged` and pages signed by an unconfirmed key are not accepted (free installs proceed from the pages that verified, labelled); once confirmed, each held or fetched page is verified against the key its `market` field names, and a page whose key is not in the chain is `Tampered(Record)`;
6. a statement can also be imported from a file (`trust.import`, identity D5). A compromised key is not a rotation: the person removes the market and adds it again with the new fingerprint.

## 5. Limits

Constants in `omuzaki-market-api`, tested at the limit and over; every read stops at the limit plus one, so an untrusted market cannot make a call allocate more (sharing D9).

| Thing | Limit |
|---|---|
| any JSON response (except pages) | 1 MiB; a `Listing` 16 KiB (its `commerce` 4 KiB); a head 4 KiB; a page 2 MiB and 4 096 entries |
| manifest | the library's manifest limit |
| custom licence text | 64 KiB, UTF-8, control characters other than newline and tab refused |
| preview | 5 per listing, 5 MiB each |
| attribution (effective rights) | 1 KiB a string, 8 KiB the joined set, 64 distinct strings; over any is `AttributionTooLarge`, never truncated (main D5) |
| asset | 256 MiB each; **per item** at most 64 assets and 1 GiB in total, both read from the signed manifest before any download (main D5) |
| pages or key-change links per call | 64 each, resumable; verified pages and links kept without a cap |
| search page / review page | 50 listings / 50 reviews |
| timeouts | connect 10 s; any non-asset body 30 s in total; an asset stalls out after 30 s without a byte (no overall cap, cancellable) |
