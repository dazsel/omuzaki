---
title: "0063: The marketplace client trusts signatures, not the store; listings are advice and receipts are checked locally (2026-10-15)"
summary: "The marketplace client trusts signatures, not the store; listings are advice and receipts are checked locally"
area: decisions
---

# 0063: The marketplace client trusts signatures, not the store; listings are advice and receipts are checked locally (2026-10-15)

**Status:** proposed; the hosted backend, payments and moderation (Q1 to Q3) remain the owner's. It adds a new API crate, `omuzaki-market-api`, and one additive contract task on `omuzaki-library-api` (signed `attestation` and `terms_hash` on `ItemRights`, `asset_info` on the manifest (each entry the item's own `ItemRights` type plus a size), `receipt` on the application record). **Context:** LB-08, SE-07; spec v3 §28, §31, §37, §38; ADR 0042 (identity), ADR 0057 (library sharing). Design: `docs/design/marketplace.md`; wire detail: `docs/design/marketplace-protocol.md`.

## Context

ADR 0057 D11 said the marketplace reuses the shared-source protocol as `MarketStore: ItemStore` and left the store split, listing metadata, entitlements and takedowns open. The best open-source extension-store client examined (Open VSX, EPL-2.0) trusts the registry's answer; Flatpak (LGPL-2.1-or-later) trusts one signing key per remote and records no creator or licence. Neither gives the client a way to check who made an item, what it may be used for, or whether a purchase is still valid.

## Decision

1. **A market is not an `ItemStore`.** It has no log, roster, licence file or index, so `Market` (listing, manifest, asset, receipt, revocations, publish, report, reviews) is its own trait. The byte checks are the pure functions of `omuzaki-library-source-api::verify`, shared by both transports. This refines ADR 0057 D11.
2. **A listing is advice.** The client re-derives hash, creator, id, version, rights (including the asset rights) and kind from the signed manifest; any mismatch is refused (`Tampered(Listing)`, `LicenceMismatch`). Listing text is plain, bounded and labelled "from the store".
3. **A receipt is a `Signed<EntitlementBody>` under `Domain::Entitlement`**, checked locally against a pinned market key (or a retired pin; a paid install still needs a fresh head, point 4): holder equals the person's identity key, item and version in range, terms hash equals the manifest's, not in the revocation chain. No expiry; `issued` is display only. A refund stops new paid installs and updates once it is in a page the head names, never an item already installed or an applied record (ADR 0057 Q6). Single items only: packs are a later extension.
4. **Market-signed documents share the `Entitlement` domain** (receipt, head, revocation and takedown pages), told apart by a required `doc` field. Revocations, takedowns and market key changes are **append-only signed lists, paged and with no lifetime cap** (64 per call; any number of rotations). A signed **head** gives freshness: a paid install or update needs a head fetched in that same attempt and recent (a cached head never counts); otherwise `Stale`, and installed items keep working offline. A paid item cannot be newly installed offline, even with an imported receipt; a free item can, labelled when takedowns are unchecked. A market key rotates only with both keys' signatures and the person's confirmation; pages signed by retired keys in the confirmed chain stay valid, and an empty chain is `pages = 0`.
5. **Install order is fixed** (design D5): listing, freshness, manifest, signature and trust, then every cheap refusal (compatibility, status, takedown, entitlement, item limits) and the rights join (one table, design D5; an asset's `terms_hash` must match the item's) and item-level gate, all from signed metadata **before any asset is requested**; then streamed, hash-verified staging and one atomic promote. The signed receipt is stored with the installed item in user data; only its hash goes into an application record.
6. **Licence enforcement is provenance plus the existing gate**, not copy protection. The record carries creator, licence, attribution, commercial, grant and the receipt hash; `io.check_export` and signed exports (SE-07) are unchanged. A Machine setting `library.market.publish_ready` (off) runs the gate at install.
7. **`commerce` is opaque.** Prices and currencies, if a backend has them, live inside it; the client never reads it, never stores it in a project and never exports it.
8. **The creator's preflight** adds to ADR 0057's six checks: `commercial: ok`, no `private_test`, provenance on every asset (copied into the signed manifest), a signed creator attestation, a licence template or custom terms by signed hash; templates that forbid commercial use are not offered.
9. **Privacy:** one pinned base address and key, added by a person; no telemetry, ids, cookies or referrers; a fixed, enumerated request-header set; listings hold paths, never hosts; redirects to another origin are refused. Listings carry no app-version requirement (the signed manifest has none).
10. **Tests use a `FakeMarket`** (in-process and over a loopback HTTP server) with a conformance suite; the real client never needs a network to be tested.

## Consequences

- Six tasks follow (design §5.3); T1 needs the Director's approval of the new API crate and the root `Cargo.toml` hub sign-off, T2 (the additive `omuzaki-library-api` contract) the Director's approval and lands first.
- A hostile or buggy store can refuse to serve, but cannot substitute an item, a licence or a receipt.
- A copied item file keeps its rights and provenance but is not protected from copying; the design says so.
- Open, the owner's (design §8): Q1 hosted backend, Q2 payments, Q3 moderation, Q10 which licence templates ship, Q11 packs, Q12 an app-version requirement. Tasks needing a real backend wait on them; the client is built and tested against `FakeMarket`.
