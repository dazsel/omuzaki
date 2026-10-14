# Platform export: limit tables, the fit pipeline and platform profiles

**Status:** proposal. **ADR:** 0047.
**Spec:** v3 §27 (EX-03, EX-04, EX-13, EX-14, EX-15), §10, §6, §7 (variants), §39 (rules 1, 2, 9, 10).
**Builds on:** `docs/design/io-api.md` (D3, D4, D5, D10), ADR 0011 (lib crates), ADR 0015 (link), ADR 0033 (imported
avatars), `vrchat_rank.rs`, the cleanup tool and the fit tools. **Read on 2026-10-13.**

Five features export a character for a platform that sets limits: VRChat PC and Quest (EX-03), Roblox (EX-13), cluster
(EX-14), Resonite (EX-15), and the engine packages (EX-04). This page says where limits, fixes and platforms live, and
how an export is fitted to a limit. Types are named here; their docs live in the code.

## 1. What this covers

| The issue asks | Where |
|---|---|
| Where the code lives, layering, `vrchat_rank.rs` as the first table, `measure.perf` | P1, P3 |
| The profile model: data vs code, ids, adding a platform | P2, P5 |
| The fit pipeline: order, re-evaluation, stop rules, invariants, refusal, determinism, time cap, progress | P6 |
| Data hygiene | P7 |
| Ops and agent rules, rights, the SE-07 hook | P8, P9 |
| Variants; uploading | P10, P11 |
| Cost | P12 |
| Engine packages and animations | P13 |
| Worked example | §4 |

## 2. The shape of it

```
character ──gather──► IoScene ──prepare (profile code, e.g. Roblox rig)──► fit_to(table, tier) ──► [mark] ──► run_export ──► [sign] ──► [package] ──► host writes
 (+ variant)                 │                                                                     SE-07    (preflight unchanged)  SE-07       EX-04
                             └──► stats_from_scene ──► evaluate(table) ──► findings with fit hints   (platform.check)
```

Everything left of `run_export` works on an in-memory `IoScene` the op layer owns; nothing is written to the project.

## 3. Decisions

### P1. The code stays in `omuzaki-io-ops`; no new crate

The limits engine, the fit tools, the planner, the profiles and the `platform.*` ops all live in `omuzaki-io-ops`:

| Module | Holds | Work unit |
|---|---|---|
| `limits/{table,stats,eval}.rs` | `LimitTable`, `AvatarStats`, `stats_from_scene`, `evaluate`, `findings`, `stale` | - |
| `prepare/cleanup.rs` | weld, merge by material, drop unused | - |
| `fit_texture.rs`, `fit_atlas.rs`, `fit_bones.rs`, `fit_decimate.rs`, `fit_meshes.rs` | the fit tools, one per stage | PX3 |
| `fit.rs` | `fit_to`, the planner | - |
| `platform/{mod,profile}.rs`, `platform/profiles/<id>.rs` | `PlatformProfile`, `register_platform_profile!`, one file per platform | PX1 |
| `ops/platform/{list,check,fit}.rs` | `platform.list`, `platform.check`, `platform.fit` | PX2 |
| `data/platforms/<id>.toml` | one limit table and profile per platform id | PX1 |

**Why not a lib crate (ADR 0011).** A lib is for code that *several implementations* share and that is a format. Nothing
outside the op layer needs the engine:

- **Exporters never call it.** The fit runs in the op layer on the scene *before* `run_export`, so a format crate only
  ever sees an already-fitted scene. The rule that exporters cannot depend on each other is not touched.
- **`measure.perf` can depend on `omuzaki-io-ops` directly**, because `omuzaki-stage` is an app (`crates/README.md`);
  it calls `evaluate` with the `vrchat-pc` and `vrchat-quest` tables.
- **The fit tools are behaviour**, and the profiles must self-register with `inventory`, which ADR 0011 rules out for a
  lib ("a lib that needs to register something ... is not a lib").

If a second implementation crate ever needs `evaluate` (a plugin host, say), `limits/` moves to a lib unchanged: it is
pure and takes only `IoScene` and its own types. That is the extraction test, written down so nobody pre-empts it.

**Why not a trait in `omuzaki-io-api`.** A profile is op-layer policy over a format, not a format. Keeping
`PlatformProfile` in `omuzaki-io-ops` costs no API change and no hub-crate review. The one cross-crate extension point,
the package wrapper, is EX-04's to design (P13).

### P2. A platform is one data file and one registration

**Data** (`crates/omuzaki-io-ops/data/platforms/<id>.toml`), everything a person could check against the platform's page
or would tune without code:

```toml
[limits]                       # the platform's published numbers; nothing else goes here
platform = "VRChat (Android)"
version = "Avatar Performance Ranking, page dated 2026-04-21"
source = "https://creators.vrchat.com/avatars/avatar-performance-ranking-system/"
retrieved = "2026-10-12"
tiers = ["Excellent", "Good", "Medium", "Poor"]   # best first
beyond = "Very Poor"           # the name past the last tier (a hard-limit platform: tiers = ["accept"], beyond = "refused")
texture = { opaque_bits = 3.56, alpha_bits = 3.56, mips = true }   # how this platform's texture memory is estimated
unsupported_note = "disabled on mobile; the platform strips them"

[limits.stats]                 # one row per stat: a value per tier, "-" for unlimited, "unsupported" for stripped
triangles = [7500, 10000, 15000, 20000]
bounds = [[2.5, 2.5, 2.5], [4, 4, 4], [5, 6, 5], [5, 6, 5]]
lights = "unsupported"
# ... every row of today's [android] table

[profile]                      # our policy for this platform
name = "VRChat Quest"
format = "vrm0"                # the exporter id it writes through (see io.list_formats)
default_tier = "Poor"
strip = ["lights", "cloths", "physics_colliders", "physics_rigidbodies", "audio_sources"]
texture_max_side = 2048        # per texture, before atlasing
atlas_max_side = 4096
decimate_margin = 0.01         # aim 1% under the triangle limit
# package = "…"               # reserved for EX-04; refused until a packager registry exists
```

**Code** (`src/platform/profiles/<id>.rs`), only what data cannot say:

```rust
/// One platform. `DATA` is its table and profile; `prepare` reshapes the scene before fitting (a rig the platform
/// requires, attachment nodes); the default does nothing.
pub trait PlatformProfile: Send + Sync + 'static {
    const ID: &'static str;                       // "vrchat-quest"; must equal the file stem
    const DATA: &'static str;                     // include_str!("../../../data/platforms/vrchat-quest.toml")
    fn prepare(&self, _scene: &mut IoScene, _report: &mut Report) -> Result<(), IoError> { Ok(()) }
}
register_platform_profile!(VrchatQuest);           // inventory, like register_exporter!
pub fn platform_profiles() -> Vec<Profile>;          // parsed, sorted by id
pub fn find_platform_profile(id: &str) -> Option<Profile>;
```

A data-only platform (cluster, Resonite, both VRChat ones) is a six-line file: the struct, the two constants and the
macro. Roblox adds `prepare` (R15 rig names, attachment nodes).

**One table per id.** A profile holds exactly one limit table. A platform that publishes several tables for different
kinds of upload gets one id per table: Roblox's body, accessory and layered-clothing rules are `roblox-body`,
`roblox-accessory` and `roblox-layered`, three data files and three registrations that share one `prepare` (a
`profiles/roblox/` module the three call, switching on the asset kind). A person picks the id for what they upload;
`platform.list` groups ids by their `[limits].platform` name. A test parses every registered profile, checks its
`format` names a registered exporter (when that crate is linked), its `strip` keys are stats, `default_tier` is a tier,
and the file stem is the id, so a broken data file fails CI, never a person's export.

**Ids:** `vrchat-pc`, `vrchat-quest`, `roblox-body`, `roblox-accessory`, `roblox-layered`, `cluster`, `resonite`. EX-04's engine packages reserve `unity`,
`godot`, `unreal` and `vrchat-unity`; their design decides whether they are profiles with a package (P13). A
lower-case slug, like format ids.

### P3. Stats: counts, generalised, and how VRChat becomes the first table

- `AvatarStats` moves from `vrchat_rank.rs` to `limits/stats.rs` with its fields unchanged, and gains the counts the
  other platforms cap: `vertices`, `morph_targets` (distinct targets over all meshes), `textures`, `max_texture_side`,
  `meshes`, and `attachments` (named attachment nodes; Roblox). It stays a typed struct; `AvatarStats::get(key)` reads
  a stat by its data-file key, and `LimitTable::parse` refuses a key `get` does not know, naming it. A new stat is one
  field, one `get` arm and its counting code, in one file, rarely.
- `stats_from_scene` is `stats_from_ioscene` plus the new counts. Texture memory is no longer a field: raw pixel
  counts are, and `[limits].texture` turns them into MiB per table, so PC and Android keep exactly the same estimates.
- `vrchat_rank.rs` becomes a thin wrapper: `Platform::Pc` and `Android` map to `vrchat-pc` and `vrchat-quest`, `Rank`
  maps to tier indices (Very Poor is `None`), and `stats_from_ioscene`, `rank()` and `texture_mib` keep their signatures,
  so all 22 of its existing tests pass unchanged (the acceptance check). The old data file is deleted once a test shows every
  number in the new files.

### P4. Evaluation and findings

`evaluate` gives each stat the best tier it fits (`Some(i)`), `None` past the last tier, or `Unsupported`; the avatar's
tier is the worst supported stat, as `vrchat_rank.rs` does. `findings(eval, table, target)` emits one finding per stat over the
target, rule id `limits.<platform-id>.<stat>`, located at the stat, message "triangles 120,000, over Poor (20,000)", hint
the fit stage that would help (`fit.decimate`, `fit.atlas`, `fit.meshes`, `fit.bones`, `fit.textures`, `fit.strip`) or,
for a stat no stage changes (springs, constraints), what the person changes by hand. An unsupported stat with a value
is an info finding using the table's `unsupported_note`. A table may mark tiers as advisory (`advisory = ["recommended"]`,
Resonite's recommendations): a stat past an advisory tier but within the target is a warning, never an error.

### P5. Adding a platform

1. A research-style task reads the platform's published rules and writes `data/platforms/<id>.toml` with `source`,
   `retrieved` and `version` (P7).
2. One registration file and one `mod` line in `platform/profiles/mod.rs` (the ADR 0015 pattern).
3. Platform code, if any, goes in that file's `prepare` or a folder beside it.

Nothing else changes: the ops, the planner and `io.export` read the registry.

### P6. The fit pipeline

`fit_to(scene, table, tier, options, progress) -> Result<FitReport, IoError>` runs a fixed order of stages.
Each stage runs only when a stat it affects is over the target tier, and the planner re-evaluates after every stage:

| # | Stage | Tool | Runs when over | Changes | Lossy |
|---|---|---|---|---|---|
| 1 | `fit.strip` | in `fit.rs` | any `strip` stat is non-zero | removes components the platform strips anyway | no (the platform drops them) |
| 2 | `fit.drop` | `cleanup`: weld, drop unused | any mesh, material or texture stat | duplicate vertices, unused materials, textures, images | no |
| 3 | `fit.meshes` | `merge_meshes` (PX3) | `skinned_meshes`, `basic_meshes`, `meshes` | joins mesh nodes that no expression binds and whose skins agree (below) into one mesh; their nodes stay, meshless | no |
| 4 | `fit.textures` | `downscale_to` | `texture_mib`, `max_texture_side`, or slots over (sizing for 5) | halves sides until each texture fits `texture_max_side` and every atlas group packs | yes |
| 5 | `fit.atlas` | `build_atlas`, then merge by material | `material_slots`, `textures` | one material per settings group, UVs remapped | slightly (filtering at seams) |
| 4' | `fit.textures` | as 4, on the atlases | `texture_mib` still over | halves the atlases | yes |
| 6 | `fit.bones` | `reduce_bones` | `bones` | merges unprotected joints into ancestors | yes (deformation detail) |
| 7 | `fit.decimate` | `decimate_scene` | `triangles`, `vertices` | triangles, aiming `decimate_margin` under the limit | yes |

**Why this order.** Lossless before lossy, and cheap before expensive. Strip and drop first, so nothing is spent on what
the platform discards. Meshes before atlasing, because slots count per mesh node. Textures before the atlas, because 24
full-size textures cannot share an atlas: stage 4 picks the largest power-of-two scale at which every settings group
packs into one atlas of `atlas_max_side` (it asks `omuzaki_core::pack` and stops at the first scale that places
all), and stage 4' only runs if memory is still over afterwards. That is the one stage allowed twice, and only in that
position; nothing loops. Bones before decimation, because decimation blends weights and fewer joints give it fewer to
blend. Decimation last: it is the costliest and most visible, so it runs on what is left.

**Which meshes may join (`fit.meshes`).** Two skins can list the same joint with different inverse bind matrices (each
mesh keeps its file's matrices, ADR 0033), and a joined skin has one matrix per joint, so picking one would move the
other mesh's vertices. Meshes join only when their skins are compatible: every joint they share has the same inverse
bind matrix within 1e-6. Incompatible meshes stay apart, the report names the pair and the first differing joint, and
if that leaves the mesh count over, it is a refusal floor (below).

**Basic meshes move with their node**, not with joints, so joining two of them is exact only if they share the same
motion. Each mesh's vertices are baked into the target node's space. That baked transform must stay correct in every
pose, so a basic mesh joins another only when:
- the two nodes have the same parent, and
- neither node's own transform can change at run time: no humanoid bone, spring joint, collider or look-at node, no
  constraint source or destination, and nothing animated.

A head-attached and a hips-attached accessory therefore stay apart, and a mesh count still over the limit is a floor
like any other mismatch. Skinned meshes have no such limit: their vertices follow joints, not their node. PX3's
acceptance adds two cases:
- two basic meshes under the same parent are joined, and match in world position within 1e-6 after the parent is
  rotated 90°;
- meshes under the head and under the hips are not joined.

**First-person visibility must match too.** VRM's first-person annotation (`Auto`, `Both`, `ThirdPersonOnly`,
`FirstPersonOnly`) is per mesh node, so joining a `ThirdPersonOnly` hair mesh into a `Both` outfit would make the hair
visible to the wearer. Meshes join only when their annotations are equal, counting an unannotated mesh as `Auto`. The
joined mesh keeps that shared flag on the target node. The emptied nodes' annotations are removed, because they have no
mesh left to annotate. So the first-person data still says, mesh by mesh, exactly what it said before. A difference
keeps the meshes apart and can be a floor, like an incompatible skin. PX3 adds a test where `ThirdPersonOnly` Hair and
`Both` Outfit stay apart, and two `Both` meshes join and export to VRM 0.x with the annotation on the target node.

**Never changed.** The humanoid map, spring chains, colliders and their joints, constraints, look-at, first person,
expressions and their binds, node count and order, the rights fields of `scene.meta` (licence, authors, every
permission and URL: what the file states) and `scene.provenance`. "Unchanged" is by identity, not by arena index:
`fit.drop` and `fit.atlas` compact the material, texture and image arenas, so an index may be renumbered as long as it
still points at the same content.

**Protected images.** `fit.textures` never resamples an image that something identifies by content: the thumbnail, and
any texture of a material named by an expression colour or UV bind (the atlas tool already skips those materials when
atlasing). Their memory still counts toward `texture_mib`. If the protected images alone are over the target, that is a
refusal floor naming each one: "the eye texture (4096², bound by the blink UV bind) alone is 11.9 MiB, over …". The
re-cut of the planner adds an oversized bound texture: it stays byte-identical while the others shrink, and a table it alone
exceeds is refused before any lossy stage.

What "unchanged" means, item by item:

- **Rights and provenance:** compared field by field, exactly. Any difference is `Internal`.
- **The thumbnail:** `meta.thumbnail_image` may change index, but must name an image whose bytes are identical before
  and after (fit never resamples it: the atlas skips it, `fit.textures` leaves it alone).
- **Expression binds:** each morph bind names the same node and the same morph target (index and name; the decimator keeps
  target order). Each colour or UV bind names a material whose content (factors, settings and the bytes of its
  textures) is identical before and after; the atlas tool skips bound materials, so they are never atlased.

The tools enforce their own part (protected joints, skipped bound materials, morph indices in
the decimator); `fit_to` checks all of the above before returning and fails `Internal` on any difference. The planner re-cut adds
an acceptance case where `fit.drop` drops an image ahead of the thumbnail and the remapped thumbnail passes.

**Refusal.** Before any lossy stage, `fit_to` checks the floors no stage can lower: stats only a person changes (spring
counts, constraints, bounds), protected joints over the bone budget, bound materials over the slot budget, protected images over the texture
budget, and meshes that cannot join (skin or first-person mismatch) over the mesh budget. If a floor
is over the target, it returns `IoError::Rejected` at once, with one finding per item: "the protected bones alone are
100, over Good's 90: remove helper bones in the source or target Poor". After the last stage, any stat still over is a
`Rejected` with the same shape. A scene already inside the target returns unchanged with no stages.

**Determinism and time.** Every tool is deterministic (their tasks test byte-identical reruns), and the planner's
choices depend only on the stats, so the same scene, table and tier give byte-identical output on every machine. A
time cap (`FitOptions::time_cap`, default 120 s) never cuts a stage short: when it passes, `fit_to` returns
`IoError::Limit` and no result, so a slow machine can fail where a fast one succeeds, but never produces a different
file. Target on a CI machine: 30 s for the §4 avatar.

**Progress and cancel.** Each stage reports `checkpoint("fit.<stage>", fraction)` through `Progress` (io-api). The
planner calls the tools one item at a time (one image for `fit.textures`, one settings group for `fit.atlas`, one
primitive for `fit.decimate`, using the per-primitive `decimate` with the shares `decimate_scene` would compute) and
checks between items. So:

- **Between stages and between items**, a cancel lands within 50 ms.
- **Until PX11, inside one item**, a cancel lands when that item finishes. The tools as first written
  take no `Progress`, and changing them now would re-open work in flight. The end-to-end bound is the longest single
  item: one primitive's decimation, at most 5 s for the 100,000-triangle case (the decimator's bench). The planner's test checks this bound
  with a fake `Progress` that cancels in the middle of `fit.decimate`.
- **PX11 adds `Progress` to the three heaviest tools:** `resize` (every 64 output rows), `build_atlas`
  (every 4,096 pixel rows) and `decimate` (every 4,096 collapses), with tests that cancel in the middle of each.
  `cleanup`, `merge_meshes` and `reduce_bones` stay without `Progress`, so after PX11 the end-to-end bound is the
  longest of them: `cleanup`'s weld, capped at 500 ms for the sizing case (P12). PX9's bench checks those caps, and
  the planner's test tightens to 500 ms once PX11 lands. 50 ms holds between stages and items, not inside these three.

**Atomicity.** On an error the scene is partly fitted. Callers always fit a scene they own (the op layer gathers a fresh
one per call); `fit_to` documents that a failed scene is to be dropped.

### P7. Data hygiene

- `source` (a URL), `retrieved` (`YYYY-MM-DD`) and `version` (the platform's own version or page date) are required;
  parse refuses a table without one, naming the field.
- **Staleness: 90 days, checked at release, not in PR CI.** A calendar test in PR CI would turn red on its own and
  block unrelated PRs. `stale(table, today, 90)` is unit-tested with fixed dates; an `#[ignore]` test reads
  `OMUZAKI_TODAY` and lists stale tables, and the release checklist runs it (PX8). `platform.list` also
  shows `stale: true` so a person sees it in the app.
- **Who updates a table:** a research-style task that cites the page, the date and what
  changed, and attaches the comparison. Two independent reads must agree, as the VRChat read did.
- **Unverified numbers never enter a table.** If a page cannot be read, the row is left out and the gap is filed; a
  guess is never written with a comment.

### P8. Ops and agent rules

| Op | Kind | Scope | Does |
|---|---|---|---|
| `platform.list {}` | query | `read` | every profile: id, name, format, tiers, default tier, `retrieved`, `stale` |
| `platform.check {character, profile, tier?}` | query | `read` | gather, `prepare` on the copy, stats, `evaluate`, findings with fit hints and refusal floors; nothing written |
| `platform.fit {character, profile, tier?}` | job | `read` | the dry run: `fit_to` on a gathered copy; per stage the before and after stats and what changed; nothing written |
| `io.check_export {…, profile?}` | query | `read` | as today; with `profile`, the format comes from it and `platform.check`'s findings are merged in (over-limit is a warning when a stage can fix it, an error at a floor) |
| `io.export {…, profile?, tier?}` | job | `publish` + `fs` | gather, bake, `prepare`, `fit_to`, [mark], `run_export`, [sign], [package], the host writes; the report holds the stages and "what to do next" (P11) |

- `platform.fit` is a job rather than the query the issue suggested: it can take seconds, so it needs the job
  manager's progress and cancel (io-api Q6). It writes nothing and needs only `read`.
- `format` and `profile` on the same call must agree, or the op is `Invalid` naming both; `tier` defaults to the
  profile's `default_tier`; an unknown profile is `NotFound` with the list as hint.
- An agent has no `publish` grant by default, so `io.export` with any profile is `needs_person` for it, exactly as
  today; checks and dry runs are free.

### P9. Rights and signing

The licence gate and the preflight apply unchanged, because the fitted scene still goes through `run_export`: an
imported model whose terms forbid redistribution or modification is refused for `Publish` (io-api D5), the stored
record is checked against the parts (D10), and `meta` is never changed by the fit (P6). A fitted export is a
modification, so `rights.licence_forbids_modification` is exactly the right rule; no new rule is needed.

**SE-07's hooks.** Two positions, both in `io.export` for every profile:
- **The mark** (ADR 0042 D9) is applied to the scene *after* `fit_to` and *before* `run_export`, so the fit never
  resamples, atlases or downscales a marked texture, and the mark the person ships is the one SE-07 wrote.
- **The signature**, if SE-07 signs files, sits after `run_export` and before the package step, so a package wraps an
  already-signed model.

What they embed and which key signs is SE-07's design (identity and trust); this design fixes only
the positions.

### P10. Variants

`characters/<id>/variants/<name>.toml` (spec §7, "VRChat Quest") selects parts and materials: drop the ribbon, swap the
hair for a lighter part, a flat material for the cloth. It is applied when the character is built, before gather. It
may also name the profile and tier it is meant for (`export = { profile = "vrchat-quest", tier = "Poor" }`).

The fit is export-time only and reduces what the variant leaves. **Why not store the fit:** it is lossy, it depends on
the table's version, it is reproducible from the inputs in seconds, and stored output would go stale when the table or
the character changes, duplicating assets in the store. A variant is a person's design decision; a fit is arithmetic.

**A variant may not:** change the licence record or provenance; add a part whose source the licence gate does not
cover; carry fitted numbers (no "triangles = 20000"; that is the profile's job); change the humanoid map, springs or
expressions' binds (it can drop a part that holds an expression, which the existing part rules report).

### P11. Uploading

Omuzaki writes files and says what the person does next, in the export report: for VRChat, "open the Unity project with
the VRChat SDK, import the file with UniVRM, upload from the SDK panel"; for each other platform, its own steps,
stored in the profile's data as `next_steps` (localised through the op-text tables, ADR 0023). An upload op
(`platform.upload`, scopes `publish` and `net`, always `needs_person`) waits on the maintainers' platform accounts and a maintainer decision; nothing here assumes it.

### P12. Cost

For the issue's sizing case, 100,000 triangles, 52 morph targets, 24 materials (about 55,000 vertices): targets per
stage on the cloud box, `reported` now and gated on `Dev` once PX9's bench exists. The fixtures are built in code.

| Stage | Work | Time cap | Memory cap |
|---|---|---|---|
| stats | image headers; one pass over vertices with skinning for bounds | 50 ms | 16 MiB |
| `fit.strip` | flags | 10 ms | none extra |
| `fit.drop` | weld over a spatial hash with 52 sparse delta checks | 500 ms | 150 MiB |
| `fit.meshes` | concatenate vertex arrays, join skins | 200 ms | 2x the meshes, about 40 MiB |
| `fit.textures` | decode, resize in linear `f32`, re-encode, one image at a time | 4 s | 200 MiB (a 2048² `f32` buffer is 64 MiB) |
| `fit.atlas` | pack, blit, encode one atlas per texture slot | 4 s | 300 MiB (a 4096² RGBA8 atlas is 64 MiB) |
| `fit.bones` | weight transfer over vertices | 100 ms | 20 MiB |
| `fit.decimate` | quadric collapse with morph attributes, 100k to 20k | 5 s | 400 MiB |
| whole job | | 30 s | 1 GiB peak |

### P13. Engine packages and animations

- **Engine packages (EX-04)** are profiles with a `package`: a step that takes the model files `run_export` wrote and
  adds the engine's files (a prefab, materials, an animator, an expression controller). The packager contract, its
  registry and whether it is an API crate are the next design's; this one reserves the `package` field and the ids.
  VRChat's flavour is the `vrchat-unity` package around the `vrchat-pc` or `vrchat-quest` fit.
- **Animations** go through their own registry (`AnimationExporter`, `run_export_animation`), never through a
  profile's fit: clips have no platform limits here. Humanoid tracks (VRMA) are unaffected by `fit.bones`, which never
  merges a humanoid bone. A package may include animations by calling `run_export_animation` beside the model.
- **`Exporter` and `IoScene` stay as they are.** A profile is not an exporter: it reshapes and fits an `IoScene` and
  then names one.

## 4. Worked example: the Quest fit

A synthetic avatar, built in code: 120,000 triangles over three skinned meshes (Body with the 52 ARKit morph targets
and no UV binds, Hair, Outfit), 24 material slots (Body 8, Hair 6, Outfit 10), each with one texture: seven at
2048x2048 and seventeen at 1024x1024, 180 MiB as decoded RGBA8; four of the 1024 ones (Body's lashes, three hair cards)
have alpha. 210 bones: 52 humanoid, 48 spring joints in 6 chains, 110 helpers. 4 colliders, 64 collision checks,
bounds 1.5 x 1.6 x 0.4 m, one animator.

`rank()`, run on a CI machine on 2026-10-13 (scratch test, not committed), before and after the expected fit:

```
before Pc: rank Very Poor (texture 32.67 MiB); at that rank: triangles
before Android: rank Very Poor (texture 26.70 MiB); at that rank: triangles, skinned_meshes, material_slots, bones
after  Pc: rank Good (texture 16.00 MiB); at that rank: skinned_meshes, physbones_components, physbones_transforms, physbones_collision_checks, bones
after  Android: rank Poor (texture 11.87 MiB); at that rank: triangles, material_slots, physbones_transforms, physbones_collision_checks
test result: ok. 1 passed; 0 failed
```

"After" is the stats row of the table below (19,800 triangles, 2 skinned meshes, 4 slots, a 4096² opaque and a 2048²
alpha atlas, 150 bones), everything else as before.

**The plan for `vrchat-quest` at its default tier, Poor** (table `[android]`, column Poor). Targets are the table's;
the "after" numbers are arithmetic, to be checked by the planner's test, not measured:

| Stage | Triangles | Skinned meshes | Slots | Textures (Android est.) | Bones | Notes |
|---|---|---|---|---|---|---|
| limit (Poor) | 20,000 | 2 | 4 | 40 MiB | 150 | |
| before | 120,000 | 3 | 24 | 26.70 MiB | 210 | Very Poor |
| `fit.strip` | | | | | | skipped: nothing the Quest strips is present |
| `fit.drop` | 120,000 | 3 | 24 | 26.70 | 210 | the synthetic mesh has nothing to weld or drop |
| `fit.meshes` | 120,000 | **2** | 24 | 26.70 | 210 | Hair and Outfit join (no expression binds them); Body keeps its 52 targets |
| `fit.textures` | | | | **6.67** | | scale 1/2: 7 at 1024, 17 at 512, so the 20 opaque ones pack into one 4096 atlas |
| `fit.atlas` | 120,000 | 2 | **4** | **11.87** | 210 | 2 materials (opaque 4096², alpha 2048²); each mesh uses both |
| `fit.textures` (4') | | | | | | skipped: 11.87 is under 40 |
| `fit.bones` | 120,000 | 2 | 4 | 11.87 | **150** | 60 of 110 helpers merged; 100 protected |
| `fit.decimate` | **19,800** | 2 | 4 | 11.87 | 150 | 1% margin under 20,000 |
| after | 19,800 | 2 | 4 | 11.87 | 150 | **Poor** |

Texture arithmetic: Android is 3.56 bits per pixel with mips (x4/3). Before: 47,185,920 px, 26.70 MiB. After stage 4:
11,796,480 px, 6.67 MiB. After the atlas: 4096² + 2048² = 20,971,520 px, 11.87 MiB (bins round up to powers of two:
the opaque bin is 64% full, and the four 512² alpha textures with 4 px gutters need 1040², so their bin is 2048²). Packing check with 4 px gutters: the seven 1032² cells take a 3 by 3 grid of 3096², the right strip
holds 7 of the 520² cells, the bottom strip 5 and the two empty grid cells 1 each: 14 places for 13.

**Other tiers.** Medium fails at the floors before any lossy stage: 48 PhysBones transforms is over Medium's 32 and
`fit` never changes springs, so `platform.check` says "remove spring joints or target Poor". Good also fails on the
100 protected bones (Good allows 90). On PC the same avatar is Very Poor only on triangles; `vrchat-pc` at Good (70,000
triangles, 2 skinned meshes, 8 slots, 150 bones) runs `fit.meshes` (3 to 2), `fit.textures` only as atlas sizing
(32.67 MiB is already Excellent), `fit.atlas` (24 to 4 slots), `fit.bones` (210 to 150) and `fit.decimate` to 69,300.

## 5. Seams with the other Phase 5 designs

Named here, decided there:

| Design | Seam | This side |
|---|---|---|
| identity and trust (0042) | which key signs an export | the `sign` step's position (P9) |
| team roles and audit (0043) | an export is a `publish` op | no new scope; whatever 0043 audits for `publish` covers it |
| shared library sources (0046) | library items' terms | reach the record through `licence.derive`; nothing new here |
| git (0045) | where exports go | never into the project tree; variants are project files |
| comments (0044) | none | |

## 6. Task cut

Already filed and kept: the packer, resize, decimate, bones and atlas tools and the cleanup. The platform data and
research tasks are filed: Roblox (research and tables), cluster and Resonite; SE-07 has its hooks. Re-cut by this
design: the limits data and evaluation task (the data path becomes `data/platforms/<id>.toml` with a `[limits]`
section; `AvatarStats` gains P3's counts; tiers gain `beyond`; hints include `fit.meshes`) and the planner (adds
`fit.meshes` and stage 4', the floor check, `time_cap`, `decimate_margin`). New: PX1 to PX4, PX8, PX9 and PX11.

## 7. Sources

- VRChat, Avatar Performance Ranking System, https://creators.vrchat.com/avatars/avatar-performance-ranking-system/,
  page dated 2026-04-21, read 2026-10-12 (`data/vrchat-performance.toml`). Re-read on 2026-10-13: unverified
  (unreachable from the build box).
- Roblox, cluster and Resonite published avatar rules: unverified (unreachable from the build box); the research tasks
  read them. No number from them appears here.
