---
title: "0042: One ed25519 key per person per machine, in the keychain; one domain-tagged envelope; trust is a person's own list (2026-10-13)"
summary: "One ed25519 key per person per machine, in the keychain; one domain-tagged envelope; trust is a person's own list"
area: decisions
---

# 0042: One ed25519 key per person per machine, in the keychain; one domain-tagged envelope; trust is a person's own list (2026-10-13)

**Status:** proposed. It adds a new API crate (`omuzaki-identity-api`) and an additive change to `omuzaki-library-api`. **Context:** SE-07, LB-07, LB-08, VC-10, VC-11, VC-12, CN-05, EX-05; spec v3 §7, §27, §28, §29, §31, §37; ADR 0009 (canonical form), ADR 0026 (settings); the secret store; the Omuzaki key; watermarking research. Design: `docs/design/identity.md`.

**Amendment (2026-10-15, approved by the Director, #1745):** a tenth domain, `Plugin` (tag `omuzaki-plugin-v1\0`), for a plugin package's publisher signature over its content hash (ADR 0058). Decision 2's "nine closed domains" reads as ten.

## Context

Five features need "who signed this", and so far only the library verifies a signature (`omuzaki-library-api`: ed25519, the tag `omuzaki-library-item-v1\0`, no signing). `Actor::Person` carries a name and no key. Exports are deterministic (io-api D3), and signing must keep them so. Without one shared answer, each feature would invent its own key, message format and notion of trust, and a signature made for one use could be replayed as another.

## Decision

1. **Keys.** Each person has one ed25519 key per machine (per OS account) by default. They may hold several identities (slots), picking one per project; an agency keeps one per client so clients can't link the work. Slots are never linked automatically. The key is generated from `getrandom` and its seed is kept only in the OS keychain, never in a file, setting, log, op record or MCP output. Private keys are never exported. Backup is a second machine's key linked by an `Also` statement. Loss, compromise and planned rotation are handled by signed key statements (`Rotate` signed by both keys, `Revoke`), a team revocation list, or a person marking a key by hand. The Omuzaki release key stays separate.
2. **One envelope.** A signature covers a domain tag (`omuzaki-<use>-v<N>\0`) followed by the 32-byte BLAKE3 hash of the content in canonical form. The canonical form is the library's sorted-key JSON, moved into the shared crate. Checks use ed25519 `verify_strict`. Keys and signatures are lowercase hex. A signature is never inside the hash it signs. There are nine closed domains: `LibraryItem` (byte-compatible with today), `LibraryIndex`, `Export`, `AuditExport` (so an audit export can never pass as a platform export), `Membership`, `Approval`, `ReviewBundle`, `KeyStatement` and `Entitlement`. `Signed<T>` binds the signer's name and key into the hash, and its hashing returns an error rather than panicking. `LibraryItem` keeps its legacy hash, which leaves out the publisher: the key is bound by verification, and the name is display only. A GLB's external buffers and images are part of its content digest.
3. **Trust is a person's list.** The `User` setting `identity.trusted` (`AgentWrite::Never`) holds pinned full keys, each with the person's own label. Keys are trusted on first use only after an explicit confirmation. The verifier reports `Valid {Trusted | Unknown | Revoked}`, `Tampered(Content | Signature)`, `Unsigned` or `Malformed`. Fingerprints (64 bits) are for display and watermarks only and bind nothing.
4. **Signed exports.** A root glTF extension `OMUZAKI_signed_export` (in `extensionsUsed`, never in `extensionsRequired`) holds a signed statement. The statement commits to the file's content digest (canonical JSON chunk plus every other chunk, with the extension absent) and carries the publisher, the licence, `consent_recorded`, the tool string and an opt-in UTC date. No prompts, seeds, paths, private names, capture data or default timestamps go in. Signing private-test material is refused. Formats without an extension point use a detached `.omuzaki-sig.json`.
5. **Agents.** Signing calls `require_person` first, so an agent's request queues as `Need::Person`. `identity.*` and `trust.*` writes are `person_only`.
6. **Shared crates.** `omuzaki-identity-api` (verify only, wasm-clean), `omuzaki-identity-mock` (test seeds), `omuzaki-identity` (keychain signer, settings trust, ops). `omuzaki-library-api` re-exports `KeyId` as `PublisherKey` and delegates to the shared verify.

## Consequences

- The watermark hook is reserved: `KeyId::mark_payload()` (8 bytes), an exporter `mark` option and `io.detect_mark`. The watermarking research picks the technique.
- A team never holds a private key. Team items are signed by members whose keys the roster allows (0043), which settles library.md D2's "per person or team".
- Rotated keys stay valid for what they signed; revoked keys taint everything, since there are no timestamps.
- No new dependency. The `ed25519-dalek` row widens to signing (the `zeroize` feature), and the `getrandom` row to key generation.
- `require_person` before `Signer::sign` is a convention backed by tests; a `PersonPresent` token in `omuzaki-ops-api` is proposed.
- **Rejected:** team keys (no custodian, no per-member revocation); exporting keys as files, even encrypted (they leak, and it would need new crypto); signing raw GLB bytes (the verifier would have to re-serialise byte-exactly); signing the bytes and the claims separately (a claim could be moved onto other bytes); automatic trust on first use; RFC 8785 canonical JSON (the library's form already exists and is in use); a self-registered domain registry (a new signing use should be a reviewed API change).
