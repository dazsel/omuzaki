# 0055: The avatar scene models extended colliders and spring joint limits, a breaking `omuzaki-io-api` change (2026-10-15)

**Status:** proposed (PR #1491); accepted by the Director when merged. **Context:** #982, EX-11 (#607);
`docs/design/io-api.md` D9 ("Spring bones"); `VRMC_springBone_extended_collider-1.0`, `VRMC_springBone_limit-1.0`.

## Context

`omuzaki-io-api`'s `ColliderShape` held only `Sphere` and `Capsule`, and `SpringJoint` had no limit, so importers
dropped the inside-out and plane colliders and the joint limits of VRM 1.0 files. EX-11 needs them in the scene. Adding
them changes public types that other crates match exhaustively or build with struct literals, which is a breaking API
change.

## Decision

1. `ColliderShape` gains `SphereInside`, `CapsuleInside` and `Plane`, and becomes `#[non_exhaustive]`: the VRM family
   keeps adding shapes, and the next one should not break consumers again.
2. A new `#[non_exhaustive]` enum `JointLimit` (`Cone`, `Hinge`, `Spherical`; angles in radians, `rotation` in the
   joint's local space) is re-exported from the crate root.
3. `SpringJoint` gains a required public field `limit: Option<JointLimit>`. `SpringJoint::new` sets it to `None`.
4. `IoScene::check` gains `avatar.collider_invalid` and `avatar.limit_invalid` (errors) and `avatar.limit_on_tail`
   (warning).
5. `vrm_profile` reports what each version's exporter cannot write yet, as warnings: `vrm.collider_dropped` and
   `vrm.limit_dropped` for 1.0, `vrm0.collider_unsupported` and `vrm0.limit_unsupported` for 0.x. The writers do not
   raise them, so `Exporter::check` shows them and strict mode refuses the export before anything is written.

No importer produces the new shapes or limits yet, and no exporter writes them: a 1.0 export writes the spec's inert
fallback sphere in a shape's place and leaves limits out; a 0.x export leaves both out.

## Migration

- **A `match` on `ColliderShape` or `JointLimit` outside `omuzaki-io-api`** needs a fallback arm (`_ => …` or
  `other => …`). Pick what is safe when the shape is unknown: skip it and report it, rather than treat it as a sphere.
- **A `SpringJoint { .. }` struct literal** adds `limit: None` (or the limit it means), or starts from
  `SpringJoint::new(node)` and sets the fields it needs.
- **A comparison of whole joints** (for example, whether a chain's joints share parameters) includes `limit`; a caller
  that means "the same parameters except the node" sets both sides' `node` and `limit` alike first, as the 0.x writer
  does.
- **Tools that read export reports** may see the four new warnings; with strict export they now cause a rejection.

## Consequences

- Downstream crates in this workspace were updated in the same PR; a crate outside it fails to compile until it follows
  the migration above, once.
- Adding a further shape or limit kind is no longer breaking.
- When EX-11's writer lands, the 1.0 warnings go away for the shapes and limits it writes; the 0.x ones stay.
