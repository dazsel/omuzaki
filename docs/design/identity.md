---
title: "Identity: keys, signatures and trust"
summary: "keys, signatures and trust (ADR 0042): one ed25519 key per person per machine by default, one per project when needed, in the OS keychain, the domain-tagged signature envelope, the trust list, signed exports (`OMUZAKI_signed_export`), key statements, agent rules and the watermark hook."
area: design
---

# Identity: keys, signatures and trust

**Status:** awaiting acceptance. **Feature:** SE-07. **Decision record:** `docs/decisions/0042-identity.md`.
**Spec:** v3 §7 (provenance, licence gate), §27 (EX-01, EX-02, EX-05), §28 (LB-07, LB-08), §29 (VC-10 to VC-12), §31 (SE-07), CN-05, §37 (exports carry no watermark of ours).
**Builds on:** `docs/design/library.md` D2 (publishers, the `omuzaki-library-item-v1` tag), `docs/design/io-api.md` D3 and D5 (pure, deterministic exporters; rights), `docs/design/settings.md` D2 and D4 (`AgentWrite::Never`), `docs/design/ops-api.md` (`require_person`, `person_only`), ADR 0009 rule 3 (canonical form), the secret store.

Five features need an answer to "who signed this": SE-07's signed exports, LB-07 and LB-08 publishing, VC-10's roster, VC-11's approval records and VC-12's identity-provider binding. This page gives them one key per person, one signature envelope, one trust list and one verifier. The types are the contract; their doc comments belong to the implementer.

## 1. What this covers

| Asked for | Where |
|---|---|
| Crates and the contract | D1 |
| Keys: generation, storage, loss, replacement, backup; the Omuzaki key | D2, D5 |
| The envelope: domain tags, message, encoding, where signatures live | D3 |
| Trust: the list, the outcomes, who may change it | D4 |
| Signed exports: digest, extension, what is and isn't embedded, reports, viewer data | D6, D7 |
| Agent rules | D8 |
| The watermark hook | D9 |
| Consumers | D10 |
| Privacy, security, tests, dependencies | D11, D12, D13 |
| Worked example (a GLB, a library item) | §4 |

## 2. The shape of it

```
keychain ──seed──▶ omuzaki-identity: KeychainIdentity ──impl──▶ Signer ◀── ops (after require_person)
                                                                         │ sign(domain, hash)
settings: identity.trusted ──▶ SettingsTrust ──impl──▶ TrustStore         ▼
                                                    │        omuzaki-identity-api: tag ‖ hash ─▶ ed25519
                         verify(domain, hash, key, sig) + trust(key) ─▶ Check { Valid, Tampered, Unsigned, Malformed }
```

The API crate holds the format and the verifier (pure, wasm-clean, no randomness, no storage). The implementation crate holds the only private key, in the keychain. Every consumer hashes its own content canonically and signs the hash under its own domain tag.

## 3. Decisions

### D1. Three crates, and the contract

- **`omuzaki-identity-api`** (API): the types below, `signing_message`, `verify`, `canonical_json`. Depends on `omuzaki-core`, `omuzaki-store-api` (`ContentHash`), serde, serde_json, schemars and `ed25519-dalek` (default features off, verify only). It compiles for `wasm32`, so the web player (EX-05) verifies with the same code.
- **`omuzaki-identity-mock`**: `MemorySigner::from_test_seed(seed: [u8; 32], name)` and `MemoryTrust`. Test seeds exist only here, never in a shipped crate.
- **`omuzaki-identity`** (implementation): `KeychainIdentity` over `omuzaki-secrets-api`, `SettingsTrust` over the settings service, and the `identity.*` and `trust.*` ops.

```rust
pub struct KeyId(pub [u8; 32]);       // the ed25519 public key itself; serde: 64 lowercase hex
impl KeyId { pub fn from_hex(s: &str) -> Result<Self, KeyError>; pub fn to_hex(&self) -> String;
             pub fn fingerprint(&self) -> String;   // first 16 hex of BLAKE3(key): display only (D3)
             pub fn mark_payload(&self) -> [u8; 8]; } // the same 8 bytes, for D9
pub struct Signature(pub [u8; 64]);   // serde: 128 lowercase hex
pub struct Identity { pub name: String, pub key: KeyId }   // a name is never trusted without its key
#[non_exhaustive]
pub enum Domain { LibraryItem, LibraryIndex, Export, AuditExport, Membership, Approval, ReviewBundle, KeyStatement, Entitlement }
impl Domain { pub const ALL: [Domain; 9]; pub const fn tag(self) -> &'static [u8]; }
pub fn signing_message(domain: Domain, hash: &ContentHash) -> Vec<u8>;              // tag ‖ hash (D3)
pub fn verify(domain: Domain, hash: &ContentHash, key: &KeyId, sig: &Signature) -> Result<(), VerifyError>;
pub fn canonical_json(value: &serde_json::Value) -> Vec<u8>;                          // D3
pub struct Signed<T> { pub body: T, pub signer: Identity, pub signature: Signature }  // D3
impl<T: Serialize> Signed<T> { pub fn hash(&self) -> Result<ContentHash, CanonicalError>;   // fails when T has no JSON form (a map with non-string keys)
                                pub fn check(&self, domain: Domain, trust: &dyn TrustStore) -> Check; }   // a hash failure is Malformed
pub fn canonical_hash<T: Serialize>(value: &T) -> Result<ContentHash, CanonicalError>;
pub trait Signer: Send + Sync {       // a service; the private key never leaves it
    fn identity(&self) -> Result<Identity, SignError>;
    fn sign(&self, domain: Domain, hash: &ContentHash) -> Result<Signature, SignError>;
}
pub enum Trust { Trusted { name: String }, Unknown, Revoked }
pub trait TrustStore: Send + Sync { fn trust(&self, key: &KeyId) -> Trust; }
pub enum Check { Valid { identity: Identity, trust: Trust }, Tampered(Mismatch), Unsigned, Malformed(String) }
pub enum Mismatch { Content, Signature }   // the bytes changed / the signature does not hold
pub enum KeyStatement { Rotate { old: KeyId, new: KeyId }, Also { a: KeyId, b: KeyId }, Revoke { key: KeyId, reason: RevokeReason },
                        Bind { issuer: String, subject: String, key: KeyId } }   // Bind: reserved for VC-12
```

`Domain` is a closed enum on purpose, against the usual self-registration rule (§39 rule 2): a new signing use is a security decision that should pass review as an API change, and a closed set lets a test prove every tag is unique and none is a prefix of another. `#[non_exhaustive]` keeps adding one non-breaking.

### D2. Keys: one per person per machine by default, several when needed, made and kept in the OS keychain

- **One ed25519 key per person per machine by default.** "Person" means the OS account: the keychain is per account. A second machine makes its own key, and the two are linked by an `Also` statement (D5). A key never travels between machines, so there is no key file to leak.
- **Several identities on one machine, one picked per project** (from the team design §10). An agency person keeps one key per client, so two clients comparing signed files can't link the work to one key. Each identity has a slug `slot` (`default` unless the person makes more) and its own name and key. The Machine setting `identity.for_project` maps a project to a slot; a project with no entry uses `default`. The `Signer` service an op sees is the one for the current project's slot. Identities are unlinked by design: Omuzaki never makes an `Also` statement between two slots on its own, and the card that would link them says it reveals that they're the same person. The trust list is shared by all slots, because it is never embedded anywhere.
- **Generation:** 32 bytes from `getrandom`, used as the ed25519 seed (`SigningKey::from_bytes`). That needs no `rand_core` feature, and tests inject the seed instead.
- **Storage:** the seed as 64 hex characters in the secret store under label `identity/<slot>/current`, the only place it exists. `SecretValue` zeroes itself on drop. The seed is decoded into a `Zeroizing<[u8; 32]>` and dropped right after each signature, and `SigningKey` zeroes itself (`ed25519-dalek` feature `zeroize`). If the keychain is unavailable (Linux without a Secret Service), the result is `SignError::Unavailable` with the secret store's hint, never a fallback file.
- **Never** in a project file, a setting, a log, an op record, an event or MCP output (CN-05). `Debug` on a signer prints the fingerprint only. No op takes or returns key material.
- **The name** each identity signs as is in the Machine setting `identity.identities` (a list of `{ slot, name }`; Machine because the keys don't roam). The public key is derived from the seed, so it is never stored separately and can't drift from it.
- **The first-party Omuzaki key stays separate.** It is a release key in the maintainers' custody, compiled in as `OMUZAKI_PUBLISHER_KEY` and trusted built in (D4). It is never a person's key, and `identity.*` ops can't reach it.

### D3. The envelope: one message format, a tag per use, the signature outside its hash

- **Message** = the domain's tag bytes, including a terminating NUL, followed by the 32-byte BLAKE3 hash. Tags are `omuzaki-<use>-v<N>\0`, and `LibraryItem` is `omuzaki-library-item-v1\0`, byte for byte what `omuzaki-library-api` signs today (§4 proves it). The NUL means no tag can be read as a longer one. A signature made for one use fails under every other, because the signed bytes differ.
- **Algorithm:** ed25519 (RFC 8032), checked with `verify_strict`. That rejects the malleable signature encodings and weak keys that plain `verify` accepts, so one content has one valid signature per key. ed25519 signatures are deterministic, so signing the same content twice gives the same bytes, and determinism holds (io-api D3).
- **Encoding:** keys are 64 lowercase hex characters and signatures 128, in JSON and TOML alike, as the library already does. No base64: one encoding everywhere, and hex is diff-friendly in git.
- **Canonical form** (ADR 0009 rule 3): JSON with object keys sorted by their UTF-8 bytes, no whitespace, numbers as `serde_json` writes them with `float_roundtrip`. This is exactly `omuzaki-library-api`'s `write_canonical`, which moves into `canonical_json` so there's one copy. It is not RFC 8785 (JCS sorts by UTF-16 code units and formats numbers differently); this form is ours and fixed by test vectors.
- **The signature is never inside the hash it signs.** Everything else is: `Signed<T>::hash()` is BLAKE3 of the canonical `{"body": …, "signer": {"key": …, "name": …}}`, so the signer's name and key are bound too and can't be swapped. Turning a `T` into JSON can fail (a map with non-string keys, a failing `Serialize`), so `hash` returns `Result`: a signer gets the `CanonicalError` before anything is signed, and a verifier gets `Check::Malformed(why)`, never a panic or a hash of something else. Each consumer stores its signature next to the content it signs (D10).
- **One legacy exception: `LibraryItem`.** `ItemManifest::canonical_bytes` leaves out both `signature` and `publisher` (`manifest.rs`), and the tag stays byte-compatible, so that hash is kept as it is. The key is still bound, because the signature only verifies under it. The publisher's **name** is not bound: anyone can relabel a signed item without breaking its signature. That is harmless under D4, because what people see for a key is their own label from the trust list, and an unknown key shows its fingerprint next to the claimed name. A future `omuzaki-library-item-v2` may bind the name; that would be a new tag with v1 still verified, not a change to v1.
- **Types that share a tag can't stand in for each other.** Several body types share one tag (`Statement`, `Invite` and `Acceptance` under `Membership`; `ApprovalStatement` and `ReviewRecord` under `Approval`). Every such body is `deny_unknown_fields` with required fields that differ between types, so one type's canonical JSON never parses as another's. A test that a signature on one type never verifies as another.
- **The fingerprint is for people, not for security.** 16 hex characters are 64 bits: fine for display and as a watermark payload, too short to bind anything. Records, trust entries and pins store the full `KeyId`. library.md D5's record pin stores the fingerprint today, and §7 files the fix.

### D4. Trust: a person's own list of pinned keys

- **"Trusted" means "on this person's list".** Validity is maths; trust is a person's choice. The list is the `User` setting `identity.trusted`: a list of `{ key, name, state: trusted | revoked }`, with `AgentWrite::Never`, `InProfile::Excluded` and `Apply::Live`. The `name` is the person's own label for the key, not the signer's claim. The Omuzaki key is trusted built in, as "Omuzaki", and can't be edited.
- **The verifier reports five outcomes**, which every surface shows the same way:

| `Check` | Shown as |
|---|---|
| `Valid { trust: Trusted { name } }` | "Signed by *name*" (and "signs as *claimed*" when the two names differ) |
| `Valid { trust: Unknown }` | "Signed by an unknown key that calls itself *claimed* (*fingerprint*)", with **Trust this key…** |
| `Valid { trust: Revoked }` | "Signed by a revoked key": every signature by that key, since there are no timestamps to tell old from new |
| `Tampered(_)` | "Changed after it was signed", and the claimed name is not shown as fact |
| `Unsigned` | "Not signed"; a VRM still shows its own meta (D7) |

  `Malformed(why)` is a broken signature block, shown with its reason.
- **Trust on first use only with a person's explicit confirmation.** Seeing a valid signature never adds a key. **Trust this key…** shows the full fingerprint and asks for a name. A trusted key's valid `Rotate` statement (D5) offers its successor under the same name, with one confirmation.
- **Only a person changes the list:** `trust.*` writes are `person_only`, never queued (D8). A team's revocation list (0043's seam) can mark keys `Revoked` for its members, never `Trusted`.

### D5. Rotation, loss, revocation and backup: signed key statements

Key statements are `Signed<KeyStatement>` under `Domain::KeyStatement`. A statement that names two keys (`Rotate`, `Also`) is valid only when both keys sign the same body: two `Signed` values with one body. That proves both keys belong to the person making the claim.

- **Planned replacement** (`identity.rotate`): make the new seed under `identity/<slot>/next`, sign `Rotate { old, new }` with both keys, save the statement, move the seed to `identity/<slot>/current`, then delete `identity/<slot>/next`. Rotation is per slot. If the process crashes part-way, `open` finds `identity/<slot>/next` and finishes the swap when the statement was saved, or discards the new seed when it wasn't. A rotated key is retired, not revoked: what it signed stays valid.
- **Compromise** (`identity.revoke`): the key signs `Revoke { key: self, reason: Compromised }`, and a fresh key is made. Everyone who imports the statement sees that key as `Revoked`.
- **Loss** (a dead disk, a wiped account): a lost key can't sign its own revocation. Instead, a key linked by `Also` signs `Revoke` for it; or the team's revocation list does (0043); or each person who trusted the key marks it revoked by hand.
- **Backup is a second key, not a copy.** Private keys are never exported, in any form (no file, no QR, no passphrase blob). A person with two machines links their keys with `Also`, and either key can revoke the other. With one machine, losing it means a new key and re-establishing trust, which the UI says when the key is created. Why: an exported key is a file, and files leak; the `Also` route needs no new crypto (a passphrase-encrypted export would need a KDF and an AEAD). An encrypted export is an open question.
- **Statements travel as files** (`*.omuzaki-key.json`, imported with `trust.import`, person only) and inside team rosters (0043). They are public data, kept in the local data dir under `identity/statements/`.

### D6. Signed exports (SE-07)

**What is signed: a statement, which commits to the file.** The `Export` hash is the canonical hash of the statement below. Its `content` field is the digest of the file with the signature extension absent. So the licence claims and the bytes are covered by one signature, and editing either one breaks it.

**The content digest of a GLB** (`glb`, `vrm1`, `vrm0`; a VRM is a GLB). It is BLAKE3 over: `omuzaki-glb-content-v1\0`; the canonical JSON chunk's length (u64 LE) and bytes; then for each later chunk, its type (u32 LE), its length (u64 LE) and its data, padding included. The JSON is parsed first. The parse refuses duplicate keys (two readers could otherwise see two documents), removes `extensions.OMUZAKI_signed_export`, drops its name from `extensionsUsed`, and drops either container once it is empty. Hashing the canonical JSON rather than its raw bytes means the verifier never needs to re-serialise byte-exactly, and whitespace and padding, which carry no meaning, can't break a signature. Every value and every BIN byte is covered, and a VRM's own `meta` along with them.
- **External resources, GLB and `.gltf` alike.** A valid GLB may still point at external files (any `buffers[i]` or `images[i]` with a non-`data:` URI; the GLB's own `BIN` buffer has none). After the chunks, each one is added in index order, buffers first: `BUFR` or `IMAG`, the index (u32 LE), the length (u64 LE) and the bytes, read through the host's `Resolver`. Replacing an external texture therefore gives `Tampered(Content)`. A resource that can't be resolved is `Malformed("missing <uri>")`, never skipped. A `.gltf` is the same with its JSON in place of the JSON chunk and no chunks. Omuzaki's own exporters write self-contained GLBs, so their digests have no external parts (the §4 bean has none).
- **A format with no extension point** (later exporters such as PNGTuber or engine packages) gets a detached `<file>.omuzaki-sig.json`. It holds the same object, with `content` = BLAKE3 of `omuzaki-file-content-v1\0` followed by the raw bytes.

**Where the signature lives:** the root `extensions.OMUZAKI_signed_export`, which glTF and both VRM versions carry. It is listed in `extensionsUsed` and never in `extensionsRequired`, so every loader can still open the file. That's the same rule VRM's own root extensions follow.

```json
"OMUZAKI_signed_export": {
  "statement": { "v": 1, "content": "<64 hex>", "publisher": { "name": "…", "key": "<64 hex>" },
                 "licence": { "spdx": "CC0-1.0", "commercial": "ok", "attribution": "…", "terms_url": "…" },
                 "consent_recorded": false, "tool": "omuzaki 0.1.0", "date": "2026-10-13" },
  "signature": "<128 hex>"
}
```

- **Embedded:** the publisher's name and key; the licence from the scene's provenance (SPDX id, commercial `ok | no | unknown`, plus the attribution and terms URL when set), the same record EX-01 writes to VRM meta; `consent_recorded`, a boolean only, with no consenting person's name, scope or document hash; and `tool`, the host's `generator` string (io-api D3). `date` is a UTC calendar day with no time and no zone, written only when the person turns on `identity.sign_with_date` (default off), so by default signing the same scene twice gives identical bytes. Optional fields are left out, never `null`, and unknown fields make the statement `Malformed`. `v` above 1 is `Malformed("signed by a newer Omuzaki")`.
- **Never embedded:** prompts, seeds, models or tools from provenance `Source::Generated`; local paths and file names; private-test names; camera, tracking or capture data; machine or OS-account names; timestamps by default. The export privacy guard holds this rule as a test.
- **Refused:** signing a scene with any `private_test` provenance (`sign.private_test`). Signing proves authorship, and private material is never shared (§40). The licence gate is not required: a private signed export is allowed, and the statement records `commercial: unknown` truthfully.
- **Determinism:** `io.export {sign: false}` leaves every byte as it is today. With `sign: true`, omuzaki-gltf's own writer gives `strip(signed) == unsigned` byte for byte, which a test checks. The verifier doesn't rely on that.

### D7. Reports: `io.verify`, `io.inspect`, `io.import` and the "who made this" data

- **`io.verify {path | asset}`** (query, `read`; a path goes through `fs_access` as `io.inspect` does). It returns `Verification { check: Check, statement: Option<ExportStatement>, vrm: Option<VrmSummary>, findings }`. `vrm` is the file's own meta (authors, licence URL, commercial usage), reported for unsigned files too. This struct is the provenance viewer's whole data: the viewer renders it and never reads the file itself.
- **`io.inspect`** embeds the same block.
- **`io.import`** reports it as one finding: `sign.valid` (info), `sign.unknown_key` (info), `sign.revoked` (warning), `sign.tampered` (warning) or `sign.malformed` (warning), and nothing for unsigned files, which are the norm. Import never derives provenance from a signature: the io-api D5 rules stand, because a signature proves who signed, not that the claim is true. The person sees both side by side. The signature extension is not kept in the imported scene, because an edited scene can't still carry it.

### D8. Agent rules

- **Signing needs a person.** Every op that signs calls `ctx.require_person("sign as <name> (<fingerprint>)")` before `Signer::sign`. An agent may request a signed export: it queues as `Need::Person`, and the card shows the name, fingerprint and licence to be embedded. Nothing is signed until a person approves.
- **`identity.*` and `trust.*` writes are `person_only`**, never queued: they change who this app is and whom it believes. That includes `identity.use {slot}`, which picks a project's identity, because an agent switching it could link two clients' work. `identity.describe` and `trust.list` are open to agents and return only public data.
- The `require_person` call is a convention today: nothing stops an op from calling `Signer::sign` without it. Each signing op has an agent-denied test, and §7 proposes a `PersonPresent` token minted only by `require_person`. That is a hub-crate change (`omuzaki-ops-api`), so it needs a maintainer decision.

### D9. The watermark hook (technique)

The design reserves only the interface. The payload is `KeyId::mark_payload()`: the 8 bytes behind the fingerprint, 64 bits, a common size for such marks. An exporter option `mark: bool` (default off) applies the mark after the platform fit (`fit_to`, ADR 0047) and before `run_export`, so fitting never resamples a marked texture, and so before the content digest: a signature covers the marked file. `io.detect_mark {path | asset}` returns `Option<[u8; 8]>`, and the result is matched against the trust list's fingerprints. A mark is the creator's own and opt-in, never Omuzaki branding (§37). It may survive a re-export that strips the signature, which is why both exist.

### D10. Consumers: each states its domain and what it signs

| Domain | Tag | Hash of | Signature stored | Owner |
|---|---|---|---|---|
| `LibraryItem` | `omuzaki-library-item-v1` | the manifest's canonical form, legacy: publisher and signature both excluded (D3) | the manifest's `signature` field | library.md D2; LB-07, LB-08 publish |
| `LibraryIndex` | `omuzaki-library-index-v1` | a shared source's index snapshot, and the `Checkpoint` that binds the membership log's head | the index file | 0057 |
| `Export` | `omuzaki-export-v1` | the export statement (D6) | `OMUZAKI_signed_export` or the `.omuzaki-sig.json` file | this design |
| `AuditExport` | `omuzaki-audit-export-v1` | an audit-log export; its own tag, so it can never be replayed as a platform `Export` | the audit export file | 0043 |
| `Membership` | `omuzaki-membership-v1` | 0043's `Signed<Statement>`, `Signed<Invite>` and `Signed<Acceptance>`, one per log entry (nothing signs a roster as a whole) | the membership log | 0043 |
| `Approval` | `omuzaki-approval-v1` | 0043's `Signed<ApprovalStatement>` (VC-11) and 0057's `Signed<ReviewRecord>` (item reviews) | the approval log; the review records | 0043, 0057 |
| `ReviewBundle` | `omuzaki-review-bundle-v1` | a review bundle's manifest | the bundle | review links |
| `KeyStatement` | `omuzaki-key-statement-v1` | `Signed<KeyStatement>` | statement files, rosters | this design (D5) |
| `Entitlement` | `omuzaki-entitlement-v1` | a market-signed document: a receipt, a head, a revocation or takedown page (each a distinct `deny_unknown_fields` body with a required `doc` field; ADR 0063, `docs/design/marketplace-protocol.md`) | the document | 0057, 0063, LB-08, a hosted key |

Every tag above ends in `\0`. The other consumers:

- **LB-07 and LB-08:** publishing signs the item with the publisher's own key. A team has no private key of its own, because a shared secret has no single custodian and can't be revoked per person. A team item is signed by a member whose key the roster allows to publish for the team. This settles library.md D2's "per person or team" in favour of per person.
- **VC-12:** `Bind { issuer, subject, key }`, signed by the person's key and countersigned by the hosted identity service's key, which the app pins the way it pins the Omuzaki key. Whether to run that service is undecided, so this is reserved and not built.
- **EX-05:** the web player links `omuzaki-identity-api` and runs `verify`.

### D11. Privacy and security

- **No network and no telemetry** in any identity crate (`lint-deps` holds it), and no timestamps unless opted into (D6).
- **Constant time:** the private-key operations are ed25519-dalek's, which are constant-time. Our code compares only public values (digests, keys, signatures), so it needs no constant-time compare. Any comparison of secret bytes added later must use `subtle` (already in the tree).
- **Zeroize** as in D2. Errors (`SignError`, `KeyError`) never contain key bytes, which a test checks the way the keychain identity's does.
- **Hostile input:** the verifier refuses duplicate JSON keys, a GLB header whose length differs from the file's, chunks past the end and trailing bytes (`Malformed`). It checks declared sizes before allocating (io-api D3 limits). A 25 MB GLB verifies in one BLAKE3 pass.

### D12. Tests

- **Deterministic keys:** `MemorySigner::from_test_seed`, mock crate only. The §4 numbers are golden vectors: the bean's content digest, statement hash and signature, and the library item's hash and signature.
- **Envelope:** the RFC 8032 §7.1 vectors; every tag is unique and none is a prefix of another; for all 81 domain pairs, a signature made under one domain verifies only under that same domain; proptest flips one bit of the hash, signature or key; the library tag is byte-compatible (`tests/compat.rs`); a relabelled library publisher still verifies and shows the trust list's label (D3); a `Signed<T>` whose `T` fails to serialise gives `Err` from `hash` and `Malformed` from `check`.
- **The tamper matrix** (§4): one flipped byte in the JSON chunk and one in the BIN chunk; the extension stripped; another file's extension or signature pasted in; the key swapped; the wrong domain; the licence claim edited; a non-hex signature; duplicate keys; a bad header; a GLB whose external texture is replaced (`Tampered(Content)`) or missing (`Malformed`). Each has its expected `Check`.
- **The privacy guard, the agent-denied tests (D8) and no network:** the identity crates have no network dependency, which `lint-deps` checks.

### D13. Dependencies and licences

No new crate. `ed25519-dalek` 2 (BSD-3-Clause) is already in the register. Its row widens: it now backs `omuzaki-identity-api` (verify) and `omuzaki-identity` (signing, with the `zeroize` feature on; `zeroize` and `subtle` are already listed in the same row). `getrandom` (MIT OR Apache-2.0) is in the register for MCP tokens, and its row adds key generation. `blake3` comes in through `omuzaki-store-api`. The rows change in the tasks that add the uses.

## 4. Worked example (computed 2026-10-13 by a throwaway program run against this design; not committed)

**The key:** test seed `07` × 32. Public key `ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c`, fingerprint `0871f3aabc26e458`.

**The character:** a synthetic "bean", an octahedron with 6 vertices and 8 triangles, one node `synthetic_bean`, one material, a 120-byte BIN chunk. It was made by the script; nothing comes from a real character. Unsigned GLB: 800 bytes. Signed: 1356 bytes. Signing twice gave identical bytes.

- content digest: `f695c20da17fdb192cb86090b1fca6381b2259c2269cb33978d8c5fefc6d945e` (the same value from the unsigned and the signed file)
- canonical statement: `{"consent_recorded":false,"content":"f695c2…945e","licence":{"attribution":"Test Publisher","commercial":"ok","spdx":"CC0-1.0"},"publisher":{"key":"ea4a6c…d22c","name":"Test Publisher"},"tool":"omuzaki 0.1.0","v":1}`
- statement hash: `da23f3b471a937f615853039b655a47b276919ffa4a074a9715136d74a4567ed`
- signature (`Export`): `3a5cdd78818e30be4246a9abae0f52c4d3b1cedc1f2a8eea53943bed49ccfe75cdf19916d8be767e8013a9e6db25410be98ee88d2623b4fd078ca2b05262c504`

| Case | `Check` |
|---|---|
| as signed, key on the list / not on it | `Valid { Trusted }` / `Valid { Unknown }` |
| one flipped byte in the JSON chunk (`synthetic_bean` → `rynthetic_bean`) | `Tampered(Content)` |
| one flipped byte in the BIN chunk (first vertex, y) | `Tampered(Content)` |
| extension stripped (the bytes equal the unsigned original) | `Unsigned` |
| whole extension pasted from another signed bean (other colour) | `Tampered(Content)` |
| only that file's signature pasted in | `Tampered(Signature)` |
| wrong key: `publisher.key` swapped for seed `08`'s | `Tampered(Signature)` |
| wrong domain: signed with the `LibraryItem` tag | `Tampered(Signature)` |
| licence claim edited (`commercial` → `no`) | `Tampered(Signature)` |
| signature not hex | `Malformed` |

**A library item under the new envelope.** `TWINTAILS` (the library-api fixture) as `mira/hair/twintails`, publisher `Mira`, the same key, signed through today's `ItemManifest::signing_message`. Manifest hash `03f35c02b32bc1503ca0b5cfb3ea1df2a4096617f365d26963c3ebcbff2fea90`; signature `fa5c5b9da09ec697a26066a1902d90d6f5d85a0bfe0dd96fb1d608d3b3e1f9db9b3b27e01733f18e14328c3c03ac411633572bc365949763b2761e70fd1f750c`. `verify_with` returns `Verified { name: "Mira", fingerprint: "0871f3aabc26e458" }`. `tag(LibraryItem) ‖ hash` equals the library's message byte for byte and verifies; the same signature under the `Export` tag fails.

## 5. Where the sources disagree, and what this picks

- **library.md D2, "generated locally per person or team":** this design picks per person (D10), because a team key has no custodian and can't be revoked per member. A one-line note is added there.
- **library.md D5 pins the fingerprint, while D3 here says fingerprints bind nothing.** This design picks the full key. The fix is a library task (§7), since D5 is outside this fence.
- **The first sketch signs rotations under `Domain::Membership`.** This design picks `KeyStatement`: rotation is personal, and Membership belongs to a team's log (0043).
- **The first sketch takes `&mut dyn RngCore`.** This design picks a seed from `getrandom` plus a test seed, so no `rand_core` feature is needed.
- **The first sketch's `SignedExport` has no `content` field, so it signs the bytes and the claims separately.** This design picks one statement covering both, so a claim can't be swapped onto other bytes.

## 6. Crates and contract changes

- **New:** `omuzaki-identity-api`, `omuzaki-identity-mock`, `omuzaki-identity`. Their `crates/README.md` rows land with them.
- **`omuzaki-library-api` (additive):** `pub use omuzaki_identity_api::KeyId as PublisherKey`. `signing_message` and the signature check delegate to the shared functions, and `write_canonical` gives way to `canonical_json`. Public API and bytes unchanged.
- **`omuzaki-gltf`:** `OMUZAKI_signed_export` read, write and strip. **`omuzaki-io-ops`:** `io.export {sign}`, `io.verify`, `io.inspect`'s block, the import finding.
- **Settings keys** (all `AgentWrite::Never`, `InProfile::Excluded`, `Apply::Live`): `identity.trusted` and `identity.sign_with_date` (`User`); `identity.identities` and `identity.for_project` (`Machine`).
- **No change to `omuzaki-ops-api` or `Actor::Person`:** signed records carry their `Identity`. The `PersonPresent` token is a proposal for the maintainers (D8).

## 7. The task cut

Each task is filed with its fence, contract, acceptance and `Blocked by` when this design is accepted.

| Task | Status | Blocked by |
|---|---|---|
| identity API and mock | re-cut: 9 domains, `Signed<T>`, `canonical_json`, `KeyStatement`, `Mismatch`, `mark_payload`, the §4 golden vectors | this design |
| keychain identity | re-cut: slots, with `identity/<slot>/current` and `identity/<slot>/next`, `getrandom` seed, two-key `Rotate` under `KeyStatement` | identity API and mock, the keychain secret store |
| new: trust and identity settings keys plus `SettingsTrust` | new | identity API and mock |
| `identity.*` and `trust.*` ops | re-cut: adds `trust.revoke`; the settings keys moved out | keychain identity, the settings-keys task |
| new: key statements: `trust.import`, `identity.revoke`, `Also` | new | `identity.*` and `trust.*` ops |
| signed GLB export | re-cut: the statement shape, the digest of D6, duplicate keys, `private_test` refusal, §4 vectors | identity API and mock |
| `io.verify` and the import finding | re-cut: `Check`, the §4 tamper matrix, the `sign.*` findings | signed GLB export |
| export privacy guard | confirmed; adds camera data and machine names | signed GLB export |
| new: library re-exports the shared verify | new | identity API and mock |
| new: library record pins the full key | new | identity API and mock |
| new: `.gltf` sidecars and detached signatures | new | signed GLB export |
| new: provenance viewer panel | new (visual) | `io.verify` and the import finding |
| new: watermark embed and `io.detect_mark` | new | signed GLB export |
| new: web player verify | new | identity API and mock, EX-05's player |
| new: signed review bundles | new | the review-links design, identity API and mock |
| new: `PersonPresent` token for `Signer::sign` | new (hub crate; needs a maintainer decision) | identity API and mock |

## 8. Seams with the sibling designs

- **0043:** asked for several identities per machine (D2) and the `AuditExport` tag; owns the `Statement`, `Invite`, `Acceptance` and `ApprovalStatement` bodies, the roster rules, who signs a team revocation list, and which keys may publish for a team. This design gives it `Signed<T>`, the tags and the trust outcomes.
- **0044:** `Author::Person.key: Option<KeyId>` holds the full key.
- **0045:** no git commit signing in v1. Signatures sit in fields of the files they sign, so a merge never rewrites one.
- **0057:** the tags `LibraryIndex` and `Entitlement`, and `Approval` for item reviews.

## 9. Sources

Read on 2026-10-13: spec v3 §7, §27, §28, §29, §31, §37 and CN-05; `docs/design/library.md` (D2, D5, §8), `docs/design/io-api.md` (D3, D5) and `docs/design/settings.md` (D2, D4); ADR 0009 and ADR 0026; `crates/omuzaki-library-api/src/{publisher,manifest}.rs`; `crates/omuzaki-ops-api/src/{ctx,approval,actor}.rs`.

Primary sources:
- glTF 2.0 Specification, "Specifying Extensions" (`extensionsUsed`, `extensionsRequired`) and "GLB File Format" (chunk layout, padding, "ignore chunks with unknown types"): https://github.com/KhronosGroup/glTF/blob/main/specification/2.0/Specification.adoc, retrieved 2026-10-13.
- Khronos extension prefixes: https://github.com/KhronosGroup/glTF/blob/main/extensions/Prefixes.md, retrieved 2026-10-13. `VRMC` is registered and `OMUZAKI` is not (an open question).
- ed25519-dalek `verify_strict` docs and features (`default = ["fast", "zeroize"]`): https://github.com/dalek-cryptography/curve25519-dalek/tree/main/ed25519-dalek, retrieved 2026-10-13.
- RFC 8032 (EdDSA), §5.1 and the §7.1 vectors: unverified (unreachable from the build box).
- keyring-rs licence (MIT OR Apache-2.0): https://github.com/open-source-cooperative/keyring-rs, retrieved 2026-10-13. It bears on the keychain identity, not on this design.
