# Can loro carry the project document and our ops? (2026-10-13)

> Research pass (VC-06), under spec v3 §7 and §12. Everything in section 3 was **run** on the cloud box with the spike in `spikes/crdt-doc/` (loro 1.16.2, Rust 1.99.0). The alternatives (section 5) are from reading crate source only; nothing was run for them. The cloud box reaches github.com and the package registries only, so product websites and papers were not read; claims that rest on them are marked `unverified (unreachable from the build box)` or are left out.

Source marks: **verified** = I read the primary source or ran it; **secondary** = someone else's summary; **unverified** = no source reached. Retrieval date for every crates.io source: 2026-10-13.

## 1. Answer

**Recommendation: do not make the CRDT the source of truth. Keep ops and patches as the unit of change, order them with a sequencer for live sessions, and treat offline peer-to-peer editing as an open design question: loro could carry it, but no protocol that keeps §12 there has been demonstrated (section 6).** Reasons, each measured below:

1. loro carries our `Patch` and converges (1,000 random three-peer runs pass), but it resolves two edits of one field by **last writer wins** and keeps no record of the loser. §12 ("no silent overwrites") fails unless we build the detection ourselves, and what we need for that (who had seen what) is *our* log, not loro's.
2. A CRDT has no abort and no `before` check, so the exact-inverse undo and the stale-write guard stay ours. loro adds nothing to either.
3. It does fit the data shape (objects as maps merge by key), the costs are small (section 3), and the licence closure passes `cargo deny` with the repo's own `deny.toml`.

The honest summary: loro converges and is cheap, but it is a poor *arbiter*, and whether it is a usable *carrier* of our patches is not shown. Section 6 lists what must be built around it, and section 7 the questions this note could not close.

## 2. Fit: how our document maps onto loro

Our document (`omuzaki-doc-api`): a `serde_json::Value` tree changed only by a `Patch` (a list of `Change { path, before, after }`), with `Patch::inverse` as exact undo and a `before` check against the current value on apply (verified: `crates/omuzaki-doc-api/src/lib.rs`).

| Our concept | In the spike | Result |
|---|---|---|
| Object | `LoroMap`, one per object, children inserted as containers | Two peers editing different keys of one object merge with no conflict. |
| Scalar | leaf `LoroValue` in the parent map | Last writer wins per key. |
| Array | leaf `LoroValue::List` | Treated as a **leaf**, as the issue asks. Editing element 1 means rewriting the whole array, so concurrent edits to elements 0 and 1 of one array collide (LWW on the whole array) and one is lost. The detector (section 4) counts the whole array as one merge unit. |
| `Change` | `put` / `delete` on the parent map; objects become nested maps | One `Patch` is one loro commit; `export(updates since the version before the patch)` gives that patch's bytes. |
| `before` check | read the current value from loro, compare, refuse | Works, and must run **before** writing, because loro has no abort once an op is recorded. |
| `Patch::inverse` | apply the swapped patch | Exact when no one else touched the paths in between (test `inverse_restores_exactly`: 30 random patches undone in reverse give the starting value). With a concurrent edit the inverse's `before` no longer matches and is **refused**, same as today (section 4). |
| Movable tree | not exercised | loro has `LoroTree` (verified: `loro-1.16.2/src/lib.rs`). `docs/design/scenes.md` D10 says scene nodes are flat and id-keyed with anchors as parent links, so a tree would map. The spike did not try it; what happens to a `Patch` that reparents a node is **not measured**. |

**Determinism (ADR 0009).** Same patches in the same order on a peer with the same fixed peer id give byte-identical full snapshots (test `same_ops_same_bytes`, verified). Peer ids are part of op ids, so the peer id must be derived from something stable (an installation or session id), never random per process. Not measured: whether peers that received the same ops in different orders export identical bytes. State equality (the value) is what converges; byte equality across peers is not claimed. loro's API has `set_record_timestamp(true)` to record commit timestamps (verified: `loro-1.16.2/src/lib.rs`); the spike never calls it, so no wall-clock value enters the bytes. Keep it off.

**Mismatches found (the list the issue asks for):**

1. **No stale-write guard.** Our model rejects an edit whose `before` is stale. A CRDT merges both edits.
2. **LWW on every leaf**, and on object *creation*: two peers creating the same key with different sub-objects keep one whole subtree and drop the other, they do not merge (test `concurrent_object_creation_keeps_one_subtree`: both created `extra0`, `{"a":1}` and `{"b":2}`; both ended with `{"b":2}`).
3. **No atomic patch.** A `Patch` is "all or none" today; loro commits what was written, so validation must run first, change by change against the state the earlier changes left (not against the initial state).
4. **Array elements are not merge units** in this mapping (leaf arrays).
5. **Undo across peers.** `inverse` expects the document to be exactly as the patch left it. After a merge it often is not.
6. **Integers beyond i64** and non-finite floats do not round-trip through `LoroValue` (I64/Double only; `u64` above `i64::MAX` is not representable in the spike's mapping). Our TOML files can hold only what TOML holds, so this is likely harmless, but it is a check the adapter needs.
7. **History grows** (section 3): a full snapshot is about 2x the state.

## 3. Cost on the cloud box (reported, never gating; spec §10)

Cloud box: 4 vCPU Linux container, release build, `cargo run --release --bin bench` in `spikes/crdt-doc`, three runs. Synthetic project: 500 "files" of about 20 fields each (scalars, a nested transform and material object, a 3-element array), 5,000 seeded random patches of 1 to 2 changes. No private material. **reported** means measured once on this box, not a budget result.

| Metric | Value | Status |
|---|---|---|
| Update bytes per patch (loro `Updates` export of one commit) | p50 **93 B**, p95 **121 B** (5,000 patches; identical in all three runs) | reported |
| Genesis update (create all 500 files in one patch) | 63 KB | reported |
| Full snapshot after 5,000 edits (with history) | **191 KB** | reported |
| State-only snapshot (no history) | 113 KB | reported |
| Plain JSON of the same state | 98 KB | reported |
| Merge, two peers diverged by 300 patches each, as one batch | 10.7 to 15.0 ms | reported |
| Resident memory for the document | +7 to 10 MB after building the 500-file doc, +15 to 17 MB after 5,000 edits (RSS delta in one process, includes the harness) | reported |
| wasm32 compile | `cargo check --release --lib --target wasm32-unknown-unknown` passes (check only; no wasm linking or run) | verified |
| Licence closure | `cargo deny --config ../../deny.toml check licenses bans sources`: `bans ok, licenses ok, sources ok`. `cargo tree` lists 143 unique normal-dependency lines. `cargo about generate --config ../../about.toml ../../about.hbs` (the repo's notices config) exits 0 with 132 crate entries, so every licence is on the accepted list. The closure is attached as `spikes/crdt-doc/licence-closure.txt` (`cargo deny list`). The three MPL-2.0 crates are `im` 15.1.0, `bitmaps` 2.1.0, `sized-chunks` 0.6.5, the same trio the register already lists. The rest is MIT, Apache-2.0, BSD, BSL-1.0 (`xxhash-rust`), Unlicense. | verified |

How to read the table: a single edit costs about 100 bytes on the wire, which is a fine size for live sync; merging hundreds of edits takes tens of milliseconds; the whole 500-file project is small in memory. Nothing here threatens a §10 budget, and none of it was measured on the reference machines. The `5,000 patches` timing is not reported: the harness re-reads the whole document per patch, so it measures the harness.

## 4. Conflicts: what two peers editing one field get

**Demonstration** (test `two_peers_set_one_field`, run, `cargo test --release -- --nocapture`):

- Three peers share a 1-file project. Field `files/f000/transform/x` is `0` everywhere.
- Peer 2 sets it to `11`; peer 3 sets it to `22`, concurrently, each with a correct `before`.
- After exchanging updates both end with `Some(22)` and `Some(22)`. Peer 2's `11` is gone. **No error, no notice, no trace in the document.** loro's map is documented as "LWW(Last-Write-Win) Map. It can support Multi-Value Map in the future" (verified: `loro-1.16.2/src/lib.rs`, doc comment on `LoroMap`). Which value wins in a tie is loro's rule, not ours; in this run the higher peer id won, and I did not check the rule against loro's source or docs, so do not rely on it.
- The loser's own undo (`before` = the value it wrote, `after: 0`) is then **refused** with a conflict, because the merged document holds the winner's value, not the one the patch left. That is today's `PatchError::Conflict` behaviour on the `before` check, and it is the only trace of the lost write inside the document model (asserted in the same test).

**What a conflict item needs.** Our log (not loro's) already has it: each logged patch carries the author, the author's version vector when it applied the patch (`saw`), and its changes. `conflicts()` in the spike takes the log and reports every pair of patches that are **concurrent** (neither had seen the other's first op) and write to one merge unit with different results. Concurrent means `saw[a.peer] <= b.start` both ways. Result on the demo: exactly one conflict. Across 50 seeded runs of 40 patches the generator produced more than zero (test `some_runs_really_conflict`), so the detector is exercised by the property workload, not only by the hand-built case.

A design would need, per conflict item: the unit (merge unit path), both values with authors, and the choice "keep mine / keep theirs" implemented as a **new patch** whose `before` is the value that won, so the choice is itself an ordinary, undoable, journalled op. The detector only needs the logs of both peers, so it can run at import time on every peer and give the same answer on all of them (same logs in, same conflicts out). That is also exactly what a sequencer gives for free (section 5), because there the server refuses the stale patch.

What the spike does **not** do: it does not stop the CRDT from applying the losing write first (the document is already converged when the conflict is reported), and its `granule` function is computed at apply time from the local container layout; a remote peer's layout can differ if it saw a concurrent object/leaf replacement, which I did not test.

## 5. Alternatives (reading only; nothing run)

Retrieved from crates.io on 2026-10-13: `automerge` 0.12.0 (MIT, rust-version 1.90, has a `wasm` feature, repository github.com/automerge/automerge) and `yrs` 0.28.0 (MIT, repository github.com/y-crdt/y-crdt). I read their README and the `Cargo.toml` / the `Read::get_all` doc comment in the crate source. I did not build them, so there are no cost numbers for either.

| | Conflict on one field | Fit for our ops | Verdict |
|---|---|---|---|
| **automerge** 0.12.0 | `get_all(obj, prop)` "will return all of them, with each value tagged by the ID of the operation which created it" (verified: `automerge-0.12.0/src/read.rs`). A map key keeps **all** concurrent values, and the application chooses. | The closest match to §12 of the three: the loser is still there to build a conflict item from, without our own log. Same nested-map model; history is part of the document. | The strongest CRDT alternative **if** we want the CRDT to carry the conflict. Not measured: size, speed, wasm size. |
| **yrs** 0.28.0 | Y.Map is last-writer-wins per key by the YATA conflict-resolution rules (README names YATA; I did not find the per-key rule stated in text I could read). Not measured. | Designed for text and shared editing types. A JSON-like tree maps to nested maps and arrays. | No advantage over loro for a patch tree; its strength is text. |
| **Sequencer** (a relay or the host orders every patch; peers send `Patch`, the relay applies it to the authoritative document or rejects a stale `before`) | None. The stale writer gets `PatchError::Conflict` exactly as today. | It is our model unchanged: `before` check, atomic patch, exact inverse, journal by patch (ADR 0021), merge by unit (ADR 0025). Needs one writer at a time per document and a connection. | The smallest change to the design and the only one that keeps §12 by construction. Costs offline editing. |

## 6. Recommendation and what must be built around it

**Sequencer first. Offline editing is unresolved.**

- Live co-editing (a session with a host): peers send patches to the host's executor, which already applies them with the `before` check; others receive the applied patches in order. No CRDT in the path. Conflicts are the existing `PatchError::Conflict`, shown as the existing conflict card. This path needs nothing from loro.
- Offline or peer-to-peer editing (the case a CRDT is for): **not solved by this spike.** The idea of using loro only as a carrier and replaying remote patches through the executor, turning a stale `before` into a conflict item, does not work as stated, for two reasons found in review:
  1. **Concurrent writers diverge.** Both peers start at `0`; peer 2 writes `11`, peer 3 writes `22`. Replaying the other side's patch on each peer, each rejects it as stale (`before: 0` no longer matches), so peer 2 keeps `11` and peer 3 keeps `22`. Causal order does not order two concurrent patches. A replay protocol needs a deterministic total order or a shared tie-break that every peer applies the same way (loro's own LWW is one such rule, which is exactly the silent overwrite).
  2. **The updates do not carry the patches.** A loro update holds CRDT ops, not our `Patch` list; the patch log in the spike (`Logged`) is a separate structure that would have to travel beside the updates (journal records, ADR 0021) and be kept consistent with them.

  So the open requirements are: a **patch transport** (how the log travels), and a **convergence rule** for concurrent patches that is the same on every peer and still surfaces a conflict item. The spike shows the ingredients (the log with `saw` versions, `conflicts()`), not a protocol. A follow-up spike should demonstrate one, or choose automerge (which keeps both concurrent values) or a sequencer-only product without offline co-editing.

Whichever path is chosen, these must be built; none comes from loro:

1. **Peer ids** from a stable source (determinism, ADR 0009).
2. **Pre-validation** of every patch before it is written (no abort in a CRDT). The spike replays the patch change by change on a plain copy first, so each `before` sees what the earlier changes left, and refuses array appends and removes instead of dropping them (test `a_patch_is_validated_in_order_and_refused_whole`). The write pass then sizes each merge unit against the containers the earlier changes left, not the initial layout: a patch that replaces an object, edits it and deletes it must not delete its parent's other keys (regression in the same test, found in review of 247b6e9).
3. **A patch log with `saw` versions** per peer, kept in the journal (ADR 0021) so conflicts can be computed at import.
4. **Conflict items** and the "keep mine / keep theirs" patch that resolves them (the UX in `docs/design/ux/principles.md` already describes the card).
5. **Inverse and undo across peers**: define what undo means after a merge (refuse, as today, or rebase the inverse).
6. **Object creation and array rules**: decide whether arrays should become `LoroMovableList` (element-level merge) or stay leaves; decide how a concurrent create of one key merges.
7. **Value range** checks at the adapter (integers over i64, non-finite floats).
8. **Snapshots**: our content-addressed snapshots (ADR 0021) stay the durable form; a loro snapshot is a cache, since it holds history and is twice the size of the state.

## 7. Open questions and what this note did not do

- The **movable tree** was not exercised: no reparenting patch, no cycle test. It decides whether scene-node moves merge or lose a branch. A follow-up spike.
- **Automerge and yrs** were read, not run. If the CRDT is to carry the conflict, a second spike should run automerge on the same harness for the same table.
- **Different delivery order, same bytes** was not measured; only same-order was.
- Whether loro's winner rule for ties is by peer id or by something else: **unverified**; the demo shows one run.
- Product pages, papers and benchmarks from loro, automerge and yrs were **unreachable from the build box** and not used. `unverified (unreachable from the build box)`: https://loro.dev/docs, https://automerge.org/docs, https://docs.yjs.dev.
- The spike's patch mirror (`Change`, `Patch`) is a copy of `omuzaki-doc-api`'s types, not an import, to keep the spike out of the main workspace. Paths into arrays are supported only for replacing an existing element; append, insert and remove are refused (`Refused::Unsupported`).
- This is not legal advice. For legal review: loro pulls three MPL-2.0 crates (file-level copyleft). Keeping them unmodified and shipping the notice is what the register row already says; whether linking them statically into a closed-source binary needs anything more is not decided here.

## 8. Reproduce

From `spikes/crdt-doc/`:

    cargo test --release -- --nocapture                    # 7 tests incl. the 1,000-case property test
    cargo run --release --bin bench                        # the table in section 3
    cargo check --release --lib --target wasm32-unknown-unknown
    cargo deny --config ../../deny.toml check licenses bans sources

The spike is its own workspace and lockfile (`spikes/crdt-doc/Cargo.lock`); nothing under `crates/` changed.
