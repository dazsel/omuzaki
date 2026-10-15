---
title: "The library contract: items as recipes, pins, search and the seed"
summary: "items as recipes, pins, search and the seed"
area: design
---

# The library contract: items as recipes, pins, search and the seed

**Status:** accepted (2026-10-12); the review decisions are folded in and listed in §8. **Decision record:** `docs/decisions/0019-library-items.md`.
**Spec:** v3 §28 (LB-01 to LB-15), §7 (project and data model), §6 (ops), §39.1 (rules 1, 2, 5, 6, 10), §40 (licensing).
**Builds on:** `omuzaki-ops-api` (`Op`, `Plan`, `Request`, scopes, services), `omuzaki-doc-api` (`Patch`, `DocRead`), `omuzaki-store-api` (`ContentHash`, `Provenance`, `Source::Library`, `licence_gate`), `omuzaki-char-api` (`CharacterDef::SIBLING_KEYS`, `paths`, `KindProvenance`), the part source rows and licence ops (`docs/design/io-api.md` D10), `io.check_export`, ADR 0015 (registration linking), ADR 0016 (user-supplied models: the provenance pattern this copies).

This page explains the decisions so that an implementer, a seed author or a reviewer can disagree with a reason. The types will be documented in the code; the page names them but doesn't repeat their docs.

## 1. What this covers

| Asked for (the brief) | Where |
|---|---|
| Item model: kinds, ids, versions, the manifest, previews, licence and provenance, private and test flags | D1, D2, D3, D8, D12 |
| `library.apply` as one transaction that writes the part source rows | D4, D5, D8 |
| Lock: id@version plus hash; a locked project never changes unless updated | D5, D10 |
| Storage: built-in, user, team (LB-07 later); content-addressed through the store; dedupe | D11 |
| Ops for people and agents: list, search, describe, apply (insert), save as item, update, fork, install; who may do what | D9 |
| Search: text, tags, facets, compatibility; embeddings later | D13 |
| Self-registration without a central list, wasm included; existing `crates/*/library/` data without moving or copying it | D14 |
| Seed build pipeline: our own procedural kit only, licence-clean by construction, tests | D14, D15 |
| Compatibility: how items and the documents that use them migrate across versions | D16 |
| The licence gate through `io.check_export` | D8 |
| A worked recipe per seed section, applied on two templates | §4 |
| Crates, contract changes in other API crates, the mock | §5, §6 |
| The task cut | §7 |

Not covered here, on purpose: the marketplace (LB-08: reviews, moderation), team roles (VC-10), signed-pack distribution (LB-02's update channel) and image search (LB-05). Each gets room in the contract (a scope value, a query field, a pack format) and its own design when its phase is planned.

## 2. The shape of it

```
 seed TOML (crates/*/library/seed/**)  ─┐                      my library (<data>/omuzaki/library/items/**)
 adapters over existing data files     ─┤ register_library_    installed packs, team scopes (later)
 (body shapes, tones, rigs, poses...)  ─┘ source! (one/crate)                 │
                                              │                               │
                                              ▼                               ▼
                                   ┌───────────────────── dyn Library (service) ─────────────────────┐
                                   │ registry: id → versions → manifest (canonical hash)              │
                                   │ index: text, tags, facets, compatibility                         │
                                   └──────────────────────────────────────────────────────────────────┘
                                      │ library.list / search / describe / lock          │ library.apply / update
                                      ▼                                                  ▼
                                 ItemInfo rows                       expand recipe (bindings, params)
                                 (panel, palette, agents)            plan each step on a Scratch copy
                                                                     (OpCtx::plan_step, same grants)
                                                                                         │
                                                                     one combined Patch = one undo step
                                                                                         ▼
                      characters/<id>/parts/...          the values the recipe wrote (materialised)
                      characters/<id>/sources/<part>     rights of each part it created (io-api D10 rows)
                      characters/<id>/applied/<app>      the application record: pin + bindings + what it wrote + rights
                                                                                         │
                                       licence.derive ◄── joins source rows AND application records
                                       io.check_export ◄── CheckExport::run: refuses publish when an item forbids it
```

## 3. Decisions

### D1. An item is a manifest of data: a recipe of op requests plus metadata, never code and never a mesh

An item is one TOML manifest. Its **recipe** is a list of op requests whose arguments may contain placeholders (D3). Applying it runs those ops (D4). So an item adapts to every body the ops adapt to, and what it makes stays editable with the same ops afterwards. There are no frozen meshes, no scripts and no expression language in an item: anything computed belongs in a generator or an op, where it is typed, tested and shared.

**Kinds.** Every manifest names one, and validation holds the recipe to it:

| `kind` | What applying it does | Examples |
|---|---|---|
| `part` | creates one or more parts it declares in `[creates.*]` | a hairstyle, a garment, an accessory |
| `setting` | sets values on things that already exist (the face, the palette, a part's parameters, a scene's rig) and creates no part | a face profile, an eye style, a colourway, a body template, a light rig, a bangs set on an existing hair part |
| `member` | adds one named member to a collection (`$name`) | an expression, a pose, an emote |
| `bundle` | applies other items (item steps, D7) and may add its own steps | an outfit (garments plus trims), an emote pack |

**Targets.** `target` says which node an item applies to and where its record goes: `character` (`characters/<id>`) in phase 1. `scene` (`scenes/<id>`) is **reserved in the schema and refused** ("the scene target is reserved until the ST-01 scene contract") until the ST-01 scene contract lands turns it on. `show` (`shows/<id>`, automation templates; designed in `docs/design/automation.md` D14) and `performance` (`performances/<id>`, score templates) arrive with those contracts in phases 2 and 4. An item never changes anything outside its target (D3, rule 2).

**Sections.** A section is the browsing group, and the list is fixed in the API crate (`Section`), in the seed table's order. A new section is an API change, which is rare; a new item never is.

| Section | Seed row (§28) | Target | Usual kind and slot | Seed data lives in |
|---|---|---|---|---|
| `body-templates` | Body templates | character | setting, slot `body-template` | `omuzaki-char` |
| `body-shapes` | (CB-04 shape presets) | character | setting, slot `body-shape` | `omuzaki-char` (adapter) |
| `skin-tones` | (CB-04 tone range) | character | setting, slot `skin-tone` | `omuzaki-char` (adapter) |
| `creatures` | Non-human templates | character | setting, slot `body-template` | `omuzaki-char` |
| `face-profiles` | Face profiles, jaw and chin | character | setting, slots `face-profile`, `jaw` | `omuzaki-char` |
| `eyes` | Eye styles, pupils, highlights | character | setting, slots `eyes`, `pupils`, `highlights` | `omuzaki-face` |
| `brows` | Brows | character | setting, slot `brows` | `omuzaki-face` |
| `mouths` | Mouths | character | setting, slot `mouth` | `omuzaki-face` |
| `hair` | Hairstyles | character | part, slot `hairstyle` | `omuzaki-char` |
| `bangs` | Bangs sets, the ahoge add-on | character | setting on a `$target` hair part, slot `bangs` (the ahoge has none) | `omuzaki-char` |
| `garments` | (the garments outfits are made of) | character | part | `omuzaki-char` |
| `outfits` | Outfits | character | bundle, slot `outfit` | `omuzaki-char` |
| `trims` | Trims, clasps, chains, emblems | character | setting or part on a `$target` garment | `omuzaki-char` |
| `accessories` | Accessories | character | part (glasses and hats take slots `glasses`, `hat`) | `omuzaki-char` |
| `colourways` | Palettes | character | setting, slot `colourway` | `omuzaki-char` |
| `looks` | Materials and looks, outline presets | character | setting, slots `look`, `outline` | `omuzaki-char` |
| `light-rigs` | Light rigs | scene (after ST-01) | setting, slot `light-rig` | `omuzaki-stage` (adapter) |
| `poses` | Poses and gestures | character | member | `omuzaki-anim` (adapter) |
| `expressions` | Expressions | character | member | `omuzaki-face` (adapter) |
| `emotes` | Emote packs | character | bundle | `omuzaki-stage` |
| `personalities` | Life-layer personalities | character | setting, slot `personality` | (phase 2) |
| `stages` | Stages | scene | bundle, slot `stage` | (phase 2) |
| `camera-moves` | Camera moves | scene | member | (phase 2) |
| `automations` | Automation templates | show | member | (phase 2) |
| `scores` | Score templates | performance | member | (phase 4) |

**The manifest** (schema 1). Required keys in bold.

| Key | Meaning |
|---|---|
| **`schema`** | manifest format version, `1` (D16) |
| **`id`**, **`version`** | D2 |
| **`section`**, **`kind`**, **`target`** | above |
| `slot` | an exclusive slot on the target: applying another item in the same slot replaces it (D6) |
| **`title`**, **`summary`** | what people and agents read; `summary` is one sentence |
| `tags` | lowercase words for search and facets |
| `titles.<lang>`, `tags_i18n.<lang>` | localised titles and tags (SE-03 locales), searched like the English ones |
| **`origin`** | one line: where the values came from (a design target, a generator's defaults, an issue). Required for first-party items; it is what a reviewer checks the item's cleanliness against (D15) |
| **`rights`** | D8 |
| `compat` | `kinds` (`humanoid`, `creature`), `templates` (ids, or `*` for every template of those kinds), `styles` (look styles it was designed for, or `*`), `requires` (part kinds the target must have). Templates and kinds are hard (incompatible items are refused and skipped by the conformance run with the reason); styles only warn |
| `creates.<key>` | for `part` items: `kind`, default `id`, `layer`, optional `parent` (another `$parts.<key>`) |
| `target_part` | for items that change an existing part (`bangs`, `trims`): the part kinds `$target` may be (`["hair"]`) |
| `name` | for `member` items: the default member name (`smug`) |
| `params.<key>` | the item's own knobs, offered at apply time: `type` (`number`, `integer`, `bool`, `choice`, `colour`), `default`, `min`/`max` or `choices`, `summary` |
| `assets.<key>` | store hashes of binaries the item needs (a decal, a pattern texture); never in first-party items (D15) |
| **`step`** | the recipe, in order: `[[step]] op = "...", args = {...}` or `[[step]] item = "...", version = "^1", ...` (D7); optional `when = { templates = [...] }` |
| `preview` | how to preview it: `framing` (`full`, `bust`, `face`, `hands`, `swatch`, `scene`), `turn` (`front`, `three-quarter`), optional `pose` (an item id) |
| `migrate` | parameter renames from older majors (D16) |
| `listed` | `false` keeps an item out of search; it is still applied as a dependency (default `true`) |
| `deprecated` | a reason; the item stays resolvable for pins and is hidden from search |

### D2. Ids are `publisher/name`; versions are plain semver; content is pinned by a canonical hash

- **Id:** two to four slug segments joined by `/` (`omuzaki_core::id::Slug` for each), at most 96 characters: the publisher's handle, then the name. First-party items use `omuzaki/<section>/<name>` (`omuzaki/hair/twintails`); `test` is reserved for fixtures.
- **Publisher:** a display name plus the fingerprint of an **ed25519 public key**, `publisher = { name, key }`, generated locally per person or team, and a `signature` over the canonical hash, added when the item is published. The signature is not part of the hash. **A name is never trusted without its key:**
  - an item's identity is its key and id together;
  - an unsigned item, or one whose signature fails, shows as "unverified publisher" whatever name it claims;
  - `omuzaki/` ids are valid only under the Omuzaki publisher key. Compiled-in sources are Omuzaki's by construction and carry no `publisher` key in their files; signed first-party packs (LB-02) are verified against the Omuzaki key built into the app;
  - two publishers with the same id are both kept, and a call that is ambiguous is refused, listing both.
  
  Verification is in the API crate (`ed25519-dalek`, BSD-3-Clause). Key generation, key storage and signing arrive with saving (LB-03) and publishing (LB-07, LB-08), alongside SE-07's signed exports. Account-backed identity comes later with the hosted identity service (VC-12).
  *Identity design (ADR 0042): keys are per person, never per team (a team item is signed by a member the roster allows); key generation, storage, signing and verification are `docs/design/identity.md`, and `PublisherKey` becomes a re-export of `omuzaki_identity_api::KeyId`.*
- **Version:** `MAJOR.MINOR.PATCH`, no pre-release or build parts, parsed by a small in-crate type (`Version`, `VersionReq` with `=x.y.z`, `^x.y` and `*`), so no new dependency. What a bump means for an item is in D16.
- **Hash:** BLAKE3 (`ContentHash`) of the manifest's canonical form: the TOML parsed into JSON, keys sorted, no whitespace, floats shortest round-trip (ADR 0009 rule 3). Comments and formatting don't change it; any value does. A published `id@version` never changes its hash (the seed lint enforces it for first-party items, install refuses a clash for the rest, D11).
- **Provenance id:** `Source::Library { item: "omuzaki/hair/twintails@1.0.0" }`, which `omuzaki-store-api` already has.

### D3. Recipes: steps, bindings and params, with three safety rules

A step's arguments are JSON. A string argument that is exactly a placeholder is replaced by a typed value at apply time; placeholders are never spliced into longer strings, and a literal string that starts with `$` is written `$$...`.

| Placeholder | Bound to |
|---|---|
| `$character`, `$scene` | the target the caller named (`$scene` is refused) |
| `$parts.<key>` | the id of a part this item creates (`creates.<key>`): the default id, or the next free `<id>-2`, `<id>-3` |
| `$target` | an existing part the item changes: the caller's `target`, or the one part of a `target_part` kind if there is exactly one (otherwise the refusal lists the candidates) |
| `$name` | the member name: the caller's `as`, else the manifest's `name` |
| `$template` | the target character's body template (read-only; for `when` guards and the rare argument that names it) |
| `$params.<key>` | the item's parameter: the caller's value, else the default |
| `$assets.<key>` | the item's asset hash |

Values are **body-relative** wherever the generator allows it (head heights, fractions of a limb, head-local profile units), so the same recipe writes the same values on every template and the build does the adapting. `when = { templates = [...] }` on a step exists for the rare case a generator can't; a seed item that uses it says why in `origin`.

**Safety rules,** checked when an item is loaded (it is listed as unavailable, with the reason) and again on every apply:
1. **Edits only, within a ceiling.** A step names a registered op of kind `Edit` whose scope is `edit:character` or `edit:scene`. Queries, jobs, and ops needing `fs`, `net`, `publish`, `spend`, `broadcast`, `perform`, `render` or `native` are refused. An item can't read files, reach the network or export anything.
2. **Confined to the target.** Arguments named `character` or `scene` must be the bound placeholder, and after planning, every change in the combined patch must lie at or under the target node (`characters/<id>/...`). An item can't touch another character.
3. **Known ops only.** A step whose op is not registered in this build (an item from a newer app, or a plugin op that isn't installed) makes the item unavailable, naming the op. Nothing is guessed.

### D4. `library.apply` is one op that plans every step on a scratch copy and returns one patch

`library.apply {item, version?, character? | scene?, target?, as?, params?, replace?, preview?}`:
1. **Resolve** the item (D11 order) and check compatibility with the target (template, kind, required parts; style only warns).
2. **Bind** the placeholders (D3) and validate `params` against the manifest's declarations.
3. **Plan** each step in order against a `Scratch` document (the current document plus every earlier step's patch), with `OpCtx::plan_step`: it decodes the step through the op registry, checks the step op's own scope against the request's grants (as the runner would: an edit's scope is needed when applying, not for a dry-run), and plans it with the same services, file roots and dry-run flag. A step that fails stops the apply; the error names the step (`step/3`), the op and the op's own field.
4. **Combine** the step patches with `Patch::extend` and add the application record (D5) and the source rows of created parts (D8). The result is one `Plan`: one op record, **one undo step**, and a dry-run that shows the whole change.

The output (also the dry-run summary) lists the resolved `id@version` and hash, the application key, the parts created (with their final ids), what was replaced (D6), parameters used, a rights note (D8), compatibility warnings and, with `preview: {template?, framing?}`, a preview render of the scratch document made by planning `render.preview` on it (its scope is checked like any step's). The Library panel's hover preview uses that, so there's no second preview path.

Two contract additions make this possible; both are additive and general (scripts and future composite ops need them too):
- `omuzaki-doc-api`: `Scratch`, an owned in-memory document (`DocRead + DocWrite`) made from any `DocRead`. Its patch application is the one shared implementation that `omuzaki-doc-mock` then reuses instead of keeping its own.
- `omuzaki-ops-api`: `OpCtx::plan_step(op, args, doc) -> Result<(OpMeta, Plan), OpError>`, as described in step 3. Ops still never see the grants.

`library.apply`'s own static scope is `edit:library`; the steps' scopes are checked one by one, so an agent with `edit:character` but not `edit:scene` can apply a hairstyle but not a light rig, and the refusal names the scope to grant.

### D5. Apply materialises; the application record pins the item and is the lock

After an apply, the document holds every value the recipe wrote, as if a person had made the same edits. **Building a character never reads the library.** So a project opens and builds the same with or without the item installed, on any machine and on wasm, and "a locked project never changes unless updated" holds by construction: nothing changes until someone runs `library.update`.

What the library still needs to know about an application goes into its **record**, beside what it changed: `characters/<id>/applied/<app>` (or `scenes/<id>/applied/<app>`). `applied` joins `CharacterDef::SIBLING_KEYS` (an additive `omuzaki-char-api` change, like `licence` and `sources`), so the record moves with the character on rename, duplicate and delete and merges per application, for the same reasons as `licence`. By the project store's layout rules each record is its own file, `characters/<id>/applied/<app>.toml`.

| Record key | Holds |
|---|---|
| `schema` | `1` |
| `item`, `version`, `hash`, `publisher` | the pin: the exact manifest applied, and its publisher key fingerprint |
| `from` | where it was resolved: `first-party`, `mine`, `file`, later `team:<id>` |
| `slot` | the slot it holds, if any |
| `bindings` | the resolved `parts`, `target`, `name` |
| `params` | the parameter values used, defaults filled in |
| `deps` | for bundles: each nested item's `version` and `hash` (D7) |
| `rights` | the item's rights as a source row (D8) |
| `created` | the paths it created (relative to the target node), for replacement and update |
| `wrote` | every value it wrote, keyed by path relative to the target node (created subtrees whole), excluding source rows and the record itself |

`wrote` is the base of the three-way update (D10) and what shows a person which values they changed since applying ("overridden" in the Inspector, with a reset). It duplicates the values, but in a file of its own, so `character.toml` stays as clean as hand-made, and the update never depends on an old item version still being installed.

**The records are the lock.** The spec's `library.lock` file (§7) becomes these records: they are in the document tree, so they are undoable with the apply and the update, move with their character, and merge per application. A separate root file would be outside the tree (`omuzaki-doc` doesn't load it) and so neither undoable nor renamed with the character. The `library.lock` op (D9) is the read side: it lists every pin in the project and checks that each resolves. **Choosing a version:** a call without `version` takes the version already pinned for that id in the same target, else the newest compatible installed version, and names its choice in the output. Pass `version` when replaying a script, so a replay never picks a newer item.

The apply also puts the canonical manifest (and any assets) into the project's store (`Store::put`, idempotent, first write wins; never in a dry-run). So a project carries what it pinned: the project's copies serve only that project's own applications (describe, update, replay of the op log by hash). They are not a way to share items. Applying an item anew needs it in a library the person has: their own, or a team library (LB-07, D11). An unreferenced copy is collected by the store's garbage collection.

### D6. Slots, replacement and re-apply

- **Same slot, different item:** the new item replaces the old one in one transaction. The parts and members the old application created are removed (with their source rows), its record goes, and the new item applies. Values the old item set that the new one doesn't set keep their current values. The dry-run lists every edit the person made to what's being removed ("your change to `parts/hair/params/groups/tails/...` is discarded"); undo brings everything back.
- **Same item again:** this is an update (D10) to the requested version, or to the same version with new `params`. "Re-apply with longer tails" keeps the person's other edits.
- **Parts that aren't from the library** are never removed implicitly. If the character already has a hand-made part of a kind the item creates, the dry-run warns and names it; `replace: ["hair"]` removes it in the same transaction. Agents read the warning and call again; the UI shows a checkbox.
- **Items without a slot** stack (accessories, garments, trims, members). Their application key is the item's last id segment (`headphones`, then `headphones-2`); for members, `<section>-<name>`.
- **Member names:** a name taken by a member that isn't from this item is refused with a hint to pass `as`.

### D7. Bundles and dependencies are item steps

A bundle names other items as steps: `[[step]] item = "omuzaki/garments/pleated-skirt", version = "^1", params = {...}, bind = { target = "$parts.shirt" }`. That is the whole dependency mechanism; there is no separate list to keep in step with the recipe. Rules:
- **Binding across steps:** in a bundle, `$parts.<key>` also names the parts its nested items create, by the nested item's own `creates` key, so a later step can bind to them. Two nested items with the same key are refused when the bundle is loaded, naming both.
- **Resolution:** the version already pinned in the target's records if it matches the range, else the newest installed match; none is a refusal naming the missing item and the install hint.
- **One record per top-level apply.** The bundle's record covers everything its nested items wrote (one `wrote`, one `created`) and pins each nested item in `deps`. Updating the bundle updates its nested items (D10); a nested item can't be updated on its own while the bundle holds it.
- **Bounded:** at most four levels, no cycles (checked when the registry is built; the error names the cycle).
- **Rights:** the bundle's record carries the strictest join of its own rights and every nested item's (D8). Each created part's source row carries the rights of the item that created it.

### D8. Rights: what an item records, where they go, and the licence gate

**What an item records.** `rights` holds what a part source row holds (`docs/design/io-api.md` D10), so apply writes rows without translating them:

| Key | Meaning |
|---|---|
| `author`, `licence`, `attribution`, `commercial`, `consent`, `private_test` | the `Provenance` fields; `source` is implied (`Library { item: "id@version" }`) |
| `terms` | optional VRM-meta-shaped permissions the item grants for what it makes; what it leaves out is the most restrictive value |
| `owned` | the result belongs to whoever applies it, so the part rows carry `owned { asserted_by }` and the character's record may grant more than the defaults (io-api D10's project-owned exception) |

- **First-party items** all carry the same rights, a constant in the API crate (`ItemRights::first_party()`): author `Omuzaki`, licence `CC0-1.0`, `commercial: ok`, `private_test: false`, `owned` (asserted by "Omuzaki first-party library item <id@version>"). They are parameters for our own `ProjectOwned` generators, so the output is as much the person's as a part they made by hand. **The project chose CC0-1.0** ("free to use to do whatever"): anyone may use, change and sell what they make with the items, with no attribution needed. The seed lint checks every first-party item carries it.
- **"My library" items** (LB-03): the strictest join of what they were saved from, so saving can't launder anything. `owned` only if everything captured was owned, and then asserted by the person who saved it.
- **Installed third-party items:** the declared rights as they are, shown with a disclaimer and acknowledged per item hash by a person, the ADR 0016 pattern. A missing licence reads as `commercial: unknown`. Assets inside an item carry their own store provenance, and the item's effective rights are the strictest join of its declared rights and every asset's.
- **Private test material** (`private_test`) can exist in a person's own library (a person's private tests), never in the seed (D15). It applies normally and makes the character private-test, which the gate never lets out.

**Where rights go on apply.** Each part an item creates gets a source row at `characters/<id>/sources/<part>`, written with `row_patch` in the same patch (the part-source contract; `PartSource` moves to `omuzaki-char-api` there). The application record carries the item's rights too, because many items change things that aren't parts: the face, the palette, the look, members, a scene. **`licence.derive` joins the records as well as the rows** (a `omuzaki-io-ops` change). Without that, a non-commercial face style would slip past the gate.

**The gate.** `io.check_export` (`CheckExport::run`) compares the character's licence record with `derive`. So an application whose item is `commercial: no` or `unknown`, or `private_test`, fails every `publish` check. The finding is located at `characters/<id>/applied/<app>` and names the item, version and licence; its hint is to apply a cleared item in the same slot or remove what it created. Personal (`private`) use is never blocked. The library also informs earlier:
- the apply summary says when an item will block publishing;
- `library.search {usable_for: "publish"}` filters such items out;
- each result row carries a licence badge.

**Rights never drop while an item's output remains.** `library.detach` (D9) removes a record only once nothing it wrote remains (every `wrote` value is gone or changed). Slot replacement removes what the old item created, so its record goes with it. Editing a value by hand doesn't clear the item's rights; only removing or replacing what it made does.

### D9. The ops, and what an agent may do

| Op | Kind and scope | Does | Agent | Person |
|---|---|---|---|---|
| `library.list {section?, scope?, offset, limit}` | query, `read` | items in section order then id; section counts when no section is given | yes | yes |
| `library.search {text?, tags?, section?, kind?, scope?, compatible_with?, usable_for?, offset, limit}` | query, `read` | ranked `ItemInfo` rows and facet counts (D13) | yes | yes |
| `library.describe {item, version?}` | query, `read` | the manifest, installed versions, availability, the recipe, rights explained, previews | yes | yes |
| `library.lock {character? \| scene?}` | query, `read` | every application in the project (or target): pin, `resolves` (`ok`, `missing`, `mismatch`), the newest compatible version, overridden paths, `stale` | yes | yes |
| `library.apply {...}` (the "insert") | edit, `edit:library` plus each step's scope; undoable | D4 to D7 | yes | yes |
| `library.update {character? \| scene?, application, to_version?, params?, prefer?}` | edit, as apply; undoable | D10 | yes | yes |
| `library.detach {character? \| scene?, application}` | edit, `edit:library`; undoable | removes a stale record (D8); refuses one whose output remains, listing the paths | yes | yes |
| `library.install {from}` | edit, `edit:library` (+ `fs`/`net` by source); `Undo::Explicit` | validates and adds items to the person's library (D11) | first-party signed packs only (LB-02); anything else returns `needs_person` | yes, with the acknowledgement |
| `library.uninstall {item, version?}` | edit, `edit:library`; `Undo::Explicit` | removes from the person's library; projects keep their pins and copies | `needs_person` | yes |
| `library.save_as_item {from, id?, title, summary, tags, section, params?}` | edit, `edit:library`; `Undo::Explicit` | turns current values (parts, face, palette, look, a member) into a recipe item (LB-03, phase 3) | lands as a **candidate** | saved |
| `library.fork {item, version?, id, title?}` | edit, `edit:library`; `Undo::Explicit` | copies an item into the person's library with `forked_from`, rights carried | lands as a candidate | saved |
| `library.accept {candidate}` / `library.reject {candidate, reason?}` | edit, `edit:library` | moves a candidate into the library, or drops it with a reason the agent sees | reject its own only | yes |
| `library.render_previews {item?, templates?}` | job, `render` | renders and caches previews (D12) | yes | yes |
| `library.publish {item, to}` | job, `publish` (LB-07/LB-08, later) | shares to a team or the marketplace | `needs_person` | yes |

The principle, as in ADR 0016: **agents use and propose; people decide what enters a library and what rights are asserted.** Agents may apply, update and preview anything, because the document change is undoable and visible in history. Agents never assert ownership or rights, never add to a library except as candidates (proposals land for the person to accept, LB-03), and never remove anything outside the document. `needs_person` is the structured refusal an op returns when a person must decide; once the approval queue exists the request queues instead of failing.

### D10. Update is a three-way merge: the record is the base, the person's edits win by default

`library.update` resolves the new version (`to_version`, else the newest compatible in the same major), checks compatibility (a version that drops the target's template is refused with the reason), carries `params` over (renamed by `migrate`, dropped with a note if no longer declared, new ones at their defaults), then:
1. **Base:** `B` = the record's `wrote`.
2. **Theirs:** make a `Scratch` of the document with the application's `created` paths removed and every other `wrote` path reset to its `B` value (the document as it was right after the old apply, around the person's other work), plan the new recipe on it with the same bindings, and take `N` = the values it writes.
3. **Mine:** `C` = the document now.
4. Per leaf path (arrays are leaves) in `B ∪ N`:

   | Case | Result |
   |---|---|
   | `C = B` (untouched since apply) | `N`; if the new version no longer writes the path, a created path is removed, a set path keeps `C` |
   | `C ≠ B`, `N = B` (only the person changed it) | `C` |
   | `C ≠ B`, `N ≠ B` (both changed it) | `C`, reported as a conflict; `prefer: "item"` takes `N` |
   | a new path (`B` absent) | `N` if `C` is absent or equal; otherwise a conflict as above |
   | a created part the person removed | stays removed |

5. The patch moves the document from `C` to the result, rewrites the record (`version`, `hash`, `params`, `deps`, `created`, and `wrote = N`, so the person's kept edits still read as edits next time) and rewrites the created parts' source rows from the new rights.

The dry-run is the **diff preview**: each changed path with before, after and why (`item`, `kept`, `conflict`), the parameter changes, the rights change if the licence moved, and a preview render on request. Apply is one transaction, so one undo restores the record and the document together.

### D11. Storage, scopes and content addressing

| Scope | What | Where | Notes |
|---|---|---|---|
| First-party | the seed set that ships with this build | compiled in (D14) | read-only; signed update packs (LB-02, phase 3) add versions under `packs/` |
| My library | the person's items across projects | `<data dir>/omuzaki/library/`: `items/<publisher>/<name…>/<version>.toml`, `candidates/…`, `acks.toml`, `store/` | one canonical manifest per version; binaries in the content-addressed `store/` (`omuzaki-store`) |
| Project | what this project pinned | the project's store, by hash (D5) | materialised values and records in the document; manifests and assets in the store |
| Team (LB-07, later) | a shared scope with roles (VC-10) and review state (VC-11) | a remote source with a local cache | **the only way to share a person's items with teammates**: no ad hoc path (no export of "my library" items, no promotion from a project's store); `ItemScope::Team(id)` is already in the contract so search and records have a place for it; its roles, membership log and review records are `team.md` §11.2 |

- **Content addressing and dedupe:** manifests and assets are stored by hash, so the same bytes are stored once per store, whatever ids or projects use them. The same `id@version` with the same hash from two scopes is one entry. The same `id@version` with a **different** hash is a conflict: first-party wins for `omuzaki/`; otherwise install refuses ("already installed with different content; bump the version").
- **Resolution order** for an id without a pin: first-party, then my library (newest compatible version), then team scopes. A project pin always wins (D5).
- **The registry** is the `Library` service (`omuzaki-library-api`), implemented in `omuzaki-library` and injected by the app like the store. It loads the compiled-in sources at startup and the person's library from disk, and updates its index on install.

### D12. Previews and thumbnails are generated, cached, and not part of an item's identity

A preview is a render of the item applied to the **neutral preview character** of a template: `character.create` defaults, an original design with nothing from private material, posed and framed by the manifest's `preview`. Swatch sections (colourways, looks) render a swatch card plus the bust. Sizes: 512 px for the panel's large view and the conformance goldens, 128 px thumbnails for tiles; PNG, sRGB, transparent background.

Previews are **not in the manifest and not in its hash.** A better renderer would otherwise change every item's identity and force a version bump on every item. They are cached in the person's store, keyed by `(item hash, template, framing, renderer build)`. `ItemInfo` carries the preview hash for the requested template when one is cached; otherwise it carries the default template's preview and a render is queued. The app ships one 128 px thumbnail per first-party item on its default template (a few MB), rendered by `library.render_previews` in the release pipeline, and renders the others on demand. The render that makes a preview is the one the conformance run compares with its golden, so there is one render path.

### D13. Search: one index, text and tags and facets, embeddings later

- **One index** in the `Library` service, built once from the registry and updated on install; the Library panel and the command palette both call `library.search`. Building it allocates; a query allocates only its result list.
- **Text:** title, localised titles, tags, summary and id, lowercased, accent-folded (NFKD; `unicode-normalization`, MIT/Apache-2.0, added to the register by the search task) and split into words. CJK titles also index character bigrams, so a Japanese, Chinese or Korean query matches without a dictionary.
- **Ranking:** exact id or title, then tag, then word prefix, then fuzzy (edit distance 1 for words of five letters or more). Ties go by section order, then id, so results are deterministic.
- **Filters and facets:** `section`, `kind`, `tags` (AND), `scope`, `usable_for` (`publish` hides items that would block it), and `compatible_with` (a character or scene: incompatible items are filtered out, style mismatches stay but rank lower and carry the warning). Every response carries counts per facet value for the panel's chips.
- **Rows:** `ItemInfo` = id, version, title, summary, section, kind, tags, scope, a rights badge (licence, commercial status, owned, private test, publish-ok), the compatibility status and reason, the preview hash, and `unavailable` with its reason.
- **Budgets:** a query takes ≤ 2 ms p95 over 300 items and ≤ 20 ms over 10,000 (benchmarks in the crate). Not on the real-time path.
- **Later, image search (LB-05):** `Query.image` (a store hash) and `Query.like` (an item id) are in the query type now and answer `Unsupported` until LB-05. Then an embedding service (the vision connector CN-09 or a local model in an ML slot, under the licence policy or ADR 0016) embeds previews per template. Ranking uses cosine similarity, under the same filters, so "hair like this" respects compatibility.

### D14. First-party items register themselves per crate, from data, on every target including wasm

- **Where seed data lives:** the crate that owns the ops an item uses owns the item, in `crates/<crate>/library/seed/<section>/<name>.toml`, one item per file, the file name being the id's last segment. Adding an item is adding one file.
- **How it's embedded:** a build script globs that folder and writes a sorted table of `(path, include_str!(...))` into `OUT_DIR`. The helper is one function in a small std-only lib crate, `omuzaki-embed` (`layer = "lib"`, ADR 0011), so the four crates don't each keep a copy, and the other data folders (`body-shapes`, `skin-tones`, `hand-shapes`, `light-rigs`, `poses`) can use it too instead of listing files by hand. The data is compiled in, so wasm needs no file system.
- **How it registers:** one `register_library_source!(Seed)` per crate, at item level in `src/library_items.rs`. `Seed` implements `ItemSource` (`id()`, `items() -> Vec<ItemManifest>`). The macro records the registration the way ADR 0015 requires, so the link gate counts one source per crate. The crates that own seed data already define `link()` for their ops; no new wiring is needed. There is **one registration per crate, not per item**, so the gate never needs to know how many items exist, and no list grows when items are added.
- **Existing data, without moving or copying it:** the same `ItemSource` also turns each file of the crate's existing typed libraries into an item in code (an adapter). For example, each `library/body-shapes/*.toml` becomes `omuzaki/body-shapes/<id>` with the recipe `body.apply_shape_preset {preset: <id>}`. The data file stays where it is and is still read by its own loader. It gains three optional keys its owner adds in place: `title`, `tags` and `version` (default `1.0.0`). Adapters cover body shapes and skin tones (`omuzaki-char`), poses (`omuzaki-anim`), the expression library (`omuzaki-face`) and, once the scene target is on, light rigs (`omuzaki-stage`). Hand shapes are not a section: poses use them. Small sets with no data file of their own, such as the five face-shape presets, are plain manifests that name the preset by id; that is a reference, not a copy.

### D15. The seed is built from our own procedural kit, never from private or test material, and is checked

**Licence-clean by construction:**
- Seed items are recipes over our own generators. They carry **no binary assets**: a decal, a pattern or an emblem is a parameter of a generator, drawn by our code at build time.
- Their values are designed: from geometric definitions, a generator's defaults, or a written design target. They are never measured or fitted from a reference. `origin` says which, and the reviewer checks it.
- Nothing is derived from private test material (§40, `docs/licensing/policy.md`): not private character models or concept sheets, not recordings of people. Items never name third-party characters or creators.
- **Two existing data sets fail this today and must be re-derived before they become items:** the face-shape preset `round-u` was to start as the prototype's `sheet` profile, which was fitted to a character sheet that cannot be shipped. It is defined geometrically instead. `egg-oval` was to start as a profile fitted to a test clip and is re-derived the same way. Both block the face-profile seed.
- The spec's seed table named five items after private material. They now have the neutral names the seed tasks use.

**Checks:**

| Check | Where | Catches |
|---|---|---|
| Manifest parses; schema, ids, sections, kinds, slots, params, bindings valid; every step's op registered, of kind edit, under the ceiling, and decodes after expansion with sample bindings; rights equal `ItemRights::first_party()`; `origin` present; no `assets`; dry-run apply on the mock character | each owning crate's `tests/library_seed.rs` (per PR) | malformed items, unsafe recipes, wrong rights |
| Every item applies on every compatible template, builds, renders front and three-quarter, and matches its golden; incompatible pairs are skipped with the reason | `omuzaki-test` conformance | items that break on a body |
| Ids unique and under `omuzaki/`; file name matches the id; no id, title, tag or `origin` names private material (the list in the policy's private-test section); **an `id@version` whose canonical content changed since `origin/main` without a version bump** | `cargo xtask lint-seed` (git-aware, no build; in `cargo xtask check`) | silent edits to published versions, private names |
| Taste: a contact sheet per seed PR (design target, before, after) signed off by a maintainer | seed task acceptance | items that are valid but not good |

### D16. Compatibility: how items, recipes and records migrate

- **Item versions** (semver for items):

  | Bump | Allowed changes |
  |---|---|
  | patch | values only |
  | minor | new optional params, new tags, wider compatibility, new steps writing new paths |
  | major | removed or renamed params, changed created ids, slot, kind or target, or narrower compatibility |

  `library.update` stays within the major by default. Crossing a major needs `to_version`, and params move through the manifest's `migrate.params` renames. The seed lint compares a changed first-party item with its previous version and refuses a bump too small for the change.
- **Manifest format:** `schema` is checked on load. An older schema is migrated in memory by append-only migrations in `omuzaki-library-api` (one file each, as the project store does). A newer schema makes the item unavailable ("made by a newer Omuzaki").
- **Op arguments** are a public contract once a recipe uses them. A new field must have a default; a renamed field keeps its old name as a `serde(alias)`. A removal or a change of meaning needs a step migration: an idempotent rewrite registered beside the op (`register_step_migration!`), applied to recipes on load. The seed conformance run breaks in the same PR that breaks an op, so first-party items are fixed with it. For user and third-party items, an unmigrated break makes the item unavailable, naming the op and field; it is never applied half-right.
- **Records in the document** migrate with the project's schema migrations (`omuzaki-doc`), like any other content. A record whose item can't be resolved anymore doesn't stop anything: the document is materialised. `library.lock` reports `missing`, and update is unavailable until the item is installed again (the project's store usually still has it, D5).

## 4. Worked recipes: one item per seed section, applied on two templates

Op names and arguments are the ones on `main` or in the open tasks named; parameter values are illustrative. Every manifest below also carries `schema = 1`, `version = "1.0.0"`, the first-party `rights`, `origin`, `tags` and `preview`; they are left out to keep the examples short.

**4.1 Body shape: `omuzaki/body-shapes/curvy`** (adapter over `crates/omuzaki-char/library/body-shapes/curvy.toml`)
```toml
id = "omuzaki/body-shapes/curvy"
section = "body-shapes"
kind = "setting"
target = "character"
slot = "body-shape"
title = "Curvy"
summary = "Full bust and hips with a defined waist, wider below than above."
compat = { kinds = ["humanoid"], templates = ["*"] }
params.blend = { type = "number", default = 1.0, min = 0.0, max = 1.0, summary = "How far to move from the current shape" }
[[step]]
op = "body.apply_shape_preset"
args = { character = "$character", preset = "curvy", blend = "$params.blend" }
```
| Applied to | Writes (same on both) | What the build does with it |
|---|---|---|
| `mia` on `soft-sd` | the body part's five shape axes (`height 0, build 0.3, curves 1, musculature -0.2, shoulder_hip -0.4`, preset `curvy`); record `applied/body-shape` | the axes are relative to the template, so the soft-sd body widens at the hips by soft-sd's own ranges |
| `kai` on `tall` | the same five values | the tall template's ranges give a taller, longer-waisted curvy figure |

**4.2 Face profile: `omuzaki/face-profiles/mochi`** (op `face.set_profile`)
```toml
id = "omuzaki/face-profiles/mochi"
section = "face-profiles"
kind = "setting"
target = "character"
slot = "face-profile"
title = "Mochi"
summary = "A wide, soft round face with low-set features and no chin point."
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }
[[step]]
op = "face.set_profile"
args = { character = "$character", profile = "mochi", blend = 1.0 }
```
On `soft-sd` and `mini` the op writes the same head-local outline and `face/layout` numbers. The head generator scales them to each template's head, so the mini head (3-head proportions) gets the same mochi shape at its own size. The record's `wrote` holds the outline fields and the layout ratios; a later `library.update` to 1.1 that only retunes the chin leaves the person's eye-spacing edit alone (D10).

**4.3 Hairstyle: `omuzaki/hair/twintails`** (op `part.add`, merged; generators cap, bangs and tail)
```toml
id = "omuzaki/hair/twintails"
section = "hair"
kind = "part"
target = "character"
slot = "hairstyle"
title = "Twintails"
summary = "Two high tails tied above the ears, with a light fringe."
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["head"] }
creates.hair = { kind = "hair", id = "hair", layer = 5 }
params.tail_length = { type = "number", default = 2.2, min = 0.8, max = 3.5, summary = "Tail length in head heights" }
[[step]]
op = "part.add"
[step.args]
character = "$character"
kind = "hair"
id = "$parts.hair"
layer = 5
[step.args.params.groups.cap.generator.cap]
hairline_front_deg = 18.0
thickness = 0.012
[step.args.params.groups.fringe.generator.bangs]
# the bangs generator's fields
[step.args.params.groups.tails]
mirror = true
generator.tail = { anchor = "crown_side", length = "$params.tail_length" } # tail fields
```
| Applied to | Result |
|---|---|
| `mia` on `soft-sd`, no hair yet | creates `parts/hair` and `sources/hair` (first-party, owned); record `applied/hairstyle` with `created = ["parts/hair"]` |
| `kai` on `tall`, a hand-made `hair` part already there | the dry-run warns that `hair` isn't from the library; without `replace`, the new part is `hair-2`; with `replace = ["hair"]`, the old part goes in the same transaction |

The tails are 2.2 head heights on both. Because head heights differ, they reach the shoulder blades on soft-sd and mid-back on tall: same recipe, adapted by the generator.

**4.4 Garment: `omuzaki/garments/pleated-skirt`** (op `part.add`; skirt generator)
```toml
id = "omuzaki/garments/pleated-skirt"
section = "garments"
kind = "part"
target = "character"
title = "Pleated skirt"
summary = "A knee-length skirt with knife pleats and a waistband."
compat = { kinds = ["humanoid"], templates = ["*"], requires = ["body"] }
creates.skirt = { kind = "skirt", id = "skirt", layer = 30 }
params.length = { type = "number", default = 0.85, min = 0.3, max = 1.6, summary = "Hem height as a fraction of hip-to-knee" }
[[step]]
op = "part.add"
args = { character = "$character", kind = "skirt", id = "$parts.skirt", layer = 30, params = { length = "$params.length", pleats = 16, colour = "@outfit_a" } }
```
On `mini` (short legs) and `standard`, the same `length = 0.85` lands just above each body's own knee, because the garment fits the `BodyFit` it is given. The colour is the palette slot `outfit_a`, so a colourway item (4.5) recolours it.

**4.5 Colourway: `omuzaki/colourways/night-iris`** (op `palette.set`, merged)
```toml
id = "omuzaki/colourways/night-iris"
section = "colourways"
kind = "setting"
target = "character"
slot = "colourway"
title = "Night iris"
summary = "Deep violet and ink blue with a cool silver accent; value-separated for small sizes."
[[step]]
op = "palette.set"
args = { character = "$character", slot = "hair_root", colour = "#2a2346" }
[[step]]
op = "palette.set"
args = { character = "$character", slot = "hair_mid", colour = "#463a78" }
# ... one step per standard slot: hair_tip, outfit_a, outfit_b, trim, eyes, lip, blush (skin slots stay with the skin tone)
```
Palette values don't depend on the template: on `soft-sd` and `tall` the record's `wrote` is the same `palette/<slot>` values. The seed test runs the contrast checker on every colourway.

**4.6 Light rig: `omuzaki/light-rigs/candle`** (adapter over `crates/omuzaki-stage/library/light-rigs/viewer-candle.toml`; op `stage.set_light_rig`). **After ST-01:** the `scene` target is refused.
```toml
id = "omuzaki/light-rigs/candle"
section = "light-rigs"
kind = "setting"
target = "scene"
slot = "light-rig"
title = "Candle"
summary = "A warm flickering key from a candle, a cool rim and a low warm fill."
[[step]]
op = "stage.set_light_rig"
args = { scene = "$scene", rig = "viewer-candle" }
```
It applies to a scene of the ST-01 contract; the record is `scenes/<id>/applied/light-rig`. The conformance run lights a `soft-sd` and a `tall` character with it; the record is identical, and only the renders differ.

**4.7 Pose: `omuzaki/poses/wave`** (adapter over `crates/omuzaki-anim/library/poses/wave.toml`; op `pose.set`)
```toml
id = "omuzaki/poses/wave"
section = "poses"
kind = "member"
target = "character"
name = "wave"
title = "Wave"
summary = "A one-hand wave at shoulder height, palm out."
compat = { kinds = ["humanoid"], templates = ["*"] }
[[step]]
op = "pose.set"
args = { character = "$character", id = "$name", from = "wave" }
```
`pose.set` copies the library pose into the character's pose set (`characters/<id>/poses/wave`), so it stays editable per character. Rotations are normalised humanoid rotations, so on `soft-sd` and `standard` the same values pose arms of different lengths correctly. On a creature template the item is refused (`kinds = ["humanoid"]`) and the conformance run skips it with that reason.

**4.8 Expression: `omuzaki/expressions/smug`** (adapter over the expression library; op `face.expression.set`)
```toml
id = "omuzaki/expressions/smug"
section = "expressions"
kind = "member"
target = "character"
name = "smug"
title = "Smug"
summary = "Half-lidded eyes, one brow up and a sideways smirk."
[[step]]
op = "face.expression.set"
args = { character = "$character", id = "$name", kind = "mood", deltas = { eyeSquintLeft = 0.35, browOuterUpRight = 0.4, mouthSmileRight = 0.6 } }   # curve defaults to linear
```
Expressions are deltas on the face channel bus, independent of the body, so `soft-sd` and `mini` get identical values; the painter draws them through each face profile. A character that already has its own `smug` gets a refusal with the hint to pass `as = "smug-2"`.

**4.9 A bundle: `omuzaki/outfits/school-uniform`** (shows D7)
```toml
id = "omuzaki/outfits/school-uniform"
section = "outfits"
kind = "bundle"
target = "character"
slot = "outfit"
[[step]]
item = "omuzaki/garments/shirt"
version = "^1"
[[step]]
item = "omuzaki/garments/pleated-skirt"
version = "^1"
params = { length = 0.8 }
[[step]]
item = "omuzaki/trims/piping"
version = "^1"
bind = { target = "$parts.skirt" }   # the skirt the previous step created
```
One record, `applied/outfit`, pins the bundle and, in `deps`, each nested item. Applying `omuzaki/outfits/gothic-dress` later replaces the whole outfit in one undo step (D6).

## 5. Crates and contract changes

| Crate | Kind | Holds | Depends on |
|---|---|---|---|
| `omuzaki-library-api` | API | `ItemId`, `Version`, `VersionReq`; `ItemManifest` (parse from TOML, canonical hash, `check`), `Section`, `ItemKind`, `Target`, `ItemRights`; recipe expansion (`expand`: bindings and params to concrete step requests, pure); `ApplicationRecord` and its paths; `ItemSource`, `register_library_source!`, `sources()`; the `Library` service trait with `Query`, `SearchPage`, `ItemInfo`, `Facets`; errors; manifest migrations | `omuzaki-core`, `omuzaki-ops-api`, `omuzaki-doc-api`, `omuzaki-store-api`, `omuzaki-char-api` (`PartSource`, `CharId`, `paths`) |
| `omuzaki-library-mock` | Mock | an in-memory `Library` over fixture items (ids under `test/`), `apply_for_test` (plans a recipe on a `Scratch` with `plan_step`, no records: what seed tests need), and the `conformance` suite every `Library` passes | API crates only |
| `omuzaki-library` | Implementation | the registry and index, the user library on disk, the `library.*` ops, the three-way merge, install, previews | API crates (mocks as dev-dependencies) |
| `omuzaki-embed` | Lib (`layer = "lib"`) | the build-script helper that embeds a data folder (D14); std only | nothing |

`cargo xtask lint-deps` accepts all of this as it stands: implementations depend on API and lib crates, and the mock on API crates. The owning crates (`omuzaki-char`, `omuzaki-face`, `omuzaki-anim`, `omuzaki-stage`) depend on `omuzaki-library-api` for `ItemSource` and on `omuzaki-embed` as a build dependency.

**Changes to other API crates** (all additive; each in its own task, in the API crates' own change):
- `omuzaki-ops-api`: `OpCtx::plan_step` (D4).
- `omuzaki-doc-api`: `Scratch` and the shared in-memory patch application (D4); `omuzaki-doc-mock` switches to it.
- `omuzaki-char-api`: `"applied"`, `"poses"` and `"emotes"` in `CharacterDef::SIBLING_KEYS`, with their paths; the `PartSource` move is a separate change. `omuzaki-doc`'s module docs stop listing `library.lock` as a file outside the tree.
- `omuzaki-io-ops` (implementation, not API): `licence.derive` and `io.check_export` join application records (D8).
- The pose set (`characters/<id>/poses/<name>`) and the emote set (`characters/<id>/emotes/<name>`) come with their own op tasks below. There is no scene node before the ST-01 scene contract (phase 2); the scene target waits for it.

## 6. Mock

`omuzaki-library-mock` is the independent oracle, as `omuzaki-store-mock` is for the store:
- a dozen fixture items, one of each kind and target, over the mock part kinds and the face, palette and expression ops;
- a naive linear search, which the real index must agree with on the fixture set;
- `apply_for_test`, which the seed tests in `omuzaki-char`, `omuzaki-face`, `omuzaki-anim` and `omuzaki-stage` use to dry-run every item on their own mock character without linking `omuzaki-library`;
- `conformance::run(&dyn Library)`: resolve order, version selection, the hash clash refusal, deterministic search order, unavailable items with reasons.

## 7. The task cut

The work is filed under LB-01 unless noted; existing tasks were re-cut against this page (their contract and acceptance sections now point here).

- `OpCtx::plan_step` and `omuzaki_doc_api::Scratch`
- `applied`, `poses` and `emotes` sibling keys and paths in `omuzaki-char-api` (one change, so the later tasks don't conflict)
- `omuzaki-library-api` and `omuzaki-embed`
- `omuzaki-library-mock`: fixtures, `apply_for_test`, `seed::check_source`, conformance
- `omuzaki-library`: registry, index, `library.list`, `library.describe`, the person's library folder
- `library.apply`: scratch planning, records, source rows, slots, bundles, rights note
- `library.search` (D13)
- Library panel and palette group
- `library.install`, `library.uninstall` and `library.lock` (D5, D9, D11)
- `library.update` (three-way merge) and `library.detach`
- `library.render_previews`, the preview cache, `preview` on apply
- seed conformance with goldens
- `cargo xtask lint-seed`
- `licence.derive` and `io.check_export` join application records (D8)
- the seed source in `omuzaki-char`, `omuzaki-face`, `omuzaki-anim` and `omuzaki-stage`: embedding, one `register_library_source!` each, `tests/library_seed.rs`
- `stage.set_light_rig` on the ST-01 scene node (LK-04; phase 2)
- turn on the reserved `scene` target (scene records, light-rig items; phase 2)
- the character's pose set and `pose.set` (RG-06)
- the character's emote set and `emote.set` (SH-07)
- The seed items. Each is blocked by the seed source and by the ops its recipes call. The adapters over existing data are written by the seed task that wraps that data (body shapes and skin tones, light rigs, poses, expressions).

`round-u` is redefined geometrically before it is implemented, and `egg-oval` is re-derived the same way; both block the face-profile seed.

The person's library beyond install (`save_as_item`, `fork`, candidates, accept: LB-02, LB-03), image search (LB-05), team and marketplace (LB-07, LB-08) and the phase 2 and 4 seed sections (LB-14, LB-15) are cut when their phases are planned; this contract already has room for them.

## 8. Decisions taken on review (2026-10-12)

1. **Publishers** are a display name plus an ed25519 key fingerprint, generated locally per person or team and signed on publish (ties into SE-07). First-party items use the Omuzaki publisher key. Account-backed identity comes later with the hosted identity service (VC-12). A name is never trusted without its key (D2). *Per person only, per ADR 0042.*
2. **The first-party licence** is decided: CC0-1.0 (2026-10-12).
3. **Sharing a person's items with teammates** goes only through LB-07 shared libraries under VC-10 roles; there is no ad hoc path (D5, D11).
4. **The scene target** is reserved in the schema and refused until the ST-01 design lands (D1); turning it on is blocked on ST-01. The light-rig seed and `stage.set_light_rig` wait with it.

## 9. Sources

Read on 2026-10-12: spec v3 §6, §7, §28, §39.1, §40; `docs/licensing/policy.md`; ADR 0009, 0011, 0013, 0015, 0016; `docs/design/io-api.md` D10; `crates/omuzaki-{ops,doc,store,char}-api`; `crates/omuzaki-doc/src/lib.rs` (files outside the tree).
