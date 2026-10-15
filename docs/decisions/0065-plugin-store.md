---
title: "0065: The plugin store is signed files: publishers sign packages, a store signs a listing and a revocation list, and the client verifies all of it (2026-10-15)"
summary: "The plugin store is signed files: publishers sign packages, a store signs a listing and a revocation list, and the client verifies all of it"
area: decisions
---

# 0065: The plugin store is signed files: publishers sign packages, a store signs a listing and a revocation list, and the client verifies all of it (2026-10-15)

**Status:** proposed. It adds two `omuzaki-identity-api` variants (`Domain::PluginIndex`, `Domain::PluginRevocation`), an
ADR 0042 amendment that needs the Director's approval, and amends two points of ADR 0058 (below). **Context:** PL-04
(#1084); spec v3 §30, §28, §31 (SE-04, SE-07), §6; ADR 0042, 0057, 0058. Design: `docs/design/plugin-store.md`.

## Context

ADR 0058 settled the host, the package container, its hash and its signing domain, and said the store (PL-04) refuses
non-commercial licences. Open: what a store is, who signs what, how a listing stays fresh, how a bad plugin is pulled
from machines that already have it, and what install and update show a person. The hosting and any review service are
the owner's decisions; the mechanism must work without them.

## Prior art

Open VSX (EPL-2.0): verified namespaces and a registry signature, but one signature, no revocation of installed
extensions and no permission model. TUF (python-tuf is MIT or Apache-2.0): the right threats (rollback, freeze,
compromised online keys), too many roles for a small store. Sigstore (Apache-2.0): signatures that outlive keys through
a transparency log, at the cost of accounts and online services. crates.io's sparse index (Cargo is MIT or Apache-2.0):
immutable versions, `yanked`, conditional GET, but unsigned and yank only advises. Flathub: permissions shown before
install, a signed repository, domain-verified publishers. The table and what we do better are in design §2.

## Decision

1. **Two signers, one scheme.** The publisher signs `PackageStatement { id, version, hash }` (`Domain::Plugin`, in
   `plugin.sig`, outside the hash); the store signs `IndexBody` and `RevocationBody` (`Domain::PluginIndex`,
   `Domain::PluginRevocation`). All are `Signed<T>` of `omuzaki-identity-api`; the verifier, `Signer` and `TrustStore`
   are identity's. A store compromise can withhold or replay, not ship code.
2. **The package is ADR 0058's:** a zip, BLAKE3 over staged files, 16 MiB compressed in a store. The signed body
   adds id and version so a signature cannot be relabelled; a store package carries a `LICENSE`.
3. **The index is one signed JSON file** with a pinned `(epoch, sequence)`, a 14-day expiry, immutable versions and
   `yanked`, under the updater's replay rules. Its name, summary, licence and permissions are display data that
   install recomputes; a mismatch refuses.
4. **Revocation is a separate signed list** of package hashes and publisher keys, with classes. It disables an
   installed plugin through ADR 0058's revocation path, at once and at every load. `Malware` and `Compromised`
   cannot be overridden; other classes can, per hash, by the person. A stale list shows a badge, never disables.
5. **Updates must keep the publisher key,** show only the permission difference, keep the old version running until
   accepted, never lower the version and are never automatic. One previous version is kept for rollback; a pin
   refuses updates.
6. **Nothing is fetched until a person adds a store;** then only on refresh or an opt-in daily schedule; the client
   diffs the whole index locally and sends no identifier.
7. **The store and the client both refuse non-commercial and unknown licences;** a local install is unaffected.
8. **Without a review service, listings say "Not reviewed."** A service later adds `Signed<ReviewStatement>` records
   (`Domain::Approval`) by keys the index names, verified by the client.
9. **Crates:** `omuzaki-plugin-store-api`, `-mock` and `omuzaki-plugin-store`; `omuzaki-plugin-api` is unchanged.

## Consequences

- **Cross-area, for the Director:** the two new `Domain` variants (an ADR 0042 amendment; the tag-prefix tests cover
  them); three new crates and a workspace line (hub file).
- **Amends ADR 0058,** for the plugin lead: `plugin.install` takes a `source` (a path or a store entry) and the
  signed body is `PackageStatement`, not the bare hash; an unsigned local package installs only with
  `plugin.developer_mode` on (0058 allowed it always). New ops: `plugin.update`, `plugin.pin`, `plugin.rollback`,
  `plugin.keep` and `plugin.store.*`, all person-only except `list` and `describe`.
- **Open, with a recommendation (design §14.6):** the immutability record is capped at 50,000 entries and refuses past
  it; a signed per-id commitment under a new `format` would let it compact. Recommended later, not in v1.
- **Owner's, not decided here:** hosting and key custody, the default revocation cadence (`manual` against `daily`), a
  review service, a transparency log, and whether to keep the developer-mode tightening. None blocks a folder or static
  store.
- A machine that never connects learns of a revocation late; the badge and the opt-in schedule are the mitigation.
- Nine tasks follow (design §15); the lead files them after review.
- Rejected:
  - a TUF role set, for the weight, and Sigstore keyless signing, for the account and online services it needs;
  - signing only the hash (relabelling);
  - an unsigned index (the crates.io shape), and one that identifies the person to fetch it;
  - disabling plugins when the list expires (a live show would fail because a server is down);
  - automatic updates of third-party code that acts unattended;
  - one file per plugin in v1 (sharding is an additive `format`).
