---
title: "0063: The marketplace client trusts signatures, not the store; listings are advice and receipts verify offline (2026-10-15)"
summary: "The marketplace client trusts signatures, not the store; listings are advice and receipts verify offline"
area: decisions
---

# 0063: The marketplace client trusts signatures, not the store; listings are advice and receipts verify offline (2026-10-15)

**Status:** proposed; the hosted backend, payments and moderation (Q1 to Q3) remain the owner's. It adds a new API crate, `omuzaki-market-api`, and later an additive `receipt` field on the library's application record. **Context:** LB-08, SE-07; spec v3 §28, §31, §37, §38; ADR 0042 (identity), ADR 0057 (library sharing). Design: `docs/design/marketplace.md`.

## Context

ADR 0057 D11 said the marketplace reuses the shared-source protocol as `MarketStore: ItemStore` and left the store split, listing metadata, entitlements and takedowns open. The best open-source extension-store client examined (Open VSX, EPL-2.0) trusts the registry's answer; Flatpak (LGPL-2.1-or-later) trusts one signing key per remote and records no creator or licence. Neither gives the client a way to check who made an item, what it may be used for, or whether a purchase is valid without the network.

## Decision

1. **A market is not an `ItemStore`.** It has no log, roster, licence file or index, so `Market` (listing, manifest, asset, receipt, revocations, publish, report, reviews) is its own trait. The byte checks are the pure functions of `omuzaki-library-source-api::verify`, shared by both transports. This refines ADR 0057 D11.
2. **A listing is advice.** The client re-derives hash, creator, id, version, rights, kind and requirements from the signed manifest; any mismatch is refused (`Tampered(Listing)`, `LicenceMismatch`). Listing text is plain, bounded and labelled "from the store".
3. **A receipt is a `Signed<EntitlementBody>` under `Domain::Entitlement`**, checked offline against a pinned market key: holder equals the person's identity key, item and version in range, not in the newest signed revocation list. No expiry; `issued` is display only. A refund stops new installs and updates, never an applied record (ADR 0057 Q6).
4. **Install order is fixed** (design D5): listing, manifest, item signature and trust, assets, compatibility, status, receipt, licence and gate, then write. Nothing is written on a refusal.
5. **Licence enforcement is provenance plus the existing gate**, not copy protection. The record carries creator, licence, attribution, commercial, grant and the receipt hash; `io.check_export` and signed exports (SE-07) are unchanged. A Machine setting `library.market.publish_ready` (off) runs the gate at install.
6. **`commerce` is opaque.** Prices and currencies, if a backend has them, live inside it; the client never reads it, never stores it in a project and never exports it.
7. **The creator's preflight** adds to ADR 0057's six checks: `commercial: ok`, no `private_test`, provenance on every asset, a creator attestation, a chosen licence template; templates that forbid commercial use are not offered.
8. **Privacy:** one pinned base address and key, added by a person; no telemetry, ids, cookies or referrers; listings hold paths, never hosts; redirects to another host are refused.
9. **Tests use a `FakeMarket`** (in-process and over a loopback HTTP server) with a conformance suite; the real client never needs a network to be tested.

## Consequences

- Five tasks follow (design §5.3); T1 needs the Director's approval of the new API crate and the root `Cargo.toml` hub sign-off, T3 of the additive record field.
- A hostile or buggy store can refuse to serve, but cannot substitute an item, a licence or a receipt.
- A copied item file keeps its rights and provenance but is not protected from copying; the design says so.
- Open, the owner's (design §8): Q1 hosted backend, Q2 payments, Q3 moderation, Q10 which licence templates ship. Tasks needing a real backend wait on them; the client is built and tested against `FakeMarket`.
