# 0019: Library items are data-only recipes that apply as one op, materialise into the document and pin themselves there (2026-10-12)

**Status:** accepted (2026-10-12). **Context:** spec v3 §28 (LB-01 to LB-15), §7, §6, §39.1, §40; `docs/design/library.md` (the full contract); ADR 0015 (registration linking), ADR 0016 (user-supplied models); `docs/design/io-api.md` D10 (licence records, part source rows, the export gate).

## Context

The library is how people and agents start a character: 150 or more first-party items over 18 sections, plus the person's own items, a team scope and later a marketplace. The spec asks for items that are recipes rather than meshes, that record their version, licence and compatibility, that register themselves, and that a locked project never changes until it is updated. It also needs every item's rights to reach the export gate, a seed that is licence-clean by construction, and agents that use the library as much as people do without asserting rights for them.

Nothing exists yet. Five facts constrain the answer:
- `library.lock` and `assets.lock` are files outside the document tree (`omuzaki-doc`), so an op can't change them in the same undo step as the document.
- `Source::Library { item }` already exists in the store's provenance.
- An op can't plan other ops against an intermediate state today.
- The part source rows only cover parts, but many items change the face, the palette, the look or a scene.
- The face-shape presets the seed would wrap start from the prototype's `sheet` profile and its close-up profile, both derived from material that cannot be shipped.

## Decision

1. **An item is one TOML manifest:** metadata, rights and a recipe of op requests with whole-value placeholders (`$character`, `$parts.<key>`, `$target`, `$name`, `$template`, `$params.<key>`). There is no code, no expression language and no mesh in it. Items come in four kinds: `part`, `setting`, `member` and `bundle`. They apply to a `character` in phase 1. The `scene` target is reserved in the schema and refused until the ST-01 scene contract lands. Ids are `publisher/name` (`omuzaki/hair/twintails`), versions are plain semver, and content is pinned by the BLAKE3 hash of the canonical manifest. A publisher is a display name plus an ed25519 key fingerprint, and the item is signed on publish; a name is never trusted without its key, and `omuzaki/` belongs to the Omuzaki publisher key.
2. **Recipes are confined.** A step may call only a registered `Edit` op whose scope is `edit:character` or `edit:scene`, may name only the bound target, and the combined patch must stay under the target node. An item can't read files, use the network, export or touch another character.
3. **`library.apply` is one op.** It plans every step against a `Scratch` copy of the document through a new `OpCtx::plan_step`, which checks each step's own scope against the request's grants. It then returns one combined patch: one undo step, and a dry-run that shows the whole change. `Scratch` (`omuzaki-doc-api`) and `plan_step` (`omuzaki-ops-api`) are additive, general contract changes.
4. **Apply materialises.** The document holds the values the recipe wrote, and building never reads the library. Each application writes a record at `<target>/applied/<app>`, a new sibling key of the character node. The record holds the pin (`item`, `version`, `hash`), the bindings and params, what the application created, the values it wrote, and the item's rights. **The records are the lock.** The `library.lock` op reports and verifies them, and the spec's separate `library.lock` file is dropped. Apply also puts the manifest into the project's store, so a project carries what it pinned.
5. **Update is a three-way merge:** the record's values are the base, a re-run of the new version on a scratch copy is theirs, and the document is mine. The person's edits win by default and conflicts are reported; `prefer: "item"` takes the item's values. It is one undo step.
6. **Rights flow and gate.** Created parts get source rows, and every application record carries the item's rights. `licence.derive` joins both, so `io.check_export` refuses `publish` for anything an item forbids, whether it is a part or not. First-party items all carry one constant: licence `CC0-1.0` (first-party items are free to use for anything), `commercial: ok`, `owned`. A record is removed only once nothing it wrote remains, so rights never drop while an item's output does.
7. **Agents use and propose; people decide.** Agents may list, search, describe, apply, update and preview. Anything an agent adds to a library lands as a candidate. Accepting a candidate, installing a non-first-party item (with an acknowledgement per item hash, as in ADR 0016) and uninstalling are the person's.
8. **First-party items register per crate, from data.** The crate that owns an item's ops keeps it in `library/seed/<section>/<name>.toml`, embedded at build time through a tiny std-only lib crate (`omuzaki-embed`). Each crate registers exactly once, with `register_library_source!`, which also adapts its existing data files (body shapes, skin tones, hand shapes, poses, expressions, light rigs) in place. Adding an item is adding a file. The ADR 0015 link gate counts one source per crate, and wasm needs no file system.
9. **The seed is clean by construction and checked.** It contains recipes over our own generators only: no binary assets, values from written design targets with an `origin` line, nothing measured or named from private material. `round-u` and `egg-oval` are re-derived before they become items. Each crate's seed test, the conformance run with goldens and `cargo xtask lint-seed` (immutability of published versions, private names) enforce it.
10. **Previews are generated artefacts.** They are not in the manifest or its hash. They are cached by item hash, template, framing and renderer build. The render that makes them is the one conformance compares.
11. **Crates:** `omuzaki-library-api` (contract), `omuzaki-library-mock` (oracle and test applier), `omuzaki-library` (registry, index, ops, merge, install), `omuzaki-embed` (lib).

## Consequences

- Spec §7: the `library.lock` line now points to the application records. Spec §28: the seed-table names that pointed at material that cannot be shipped become neutral names: round-U, egg oval, sleepy almond, long side-swept with a braid crown, soft rounded fringe.
- `omuzaki-char-api` gains `"applied"`, `"poses"` and `"emotes"` in `SIBLING_KEYS`, with their paths; `omuzaki-doc`'s docs stop listing `library.lock`. `omuzaki-io-ops` joins application records in `derive` and `check_export`.
- Two small ops are needed before their seed sections can apply: `pose.set` (the character's pose set) and `emote.set` (the emote record the emote packs need). `stage.set_light_rig` and the light-rig seed wait for the ST-01 scene contract with the scene target.
- Sharing a person's items with teammates goes only through LB-07 shared libraries under VC-10 roles; a project's store copies serve that project's own pins and are not a sharing path.
- Character files grow by one small file per application. `character.toml` stays as it would be if the person had made the edits by hand.
- Op argument changes become a public contract once a recipe uses them: new fields get defaults, renames get aliases, and anything else ships a step migration.
- Tasks: `docs/design/library.md` §7 (and the re-cut).
- Decisions taken on review (2026-10-12): publishers as name plus key; first-party items CC0-1.0; sharing only through LB-07; the scene target reserved until ST-01. They are folded in above and listed in `docs/design/library.md` §8.

**Rejected:**
- **Frozen meshes or baked parts as items:** they don't adapt to bodies, can't be edited with the part ops, and would carry binaries in the seed.
- **Live links (resolve the item at build time):** the library would become a build dependency. A project would change when an item is reinstalled, break offline or on a teammate's machine without the item, and couldn't be edited per character.
- **A lock file at the project root:** it is outside the tree, so it isn't undoable with the apply and isn't renamed with the character. That is the same staleness rejected for `licences/<id>`.
- **Storing only a hash of what an item wrote:** update would then need the old version installed and would fail whenever a recipe step reads the document. Storing the values costs one small file per application.
- **One registration call per item, or a hand-kept list per crate:** two seed PRs in the same crate would conflict on the list (§39 rules 2 and 5).
- **An expression language in recipes:** computed values belong in typed, tested generators. Body-relative parameters cover adaptation, and step guards on templates cover the rare rest.
- **Letting agents detach records or accept their own proposals:** that would let an agent drop or assert rights, which ADR 0016 keeps with the person.
