# 0057: A shared library source is a bag of immutable signed files; the client verifies everything (2026-10-15)

**Status:** proposed (cloud lead to accept on the PR). It adds a new API crate, `omuzaki-library-source-api`, and later additive changes to `omuzaki-library-api`. **Context:** LB-07, LB-08, VC-10, VC-11; spec v3 §28, §29, §37; ADR 0019 (library items), ADR 0042 (identity), ADR 0043 (team). Design: `docs/design/library-sharing.md`.

## Context

`library.md` D11 calls the team scope "a remote source with a local cache" and the only way to share items, and `ItemScope::Team(id)` is in the contract. Nothing said what a source is, how it is stored and trusted, what a team licence is, or how review state works. The marketplace (LB-08) is the same protocol with a hosted backend, which is not decided.

## Decision

1. **One protocol, several stores.** A source is files: `team/log/`, `items/`, `store/`, `reviews/`, `licence/`, `index/`, laid out like the person's library. A folder is first, an S3-compatible bucket second, a git repository or hosted store later. The only primitive a store needs is create-if-absent. `ItemStore` has no delete.
2. **Storage is untrusted; the client verifies everything** from the team root key the person pinned: the membership log, the signed index chain, each item's signature (`Domain::LibraryItem`), the signer's right to publish at the roster seq the entry was added, assets by hash, and review records (`Domain::Approval`). No new signing scheme: all envelopes and domain tags are identity's.
3. **Versions are immutable** (a second publish of one `id@version` is refused; same bytes are idempotent); delete means a signed deprecation; concurrent writers make a visible fork that a merge snapshot resolves. The index is a listing and a rollback anchor (`seq`, pinned locally), never the truth: review state is always recomputed from signed records.
4. **Review state** is a pure fold of signed records: draft, in review, approved (at least `library.approvals` distinct eligible keys other than the publisher), deprecated. With `apply_only_approved`, the apply op refuses unapproved team items for everyone, agents included.
5. **The team licence** is a file named by `Roster.licence`, set by the owners. An item's effective rights in a team scope are the strictest join of its declared rights and the licence, written into the application record; `outside = blocked` becomes `team_only`, which the publish gate treats like `private_test`.
6. **Sync is explicit, fetch is on demand, and a project never needs the source** once an item is applied.
7. **Agents propose, people decide:** publish, review and deprecate are person-only because they need the person's key.
8. **The marketplace reuses all of this** as `MarketStore: ItemStore`; listing metadata, entitlements and takedowns are extension points. Its hosted backend, payments and pricing are not decided here.
9. **A new API crate**, `omuzaki-library-source-api`, not an addition to `omuzaki-library-api`: it needs identity and team types the library contract should not depend on, and its verifier must compile for wasm.

## Consequences

- Eight tasks follow (design §5.4); the contract tasks need the Director's approval on their issues first.
- `identity.md` and `team.md` refer to this design as 0046 or "planned"; they should read 0057.
- A removed member's earlier items stay valid, a revoked key's never are.
- Integrity is protected, confidentiality is not: whoever can read the shared place reads the items.
- Open questions (design §6) are for the owner (marketplace backend, payments and pricing, moderation) and the cloud lead (drafts, purge, recall, encryption, sync default, git shape).
