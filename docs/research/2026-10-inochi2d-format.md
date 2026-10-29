---
title: "The Inochi2D puppet format, for our importer (2026-10-29)"
summary: "How Inochi2D stores a puppet (the INP1 container and JSON payload, the INP2 tagged container), what its nodes, parameters, bindings, masks, blend modes and physics mean, which conventions our importer (`omuzaki-inochi2d`) follows and why, the existing code and its licences, and the full-scope checklist; read from the primary sources at pinned commits."
area: research
---

# The Inochi2D puppet format, for our importer (2026-10-29)

Retrieved 2026-10-29. Everything below was read from the primary sources listed at the end, at the commits pinned
there; the line references are to those commits. It is written in our own words: no documentation text is copied, and
the importer (`crates/omuzaki-inochi2d`) is our own code, written from the format, not ported from either
implementation.

**Status:** INP1 (Inochi2D 0.8, the format real puppets use today) imports into `omuzaki-puppet-api`'s `PuppetDef`
with an import report, and writes back out for our own puppets. INP2 (Inochi2D 0.9, unreleased) reads as a container
only; its payload is refused as unsupported (see "INP2").

**Sample check:** the project's two public example puppets ("Aka" and "Midori", CC-BY-4.0, from the official
example-models repository's LFS URLs, sha256 matching its LFS pointers) were downloaded into a scratch folder,
imported, checked against the contract and written back out and read again. No sample kept: nothing from them is in
the repository, the tests or this note beyond the counts here.

| | Aka | Midori |
|---|---|---|
| nodes | 100: 76 parts, 3 composites, 19 physics | 126: 81 parts, 3 composites, 8 mesh groups, 20 physics |
| parameters / bindings / pendulums | 34 / 259 / 19 | 35 / 416 / 20 |
| textures | 76, TGA | 81, TGA |
| refusals | none | none |
| findings | canvas, metadata notes, rights not stated, 76 z offsets | the same, plus 24 z offsets, 12 mesh-group notes (flags, and parts reached only through plain nodes) and 2 vendor entries kept |
| import time (debug build) | 2.2 s for 17.7 MB | 2.3 s for 20.6 MB |

Neither puppet has a colour channel above its alpha in any pixel (22.8 and 20.5 million pixels). This confirms that
stored textures are premultiplied.

The test that does this (`tests/samples.rs`) is ignored by default and reads its paths from `INOCHI2D_SAMPLES`.

## The INP1 container

INP1 is what Inochi2D 0.8 writes (`.inp` for a finished puppet, `.inx` for Inochi Creator's project file, which is the
same container). All integers are big-endian. In order:

1. The 8-byte magic `TRNSRTS\0`.
2. A `u32` payload length, then that many bytes of UTF-8 JSON: the puppet.
3. `TEX_SECT`: a `u32` texture count, then per texture a `u32` byte length, a `u8` encoding (0 PNG, 1 TGA, 2 BC7)
   and the encoded bytes. Parts name textures by their index here.
4. Optionally `EXT_SECT`: a `u32` entry count, then per entry a `u32` name length, the name (a vendor key), a `u32`
   data length and the data. Inochi2D reads it only when at least 8 bytes remain.

Reference loader: 0.8.7 `fmt/package.d`. Details that matter for a reader:

- Inochi2D 0.8 writes every texture as TGA (encoding 1) and decodes by content, not by the declared code.
- A slot with zero bytes is legal and means an empty texture.
- There are no checksums or offsets, so every length must be checked against the bytes left. A hostile file can
  declare a 4 GiB payload in a 100-byte file.

**Our reader** (`container.rs`):

- Checks every declared length and count against the bytes left and against `Limits` before it allocates anything.
  A `u32::MAX` length is a `Limit` error, measured to cost no allocation over 1 MiB (`tests/allocation.rs`).
- Keeps vendor data and any unknown trailing section as borrowed bytes, and reports each.
- Refuses a repeated section.

## The payload (JSON)

The top level has these keys:

- `meta`;
- `physics` (`pixelsPerMeter`, `gravity`);
- `nodes` (the root node, with its tree nested in `children`);
- `param` (the parameters);
- `automation` and `animations`, which we don't model.

Source: 0.8.7 `core/puppet.d`.

### Nodes

Every node has these keys:

- `uuid` (a `u32`; parameters share the same uuid space);
- `name`, `type`, `enabled`, `zsort`;
- `transform` (`trans` x/y/z, `rot` Euler x/y/z in radians, `scale` x/y);
- `lockToRoot`;
- `children`.

Source: `core/nodes/package.d`. A node of an unregistered type is skipped together with its whole subtree (lines
816-829).

| Type | Version | What it is | Our mapping |
|---|---|---|---|
| `Node` | all | a transform | `Group` |
| `Part` | all | a textured mesh | `Part` |
| `Mask` | 0.7 only | a mesh drawn only into the stencil (`core/nodes/mask/package.d` at v0.7.2) | `Mask` |
| `Composite` | all | its children drawn into an offscreen layer, then blended as one | `Composite` |
| `MeshGroup` | 0.8 | a triangle mesh that deforms the drawables under it | `MeshDeformer` |
| `SimplePhysics` | 0.8 | a pendulum that writes a parameter | `Group` plus a `Pendulum` anchored on it |
| `Shapes`, `Tmp` | 0.8 | an experimental node and an editor placeholder, registered in 0.8.7 (`core/nodes/shapes/package.d`, `core/nodes/package.d`), storing only a plain node's fields | `Group` |
| anything else | | (0.8.7 has an `AnimatedPart` class but never registers it; `LatticeDeformer` exists only on the unreleased `v0_8` branch) | `Unknown` with its type name, plus a finding |

Key details for each type:

- **Part** (`core/nodes/part/package.d`).
  - `mesh`: `verts`, `uvs`, `indices`, `origin`, all flat lists (`core/meshdata.d:155-188`).
  - `textures`: three slot ids, `uint.max` meaning none. The first valid id is the albedo; emission and bump maps
    follow (lines 481-493).
  - Drawing fields: `blend_mode`, `tint` (multiplied), `screenTint` (screened), `emissionStrength`, `mask_threshold`,
    `opacity`.
  - `masks`: a list of `{source: uuid, mode: Mask | DodgeMask}`; the older `masked_by` plus `mask_mode` are still read
    (lines 514-524).
  - A mesh draws only when its index count is a nonzero multiple of 3.
- **Composite** (`core/nodes/composite/package.d`): `blend_mode`, `tint`, `screenTint`, `mask_threshold`, `opacity`,
  `masks`, `propagate_meshgroup`. The class default for `propagate_meshgroup` is true, but a file without the key
  reads false.
- **MeshGroup** (`core/nodes/meshgroup/package.d`).
  - Keys: `mesh`, `dynamic_deformation`, `translate_children`.
  - It moves each point of a drawable under it by the affine map of the group triangle that holds the point, rest to
    deformed. Points outside every triangle stay put.
  - It reaches through drawables and through composites that propagate, but not through a plain node: a part under a
    plain node under the group is not deformed unless `translate_children` is set.
  - Its own `origin` takes no part in this.
  - `dynamic_deformation` chooses when the map runs:
    - before the child's own deformation (false), or after it (true);
    - Inochi Creator's `.inp` export bakes non-dynamic groups into the children's deform and translation bindings
      (`Puppet.applyDeformToChildren`, called from Creator v0.8.6 `io/inpexport.d:308`).
  - We report both flags; our deformer applies its map after the child's own deformation.

### Transforms and axes

- **Space.** Model space is y-down pixels. The camera is `orthographic(0, w, h, 0, ..)` (`math/camera.d:68`), and a
  part made from a texture puts vertex `(-w/2, -h/2)` at uv `(0, 0)` (`core/nodes/part/package.d:159-170`). This is
  the same convention as `omuzaki-puppet-api`, so positions and uvs pass through unchanged.
- **Local transform.** A node's local matrix is `T * R(x, y, z) * S` (`math/transform.d:94-99`). Inochi2D 0.9 spells
  the order out as `T * Rz * Ry * Rx * S` (`core/math/transform.d`). Points have z = 0, so the in-plane part is
  `Rz * [[cos y, sin x sin y], [0, cos x]] * S`.
  - With either x or y zero, that is exactly `Rz * diag(cos y, cos x) * S`. We fold x and y into the scale that way,
    and our evaluator does the same for bound angles.
  - With both nonzero, the shear term is dropped (`inochi2d.rotation_xy`).
  - inox2d composes in a different order (`EulerRot::XYZ`); we follow the D reference.
- **Global transform.** The global transform is the parent's times the local (`core/nodes/package.d:360-388`).
  `lockToRoot` replaces the parent chain with the root's transform; the contract has no field for it, so it is
  reported (`inochi2d.lock_to_root`).
- **z translation.** It doesn't change the picture: draw order comes from `zsort`, the camera is orthographic with a
  depth range far larger than the offsets used, and drawing runs without a depth test. Inochi Creator gives every part
  a z of -1 or -2. It is dropped with a note.
- **Part vertices.** A part draws at `vertex - origin + deform` in its node's space (`shaders/basic/basic.vert`, with
  the offset uniform set from `origin`). We subtract `origin` once at import.

### Draw order, blending and masks

- **Draw order.** Each drawable's z is its `zsort` plus all its ancestors' (`core/nodes/package.d:273-289`). The
  puppet stable-sorts its root drawables (parts and composites, not the parts inside a composite) by z, descending
  (`core/puppet.d:293-297, 505-512`): a larger z is drawn first, further back. A composite sorts its own children the
  same way. `PuppetDef` uses the same rule.
- **Blend modes.** The 0.8.7 enum, in order (`core/nodes/common.d:151-213`): Normal, Multiply, Screen, Overlay,
  Darken, Lighten, ColorDodge, LinearDodge, AddGlow, ColorBurn, HardLight, SoftLight, Difference, Exclusion,
  Subtract, Inverse, DestinationIn, ClipToLower, SliceFromLower. Names are written as text; older files may hold the
  position. We map:
  - all but three by name;
  - AddGlow → LinearDodge (it also adds to alpha);
  - Inverse and DestinationIn → Normal.
  Each approximation is a finding. The formulas are in the docs' blending page (`spec/payload/blending.rst`) and in
  the shaders.
- **Textures.** Pixels are stored with premultiplied alpha:
  - Inochi Creator premultiplies on import (v0.8.6 `io/psd.d:204`, `io/package.d:184`, `package.d:335`);
  - 0.8 uploads the pixels as they are and blends with `ONE, ONE_MINUS_SRC_ALPHA`;
  - 0.9 loads with its no-premultiply flag.
  We keep the pixels as stored and never premultiply twice. The sample check above confirms it.
- **Masks.** A drawable with masks draws only where a `Mask` source covers and no `DodgeMask` source does. Coverage
  is a source's alpha above `mask_threshold`, through the stencil. Inochi2D drops a mask whose source uuid is
  missing; we refuse such a file (`inochi2d.mask_source`), because the file is broken. A source the contract can't
  mask with (a composite, a mesh group, the node itself) is dropped with a finding.

### Parameters and bindings

**Parameters** (`core/param/package.d`).

- Keys: `uuid`, `name`, `is_vec2`, `min`, `max`, `defaults`, `axis_points`, `merge_mode`, `bindings`. Missing values
  default to min `[0, 0]`, max `[1, 1]` and axis points `[[0, 1], [0, 1]]`.
- **Axis points are normalised**, `0..1` across the range: a key at `t` sits at `min + t * (max - min)` in the
  parameter's units. This is the conversion the importer applies (checked numerically in `tests/payload.rs`), and the
  writer inverts it.
- A one-axis parameter uses only `values[x][0]`.

**Bindings** (`core/param/binding.d`).

- Keys: `node` (uuid), `param_name` (the target), `values[x][y]`, `isSet[x][y]`, `interpolate_mode`. Both grids are
  forced to x count × y count.
- Inochi2D drops a binding whose node is missing, and so do we, with a finding.
- Unset keys are filled from their set neighbours at load time.
- Interpolation (`core/package.d:642-670`) is Nearest, Linear, Stepped, Cubic or Bezier. We map Stepped to Nearest
  and Bezier to Cubic, each with a finding.

Targets:

| Target | Values | Our mapping |
|---|---|---|
| `zSort` | offset | `ZSort` |
| `transform.t.x`, `.t.y` | offset | `TranslateX/Y` |
| `transform.t.z` | offset | dropped (no picture effect) |
| `transform.r.x`, `.r.y`, `.r.z` | offset, radians | `RotateX`, `RotateY`, `Rotate` |
| `transform.s.x`, `.s.y` | factor | `ScaleX/Y` |
| `deform` | per-vertex `[x, y]` offsets | `Deform` (offsets) |
| `opacity` | factor | `Opacity` |
| `tint.r/g/b` | factor | `TintR/G/B` |
| `screenTint.r/g/b`, `emissionStrength`, `alphaThreshold` | | dropped: no target in the contract |
| physics settings (`gravity`, `length`, `frequency`, dampings, `outputScale.x/y`) | | dropped: no target in the contract |

Our values are laid out x-major (`key_index = ix * ny + iy`).

### Physics

**SimplePhysics** (`core/nodes/drivers/simplephysics.d`; the integrator is `phys/system.d`).

- Keys: `param` (the uuid of the parameter it writes), `model_type` (`Pendulum` or `SpringPendulum`), `map_mode`
  (`AngleLength`, `XY`, `LengthAngle`, `YX`), `gravity`, `length`, `frequency`, `angle_damping`, `length_damping`,
  `output_scale`, `local_only`.
- Defaults: gravity 1, length 100, frequency 1, dampings 0.5 and 0.5, output scale (1, 1).
- Effective gravity is `gravity × puppet gravity × pixelsPerMeter`, in pixels per second squared.
- The anchor is the node's own global position, or its local one with `local_only`. The bob starts one length below
  the anchor.

**Equations**, in our notation, with g the effective gravity, L the length, θ the angle off the downward vertical, ζa
and ζl the dampings, and f the frequency:

- **Rigid pendulum:** θ'' = −(g/L)·sin θ − 2·ζa·√(g/L)·θ'. Each step first re-measures θ from the moved anchor to the
  bob, then integrates, then puts the bob back at length L.
- **Spring pendulum:** the bob is a 2D point with k = (2πf)².
  - It feels gravity (0, g) plus a spring force −k·(d − (L − g/k)) along the anchor-to-bob direction, where d is the
    current distance.
  - Damping acts separately across the rod (2·ζa·√(g/L)) and along it (2·ζl·2πf). The velocity is rotated into the
    rod's frame, damped, and rotated back.
- **Integration:** classic RK4, in steps of 0.01 s, with the frame time clamped to 10 s.

**Output** maps the bob back through the anchor node's inverse transform:

- AngleLength: x = atan2(−dx, dy)/π, y = distance / L.
- XY: the bob's offset in lengths, minus (0, 1), with y negated ("y up" for parameters).
- LengthAngle and YX: the same two pairs, swapped.
- The result is multiplied by the output scale and forced onto the parameter as an offset.

**Our mapping.** Each `SimplePhysics` node becomes a `Group` plus a `Pendulum` anchored on that node, and the puppet's
`physics` becomes `PhysicsWorld`. A pendulum whose parameter is missing is dropped. One with a non-positive length (or
a spring with a non-positive frequency) is dropped. A rigid one with a non-positive frequency gets 1 Hz, which it never
reads.

### Metadata and rights

Meta keys:

- `name`, `version`, `rigger`, `artist`, `copyright`, `licenseURL`, `contact`, `reference`, `thumbnailId`,
  `preservePixels`;
- `rights`, an object of `allowedUsers`, `allowViolence`, `allowSexual`, `allowCommercial`, `allowRedistribution`,
  `allowModification`, `requireAttribution`.

How we map them:

- `name`, `artist`, `rigger`, `copyright` and the licence URL map to `PuppetMeta`. The rest are notes in the report.
- `source` is `inochi2d`.
- **Rights are kept verbatim**, key by key, as text (`false` stays `"false"`).
- When the file states none, none are assumed. The report says the rights are unknown (`inochi2d.rights_unknown`);
  nothing defaults to granted.

### What the importer makes up

- **The canvas.** Inochi2D has none: its camera centres the root. Ours is the smallest box centred on the root that
  holds every part and mask at rest, or the largest texture when nothing has vertices.
- **Texture file names.** `textures/<i>.png`.

Both are reported as synthesised.

## INP2

INP2 (`TRNSRTS2`, Inochi2D 0.9, `tech-docs/inp2.md` in the main repository) is a tagged little-endian tree.

**Tags and layout.**

- Each value starts with a `u32` tag: the low byte is the type (nil, bool, int, uint, float, string, blob, array
  begin/end, object begin/end, key), and the upper 24 bits are a length or count.
- Strings and blobs longer than `0xFFFFFE` put `0xFFFFFF` there and their length in the next word.
- Blobs carry a CRC-32. Everything is 4-byte aligned from the start of the file.
- The root object holds `INP_SECT` (the puppet), `TEX_SECT` (an array of `{encoding, data}`; the spec page names the
  code `format`, the loader `encoding`, and we accept both) and `EXT_SECT`.

**Bugs in the 0.9 reference reader.** It leaves array and object end sentinels unread, and skips the value word of a
boolean. A file its own writer made therefore reads back misaligned whenever containers nest. Our reader follows the
writer and requires both.

**The payload.** The schema (`tech-docs/inp2-puppet.md`) differs from INP1's: `guid` strings, `properties`, and a
different transform form. 0.9's own puppet serialiser is unimplemented, so no tool writes INP2 puppets yet. With no
files and no settled schema, mapping the payload now would be guesswork.

**What we do.**

- `read_container` returns INP2's tree, textures and vendor data, with the same limits and checks.
- `import` refuses an INP2 file with `inochi2d.inp2_payload`.
- Mapping the payload is a checklist item for when 0.9 ships a writer.

## Existing code and licences

| Source | Licence | What it is | How we use it |
|---|---|---|---|
| Inochi2D (`github.com/Inochi2D/inochi2d`), D | BSD-2-Clause | the reference runtime and loader; its `tech-docs/` (INP2) live in the same repository | read as the primary source for every convention above; no code copied |
| inox2d (`github.com/Inochi2D/inox2d`), Rust | BSD-2-Clause | a prototype runtime; INP1 container and payload parsing, no MeshGroup | read for comparison (its transform order differs); no code copied |
| inochi2d-docs (`github.com/Inochi2D/inochi2d-docs`) | CC-BY-3.0 (branding assets excepted) | the user docs and the INP1 spec pages (`spec/inp`, `spec/payload/blending`) | read; no text copied |
| Inochi Creator (`github.com/Inochi2D/inochi-creator`), D | BSD-2-Clause | the editor that writes `.inx` | read to confirm premultiplication on import |

Because nothing is copied or ported, the importer carries no upstream notice. The decision stays recorded in
`docs/licensing/deps/inochi2d.toml`.

## Primary sources (commits pinned 2026-10-29)

| Repository | Ref | Commit |
|---|---|---|
| `github.com/Inochi2D/inochi2d` | `main` (0.9) | `4975d247f9b946a74d18e0ba9b3e9475eb636efb` |
| `github.com/Inochi2D/inochi2d` | tag `v0.8.7` | `e2235f6f29688d4013a0eec116a37ec8af8f1fb5` |
| `github.com/Inochi2D/inochi2d` | branch `v0_8` | `fdb241da048dbe330152f7b0015e2129dc392844` |
| `github.com/Inochi2D/inochi2d` | tag `v0.7.2` | `6114bd16b1bcefa8a094b3519d5d4d445895509d` |
| `github.com/Inochi2D/inox2d` | `main` | `d4dd9dd7f16b775042cbda44370570abf9a8cf81` |
| `github.com/Inochi2D/inochi2d-docs` | `main` | `593c657855d5679f01443ef311346298fd25e117` |
| `github.com/Inochi2D/inochi-creator` | tag `v0.8.6` | `371e5f4e21117102dbecc82371bfd9a6e2f5df02` |
| `github.com/Inochi2D/example-models` | `main` | `cd95dd00ddff63b1f7d2b84a19914c3c70d05945` (sample check only; LFS objects `dbf82ffb…` and `bfa33997…`; no sample kept) |

## Full scope (checklist)

Done in the first chunk:

- [x] INP1 container: magic, payload, `TEX_SECT`, `EXT_SECT`, unknown sections kept, hostile-input limits, no panics
  (10,000 seeded mutations).
- [x] Node types: Node, Part, Composite, MeshGroup, SimplePhysics, Mask (0.7); unknown types kept as `Unknown`.
- [x] Transforms (x/y turns folded into scale), zsort, enabled; `lockToRoot` reported.
- [x] Part meshes (origin applied), textures (PNG, TGA), blend modes, opacity, tint, screen tint, masks (both forms),
  mask threshold.
- [x] Parameters (normalised axis points converted), bindings (every target the contract has, `isSet`,
  interpolation), x-major layout.
- [x] SimplePhysics → pendulums; puppet physics → world.
- [x] Metadata verbatim, rights unknown stays unknown.
- [x] Import report: mapped counts, findings with `inochi2d.<rule>` ids and JSON paths.
- [x] INP1 writer for our own puppets; writer → reader round trip is exact.
- [x] INP2 container reader (tags, alignment, CRC, sentinels, limits).

Open:

- [ ] INP2 payload mapping, once Inochi2D 0.9 writes puppets and the schema settles.
- [ ] BC7 textures: needs a permissive pure-Rust BC7 decoder (none in the workspace); a transparent placeholder and an
  unsupported finding stand in.
- [ ] Contract gaps (each needs an `omuzaki-puppet-api` change, the contract owner's call):
  - binding targets for screen tint, mask threshold and emission;
  - `lockToRoot` on a node;
  - a MeshGroup's reach (not through plain nodes, `translate_children`, `dynamic_deformation`);
  - points outside every group triangle staying put rather than extrapolating;
  - a composite's `propagate_meshgroup`;
  - the Inverse, DestinationIn and AddGlow blend modes;
  - emission and bump-map textures;
  - Stepped and Bezier interpolation.
- [ ] Parameter `merge_mode` other than additive, automation, animations: reported, not modelled.
- [ ] An `.inx`-only Inochi Creator extras pass (its vendor data is kept as bytes today).
