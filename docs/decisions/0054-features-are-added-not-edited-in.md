# 0054: Features are added, not edited in: generated module lists, open fact maps, non-exhaustive contracts and wiring from manifests (2026-10-14)

**Status:** accepted (2026-10-14); the work starts once the pull requests ported in the move have merged. **Context:** spec v3 §39.1 rules 1–5, 10, 11; ADR 0015 (link gate); ADR 0053 §3 (structure code so
parallel PRs can't collide). Each step lands alone, on a quiet queue.

## Context

§39.1 says a feature should be one new folder that registers itself. The registries exist (`register_op!`,
`register_part_kind!`, `register_importer!` and a dozen more, all over `inventory`), and MCP tools already come from the
op registry. Yet a few files were edited by most pull requests. A survey of the merged history from 2026-10-08 counted
why each edit happened:

| Cause | Commits | Where |
|---|---|---|
| Central `pub mod` and `pub use` lists | about 36 | `lib.rs` of `omuzaki-char-api`, `omuzaki-ops-api`, `omuzaki-anim`, `omuzaki-face-api`, `omuzaki-stage` |
| App-layer wiring | about 20 | `omuzaki-test/Cargo.toml`, hand-written `define_link!` lists, the 524-line mock `Runner` in `omuzaki-mcp/tests/common` |
| One shared struct that every capability extends, plus exhaustive destructures and struct literals of it | about 18 | `BuildCtx`, `Publishes`, `Published`, `PartOutput`, `BuiltCharacter`, `CharacterDef`, `OpCtx`, `Event` |
| Closed lists of document keys | about 7 | `SIBLING_KEYS`, `NODE_KEYS`, `paths::*` in `omuzaki-char-api/src/character.rs` |
| Real contract growth | about 12 | new `PartKind` and `Op` methods, sessions, approvals |

Only the last row is a contract change. The rest is bookkeeping that the structure forces, and it is what made
parallel pull requests collide. (The closed style enums in `omuzaki-face-api/src/profile.rs` were not a hub: two commits.
They are left as they are until a feature needs to add styles from outside the crate.)

## Decision

### 1. Module lists are generated from the folder

A crate's feature folders (`src/ops/`, `src/parts/`, `src/grammar/rules/`, ...) are declared by a build script, not by
hand. `omuzaki-embed` gains `modules(dir, visibility)`, which writes `#[path = "<absolute file>"] <vis> mod <name>;` for
every `.rs` file and every folder holding a `mod.rs`, sorted by name, into `OUT_DIR`, and prints
`cargo:rerun-if-changed` for the folder so adding a file rebuilds. The crate includes it once:

```rust
include!(concat!(env!("OUT_DIR"), "/ops_mods.rs")); // in src/ops/mod.rs
```

No new dependency (the `automod` crate documents neither folders nor rebuild tracking). Lint: `cargo xtask lint-mods`
fails a crate that has both a generated list and a hand-written `mod` line for the same folder.

### 2. No flat re-export lists in contracts

An API crate's `lib.rs` declares its modules (`pub mod kind;`) and does not re-export their items one by one. Callers
import by path: `omuzaki_char_api::kind::BuildCtx`. A crate may keep a `prelude` module of at most about ten items that
rarely change (ids, the error type). Lint: `lint-reexports` counts `pub use` items in an API crate's `lib.rs` and fails
over the limit. The move to paths is one mechanical pull request per crate.

### 3. Capability data travels in open fact maps, not in fields of one struct

What parts publish to each other (`Publishes` → `Published`) becomes a map of typed **facts**:

```rust
pub trait Fact: Send + Sync + 'static {
    /// Stable, unique name: shows in errors, describe output and the link gate.
    const NAME: &'static str;
    type Value: Send + Sync + 'static;
    /// How values from several parts combine: `One` (a second publisher is an error), `Many` (in part order) or
    /// `Keyed` (by anchor or region id).
    const MERGE: Merge;
}

publishes.put::<Stature>(stature);                 // a part's build output
let s = ctx.published.one::<Stature>();            // a later part's input: Option<&Stature>
for fit in ctx.published.many::<BodyFitFact>() {}  // in part order
```

Storage is a `BTreeMap<&'static str, Box<dyn Any + Send + Sync>>`, so order and output stay deterministic. A fact type
lives in its own module: in the API crate when it is a contract between part kinds (`head`, `face`, `fit`, `anchors`,
`colliders`, `bound_by`, `stature` move there first), or in the implementation crate when only its own kinds use it. A
new capability is a new file, not a field. Facts register (`register_fact!`), so describe ops and the link gate can list
them.

`BuildCtx` keeps its fixed inputs (part, template, palette, face, rng, quality) as fields and carries optional inputs
(services, attach, rig export) through the same typed map (`ctx.input::<Attach>()`).

### 4. Contracts that grow are non-exhaustive and built through constructors

Contract structs and enums that features extend (`BuildCtx`, `PartOutput`, `BuiltCharacter`, `CharacterDef`, `OpCtx`,
`Event`, `ErrorCode`, `OpRecord`, `Outcome`) get `#[non_exhaustive]` and constructors (`new` plus `with_*`). Other crates
never write their struct literals or match them exhaustively: a new field or variant then edits only the crate that
defines it.

- **Cache keys** that destructure a definition on purpose, so a new field can't be missed (`omuzaki-char/src/builder.rs`),
  hash its canonical serialised form instead. A new field is in the key automatically, and a test plants one to prove it.
- **Events:** `omuzaki-mcp` forwards events generically through their serialised form and matches only the ones it
  treats specially.

### 5. Document keys register themselves

`SIBLING_KEYS`, `NODE_KEYS` and the `paths::*` table become `register_doc_key!` entries next to the feature that owns the
key, with the same validation and the same saved-file names. Saved projects are unchanged.

### 6. Wiring comes from the manifest

- **Links:** an App's `link()` list is generated by its build script from its own `Cargo.toml`: every dependency whose
  manifest sets `[package.metadata.omuzaki] registers = true` gets a `link()` call. Adding a crate to an App is one
  dependency line. The link gate (ADR 0015) is unchanged and still proves nothing was dropped.
- **Tests across crates** live in per-area test crates (`omuzaki-test-char`, `-face`, `-render`, `-io`, `-ops`; kind App),
  each depending only on its area. `omuzaki-test` keeps the harness (golden compare, bless, evidence) and depends on no
  implementation crate. A render change then no longer puts the face and character tests in scope.
- **A shared mock runner:** `omuzaki-ops-mock` holds one mock `Runner` built through constructors. `omuzaki-mcp`,
  `omuzaki-cli` and the UI tests use it instead of their own copies.

### 7. Real contract growth lands first and alone

When a batch of tasks needs a new trait method, fact or input, the contract's owner opens one pull request that adds
all of them (with defaults, so implementations keep compiling), and it merges before the implementations start
(ADR 0053 §2.2). Implementations then only add files.

## Consequences

- A typical feature PR touches its own folder, its tests and, at most, one dependency line. Two features in the same
  crate no longer share a line.
- The fact map trades compile-time field checks for typed lookups: a missing fact is `None`, as an absent `Option`
  field is today, and a fact published twice under `One` is a build error with both part ids.
- Paths replace flat imports across the workspace once: about one mechanical PR per API crate.
- Checks keep it from drifting back: `lint-mods`, `lint-reexports`, the per-area test-crate rule in `lint-deps`, the
  generated-link check and the existing link gate. The compiler itself stops other crates from writing struct literals
  of a `#[non_exhaustive]` contract.

## Order of work (each lands alone)

1. `omuzaki-embed::modules` and `lint-mods`; convert `omuzaki-anim`, `omuzaki-stage` and the ops folders. (Sonnet, then
   Haiku per crate.)
2. Facts in `omuzaki-char-api` and the move of the seven published values; `BuildCtx` inputs. (Director designs the
   trait in the PR; Sonnet implements.)
3. `#[non_exhaustive]` and constructors on the contracts in §4, the cache-key hash, generic event forwarding in MCP.
   (Sonnet.)
4. `register_doc_key!`. (Sonnet.)
5. Generated `link()` lists, `omuzaki-ops-mock`, the per-area test crates. (Sonnet for the first two, then Haiku moves
   the tests.)
6. Re-exports to paths, one crate per PR. (Haiku.)
