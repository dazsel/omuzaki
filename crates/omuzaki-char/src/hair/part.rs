//! The `hair` part kind: a hairstyle's groups compiled to clumps ([`compile`](super::compile)) and drawn as lens tubes
//! ([`tube`](super::tube)) with the hair shading program.
//!
//! **Meshes.** Static groups (not simulated) go into one mesh, `<part>-static`, skinned wholly to the head joint.
//! Simulated groups go into `<part>-dynamic`, skinned wholly to the head joint too and not marked dynamic, so they
//! follow the head at their rest shape. That is interim: until the hair runtime lands (#322) nothing moves them, and
//! #322 reverts this to an unskinned, dynamic mesh. Within a mesh the tubes come first, in group order, and any
//! generator-made shells (the cap) after them, so a mesh can be walked clump by clump from the front
//! ([`recolour`](Hair::recolour) does).
//!
//! **Material.** One double-sided `hair` material per part: the `hair` shading program ([`HairShading::shading`]), vertex
//! colours for streak tints, the outline in the hair's outline colour, and the brows overlay when `brows_through`. A palette
//! change re-resolves the colours without compiling a clump ([`Hair::recolour`]).
//!
//! **Why tubes.** A lock is a closed lens, wound to face out of the tube, so the renderer's inverted-hull outline finds
//! a back face from every side (a flat ribbon wound outward gets no outline from outside).
//!
//! **Tails and buns.** An enabled tail or bun group also publishes its anchors (`<group>_tail_root`, `<group>_bun_centre`,
//! `<group>_bun_tie`) for accessories to attach to, and a tail adds a spring chain per side to the rig
//! (`publish_ties`). The tail's tubes still skin to the head until the hair runtime (#322) moves them; the chain's
//! bones are in the rig for it.
//!
//! **Not drawn yet.** The curtain's strip geometry is the curtain generator's and the runtime's: a group
//! that compiles to a strip reports a warning instead of a mesh.

use std::collections::BTreeMap;
use std::ops::Range;

use omuzaki_char_api::{
    AnchorsFact, BodyRegion, BuildCtx, BuildError, FaceOverlay, Generator, GroupId, HAIR_UV, HairParams, HeadSurface, JointRef, KindProvenance,
    MaterialName, MaterialSpec, PartCategory, PartId, PartKind, PartMesh, PartMeta, PartOutput, RecolourCtx,
    Recoloured,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::{FaceLayerId, Palette, narrow};
use omuzaki_render_api::{AlphaMode, Outline};

use super::compile::{self, CompiledEntry, Dispatch, GroupGenerator, GroupGeometry, draws_tubes, enabled};
use super::r#gen::tail::Chain;
use super::r#gen::{bun, tail};
use super::tie;
use super::tube::{CAP_VERTICES, OutwardFrame, RING, TubeError, TubeLook, TubeMesh, polar_angles, vertex_colour};

/// Where along the torso the body's vertical axis is read, from hips (0) to neck base (1): the upper back, where long
/// hair hangs.
const BODY_AXIS_AT: f32 = 0.7;

/// The `hair` kind.
pub struct Hair;

impl PartKind for Hair {
    type Params = HairParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "hair",
            summary: "Build a hairstyle from procedural groups: lens-tube locks with the hair shading program.",
            category: PartCategory::Hair,
            requires: &["head"],
        }
    }

    fn provenance() -> KindProvenance {
        KindProvenance::ProjectOwned
    }

    fn build(params: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_with(params, ctx, &Dispatch)
    }

    /// Re-tints the vertex colours of the streaked clumps and re-resolves the material: no clump is compiled and no
    /// tube drawn. The streak tints come from the groups alone ([`HairShading::streak_tints`]) and each clump's vertices
    /// are found by walking the mesh (every tube's last ring has `hair-uv.y = 1`).
    ///
    /// [`HairShading::streak_tints`]: omuzaki_char_api::HairShading::streak_tints
    fn recolour(params: &HairParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        recolour_with(params, output, ctx, &Dispatch)
    }
}

omuzaki_char_api::register_part_kind!(Hair);

/// Builds the hair with `generators` in place of the real ones. The part kind calls this with [`Dispatch`]; tests and
/// tools call it with their own generators to exercise the pipeline (ordering, mirroring, meshing) on known clumps.
///
/// # Errors
/// [`BuildError::Missing`] without a head, [`BuildError::Colour`] for an undefined palette slot, [`BuildError::Part`] for
/// an invalid legacy seed, a generator whose clump count disagrees with its `clump_count`, or a clump with fewer than two
/// points.
pub fn build_with(
    params: &HairParams,
    ctx: &mut BuildCtx<'_>,
    generators: &dyn GroupGenerator,
) -> Result<PartOutput, BuildError> {
    let head = ctx.head()?;
    let fail = |message: String| BuildError::Part { part: ctx.part.clone(), message };
    let mut stream =
        compile::stream(params.rng, &mut ctx.rng).map_err(|seed| fail(format!("invalid legacy seed {seed}")))?;
    let mut entries =
        compile::compile_groups(params, generators, head.as_ref(), ctx.palette, ctx.quality, stream.as_mut())
            .map_err(|e| fail(e.to_string()))?;
    drop(stream);
    apply_streaks(params, ctx.palette, &mut entries).map_err(|e| ctx.fail(e.to_string()))?;

    let frame = OutwardFrame { head: head.centre(), body_axis: body_axis(ctx, head.centre()) };
    let mut warnings = Vec::new();
    let chains = tail_chains(params, ctx, head.as_ref())?;
    let Meshes { stat, dynamic, ties } =
        mesh_groups(&entries, &frame, &chains, &mut warnings).map_err(|e| ctx.fail(e.to_string()))?;

    let material_name =
        MaterialName::new(format!("{}-hair", ctx.part)).map_err(|e| ctx.fail(format!("material name: {e}")))?;
    let overlays = || {
        if params.shading.brows_through { Ok(vec![ctx.face_overlay(FaceLayerId::Brows)?]) } else { Ok(Vec::new()) }
    };
    let material = hair_material(ctx.part, params, ctx.palette, overlays)?;
    let mut out = PartOutput::new().with_materials(vec![(material_name.clone(), material)]).with_warnings(warnings);
    // Hair follows the head, except a tail's locks, which follow their chain's bones (the runtime moves those).
    for (simulated, built) in [(false, stat), (true, dynamic)] {
        let Some(Skinned { mut geometry, runs }) = built else { continue };
        let joint_bones = skin_runs(&mut geometry, &runs).map_err(|e| ctx.fail(e))?;
        out.meshes.push(PartMesh {
            name: mesh_name(ctx.part, simulated),
            geometry,
            joint_bones,
            material: material_name.clone(),
            casts_shadow: true,
            render_order: 1,
            dynamic: false,
        });
    }
    if let Some(mut geometry) = ties {
        skin_to_head(&mut geometry);
        let tie_name = tie_material_name(ctx.part).map_err(|e| ctx.fail(format!("material name: {e}")))?;
        out.materials.push((tie_name.clone(), tie_material(ctx.part, params, ctx.palette)?));
        out.meshes.push(PartMesh {
            name: ties_mesh_name(ctx.part),
            geometry,
            joint_bones: vec![JointRef::HEAD],
            material: tie_name,
            casts_shadow: true,
            render_order: 2,
            dynamic: false,
        });
    }
    publish_ties(params, ctx, head.as_ref(), &mut out)?;
    Ok(out)
}

/// Publishes the anchors of every enabled tail and bun group, and adds each tail's spring chain to the rig.
///
/// **Anchor names.** Keys are `(part, anchor)`, and a part can hold several tail or bun groups, so each anchor is named
/// `<group id>_<anchor>` (`twintails_tail_root_l`; a mirrored tail group adds `_l` for the authored side and `_r` for
/// the reflected one, a single tail none). **Bone and chain names** start `hair_<part>_<group id>` for the same reason,
/// and a mirrored group's sides carry `_l` and `_r` segments. The chains collide with every collider group published
/// before the hair (the head and shoulders).
fn publish_ties(
    params: &HairParams,
    ctx: &BuildCtx<'_>,
    head: &dyn HeadSurface,
    out: &mut PartOutput,
) -> Result<(), BuildError> {
    let colliders: Vec<String> = ctx.colliders().iter().map(|group| group.name.clone()).collect();
    for (id, group) in enabled(params) {
        let anchors = match &group.generator {
            Generator::Tail(g) => {
                let prefix = chain_prefix(ctx.part, id);
                tail::rig(g, head, &prefix, group.mirror, &colliders, &mut out.rig)
                    .map_err(|e| ctx.fail(format!("group '{id}': a tail bone name: {e}")))?;
                tail::anchors(g, head, group.mirror)
            }
            Generator::Bun(g) => {
                bun::anchors(g, head).into_iter().map(|(name, anchor)| (name.to_owned(), anchor)).collect()
            }
            _ => continue,
        };
        for (name, anchor) in anchors {
            let key = format!("{id}_{name}");
            let anchor_id = key.parse().map_err(|e| ctx.fail(format!("group '{id}': anchor name '{key}': {e}")))?;
            out.publishes.put::<AnchorsFact>((anchor_id, anchor));
        }
    }
    Ok(())
}

/// The hair's streak and shade pass over every clump of every group, in compile order.
fn apply_streaks(
    params: &HairParams,
    palette: &Palette,
    entries: &mut [CompiledEntry],
) -> Result<(), omuzaki_char_api::HairShadingError> {
    let counts: Vec<usize> = entries.iter().map(|e| e.group.clumps.len()).collect();
    let mut all: Vec<_> = entries.iter_mut().flat_map(|e| std::mem::take(&mut e.group.clumps)).collect();
    params.shading.apply_to_clumps(palette, &mut all)?;
    let mut all = all.into_iter();
    for (entry, count) in entries.iter_mut().zip(counts) {
        entry.group.clumps = all.by_ref().take(count).collect();
    }
    Ok(())
}

/// The body's vertical axis as `(x, z)`: the torso's centre in the upper back, else the head's own.
fn body_axis(ctx: &BuildCtx<'_>, head: Vec3) -> Vec2 {
    ctx.fit()
        .ok()
        .and_then(|fit| fit.section(BodyRegion::Torso, BODY_AXIS_AT))
        .map_or(Vec2::new(head.x, head.z), |s| Vec2::new(s.centre.x, s.centre.z))
}

/// The static and the dynamic mesh of the entries (`None` for a mesh with nothing in it): tubes first, in group order,
/// then shells. Pushes a warning for each group that made nothing, or that is a strip (not drawn yet).
fn mesh_groups<'a>(
    entries: &[CompiledEntry],
    frame: &OutwardFrame,
    chains: &'a BTreeMap<GroupId, Vec<Chain>>,
    warnings: &mut Vec<String>,
) -> Result<Meshes<'a>, TubeError> {
    let (mut stat, mut dynamic) = (TubeMesh::new(), TubeMesh::new());
    let (mut stat_clumps, mut dynamic_clumps) = (0usize, 0usize);
    let (mut stat_runs, mut dynamic_runs, mut bands) = (Vec::new(), Vec::new(), Vec::new());
    for entry in entries {
        match &entry.group.geometry {
            GroupGeometry::Tube | GroupGeometry::Tied { .. } => {
                if let GroupGeometry::Tied { ties } = &entry.group.geometry {
                    bands.extend_from_slice(ties);
                }
                let (target, drawn, runs) = if entry.simulate {
                    (&mut dynamic, &mut dynamic_clumps, &mut dynamic_runs)
                } else {
                    (&mut stat, &mut stat_clumps, &mut stat_runs)
                };
                // A tail's clumps follow the chains of its group, the first share of them the first chain's.
                let chains = chains.get(&entry.id).map_or(&[][..], Vec::as_slice);
                let per_chain = entry.group.clumps.len().checked_div(chains.len()).unwrap_or(0).max(1);
                for (k, clump) in entry.group.clumps.iter().enumerate() {
                    let polar = polar_angles(&clump.rest, frame.head);
                    let look = TubeLook {
                        seed: strand_seed(*drawn, entry.simulate),
                        shade: clump.shade,
                        polar: &polar,
                        tint: clump.tint,
                        flip: 1.0,
                    };
                    let first = target.vertex_count();
                    target.push(&clump.rest, &clump.width, &clump.thickness, |p| frame.rest(p), &look)?;
                    if let Some(chain) = chains.get(k / per_chain) {
                        runs.push(Run { range: first..target.vertex_count(), chain });
                    }
                    *drawn += 1;
                }
                if entry.group.clumps.is_empty() {
                    warnings.push(format!("the '{}' group made no clumps", entry.id));
                }
            }
            GroupGeometry::Strip { .. } if !entry.group.clumps.is_empty() => {
                warnings.push(format!("the '{}' group is a strip, which is not drawn yet", entry.id));
            }
            GroupGeometry::Strip { .. } | GroupGeometry::Shell(_) => {}
        }
    }
    for entry in entries {
        if let GroupGeometry::Shell(shell) = &entry.group.geometry {
            let target = if entry.simulate { &mut dynamic } else { &mut stat };
            if shell.positions.is_empty() {
                warnings.push(format!("the '{}' group made no shell", entry.id));
            } else if !target.append(shell) {
                warnings.push(format!("the '{}' shell lacks valid hair-uv and hair-info and was left out", entry.id));
            }
        }
    }
    let finish = |mesh: TubeMesh| if mesh.is_empty() { Ok(None) } else { mesh.finish().map(Some) };
    Ok(Meshes {
        stat: finish(stat)?.map(|geometry| Skinned { geometry, runs: stat_runs }),
        dynamic: finish(dynamic)?.map(|geometry| Skinned { geometry, runs: dynamic_runs }),
        ties: tie::mesh(&bands),
    })
}

/// What [`mesh_groups`] makes: the two hair meshes and the tie bands.
struct Meshes<'a> {
    stat: Option<Skinned<'a>>,
    dynamic: Option<Skinned<'a>>,
    ties: Option<MeshData>,
}

/// A hair mesh and the runs of vertices that follow a tail's chain instead of the head.
struct Skinned<'a> {
    geometry: MeshData,
    runs: Vec<Run<'a>>,
}

/// The vertices of one tail clump, and the chain they follow.
struct Run<'a> {
    range: Range<usize>,
    chain: &'a Chain,
}

/// Skins `mesh` to the head, then every run to its chain's bones (by arc length, see [`tail::skin`]); returns the
/// mesh's joint list: the head, then each chain's bones in first-use order.
///
/// # Errors
/// A message if the joint list outgrows the mesh's 16-bit joint indices.
fn skin_runs(mesh: &mut MeshData, runs: &[Run<'_>]) -> Result<Vec<JointRef>, String> {
    skin_to_head(mesh);
    let mut joints = vec![JointRef::HEAD];
    let mut bases: Vec<(&str, usize)> = Vec::new();
    for run in runs {
        let base = match bases.iter().find(|(name, _)| *name == run.chain.name) {
            Some((_, base)) => *base,
            None => {
                let base = joints.len();
                joints.extend(run.chain.bones.iter().cloned().map(JointRef::Named));
                bases.push((run.chain.name.as_str(), base));
                base
            }
        };
        let index = |bone: usize| u16::try_from(base + bone).map_err(|_| "more joints than a mesh can index".to_owned());
        for v in run.range.clone() {
            let s = tail::skin(run.chain, mesh.positions[v]);
            let [head, here, next] = s.weights();
            mesh.joints[v] = [0, index(s.bone)?, index(s.bone + 1)?, 0];
            mesh.weights[v] = [head, here, next, 0.0];
        }
    }
    Ok(joints)
}

/// The chains of every enabled tail group, by group id, for the skin and the rig.
fn tail_chains(
    params: &HairParams,
    ctx: &BuildCtx<'_>,
    head: &dyn HeadSurface,
) -> Result<BTreeMap<GroupId, Vec<Chain>>, BuildError> {
    let mut out = BTreeMap::new();
    for (id, group) in enabled(params) {
        if let Generator::Tail(g) = &group.generator {
            let chains = tail::chains(g, head, &chain_prefix(ctx.part, id), group.mirror)
                .map_err(|e| ctx.fail(format!("group '{id}': a tail bone name: {e}")))?;
            out.insert(id.clone(), chains);
        }
    }
    Ok(out)
}

/// The start of every bone and chain name of a tail group: `hair_<part>_<group id>`.
fn chain_prefix(part: &PartId, group: &GroupId) -> String {
    format!("hair_{part}_{group}")
}

/// The strand seed of the `index`-th clump of a mesh (`hair-info.x`), as the prototype spreads them: a golden-ratio-like
/// stride, offset for the dynamic mesh so the two don't repeat each other.
#[allow(clippy::cast_precision_loss)] // clump indices are small
fn strand_seed(index: usize, dynamic: bool) -> f32 {
    let i = index as f32;
    if dynamic { (i * 0.271 + 0.5).fract() } else { (i * 0.137).fract() }
}

/// The name of the static or the simulated mesh of `part`: `<part>-static` or `<part>-dynamic`.
fn mesh_name(part: &PartId, simulated: bool) -> String {
    format!("{part}-{}", if simulated { "dynamic" } else { "static" })
}

/// The name of the tie mesh of `part`.
fn ties_mesh_name(part: &PartId) -> String {
    format!("{part}-ties")
}

/// The name of the tie material of `part`.
fn tie_material_name(part: &PartId) -> Result<MaterialName, omuzaki_core::id::SlugError> {
    MaterialName::new(format!("{part}-tie"))
}

/// The hair tie's material: a toon of the shading's `tie` colour. `build` and `recolour` share it.
fn tie_material(part: &PartId, params: &HairParams, palette: &Palette) -> Result<MaterialSpec, BuildError> {
    let colour = palette
        .resolve(&params.shading.tie)
        .map_err(|error| BuildError::Colour { part: part.clone(), error })?;
    Ok(MaterialSpec::toon(colour))
}

/// Skins every vertex wholly to the head joint, the one joint of `joint_bones`.
fn skin_to_head(mesh: &mut MeshData) {
    let n = mesh.positions.len();
    mesh.joints = vec![[0; 4]; n];
    mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]; n];
}

/// The hair material. Every colour it takes comes from `palette`, and `build` and `recolour` share this one function so
/// they cannot drift apart. `overlays` is asked for after the shading, so the first error is the same either way.
fn hair_material(
    part: &PartId,
    params: &HairParams,
    palette: &Palette,
    overlays: impl FnOnce() -> Result<Vec<FaceOverlay>, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    let shading =
        params.shading.shading(palette).map_err(|e| BuildError::Part { part: part.clone(), message: e.to_string() })?;
    let overlays = overlays()?;
    let colour =
        palette.resolve(&params.shading.outline).map_err(|error| BuildError::Colour { part: part.clone(), error })?;
    Ok(MaterialSpec {
        base: Colour::WHITE,
        base_texture: None,
        shading,
        overlays,
        alpha: AlphaMode::Opaque,
        double_sided: true,
        vertex_colours: true,
        outline: Some(Outline { colour, width: narrow(params.shading.outline_width) }),
        receives_shadows: true,
    })
}

/// One clump of the hair as a recolour sees it: from the parameters alone.
struct Slot<'a> {
    group: &'a GroupId,
    simulate: bool,
    /// Whether the clump is a tube in a mesh (a shell or strip clump has no vertices of its own to tint).
    tube: bool,
}

/// Every clump, in compile order.
fn layout<'a>(params: &'a HairParams, generators: &dyn GroupGenerator) -> Vec<Slot<'a>> {
    enabled(params)
        .flat_map(|(id, group)| {
            let slot = Slot { group: id, simulate: group.dynamics.simulate, tube: draws_tubes(&group.generator) };
            std::iter::repeat_with(move || Slot { ..slot }).take(compile::clumps_in(group, generators))
        })
        .collect()
}

/// [`Hair::recolour`] with `generators` in place of the real ones (for the clump counts).
///
/// # Errors
/// [`BuildError::Colour`] for a slot the new palette lacks.
pub fn recolour_with(
    params: &HairParams,
    output: &mut PartOutput,
    ctx: &RecolourCtx<'_>,
    generators: &dyn GroupGenerator,
) -> Result<Recoloured, BuildError> {
    let layout = layout(params, generators);
    let groups: Vec<&GroupId> = layout.iter().map(|s| s.group).collect();
    let tints = params.shading.streak_tints(ctx.palette, &groups).map_err(|e| ctx.fail(e.to_string()))?;
    // A mesh the layout says holds tubes, or a cap's shell (which has no clumps, so the layout cannot see it), must
    // exist: without it there is nothing to tint or keep, and `Done` would be a lie.
    for simulated in [false, true] {
        let expects_tubes = layout.iter().any(|s| s.tube && s.simulate == simulated);
        let expects_shell =
            enabled(params).any(|(_, g)| matches!(g.generator, Generator::Cap(_)) && g.dynamics.simulate == simulated);
        let name = mesh_name(ctx.part, simulated);
        if (expects_tubes || expects_shell) && !output.meshes.iter().any(|m| m.name == name) {
            return Ok(Recoloured::NeedsRebuild);
        }
    }
    let expects_ties = enabled(params).any(|(_, g)| matches!(g.generator, Generator::Tail(_)));
    if expects_ties != output.meshes.iter().any(|m| m.name == ties_mesh_name(ctx.part)) {
        return Ok(Recoloured::NeedsRebuild);
    }
    for mesh in &mut output.meshes {
        if mesh.name == ties_mesh_name(ctx.part) {
            continue; // the band has no hair vertices to tint
        }
        // Found by name: a mesh's `dynamic` flag is not the simulated groups' mark while no runtime moves them.
        let simulated = mesh.name == mesh_name(ctx.part, true);
        let mine = layout.iter().zip(&tints).filter(|(s, _)| s.tube && s.simulate == simulated).map(|(_, t)| *t);
        if !paint_tubes(&mut mesh.geometry, &mine.collect::<Vec<_>>()) {
            return Ok(Recoloured::NeedsRebuild); // not the hair `build` makes: don't guess
        }
    }
    let tie_name = tie_material_name(ctx.part).map_err(|e| ctx.fail(format!("material name: {e}")))?;
    for (name, material) in &mut output.materials {
        if *name == tie_name {
            *material = tie_material(ctx.part, params, ctx.palette)?;
        } else {
            *material = hair_material(ctx.part, params, ctx.palette, || Ok(std::mem::take(&mut material.overlays)))?;
        }
    }
    Ok(Recoloured::Done)
}

/// Gives the vertices of each tube at the front of `mesh` the colour of its tint, in order. Returns `false`, changing
/// nothing, if the mesh isn't `tints.len()` tubes followed by (at most) shells.
fn paint_tubes(mesh: &mut MeshData, tints: &[Option<Colour>]) -> bool {
    if mesh.colours.len() != mesh.positions.len() {
        return false;
    }
    let Some(ranges) = tube_ranges(mesh, tints.len()) else { return false };
    for (range, tint) in ranges.into_iter().zip(tints) {
        mesh.colours[range].fill(vertex_colour(*tint));
    }
    true
}

/// The vertex ranges of the first `count` tubes of `mesh`: a tube starts at a ring whose `hair-uv.y` is 0 and ends after
/// the ring where it reaches 1 (the last ring of every tube) and its two caps, which take the tube's tint too. `None` if
/// the mesh doesn't hold that many.
fn tube_ranges(mesh: &MeshData, count: usize) -> Option<Vec<Range<usize>>> {
    let uv = &mesh.extra_attribute(HAIR_UV)?.data;
    let along = |vertex: usize| uv.get(2 * vertex + 1).copied();
    let mut ranges = Vec::with_capacity(count);
    let mut at = 0;
    for _ in 0..count {
        let start = at;
        if along(at)? != 0.0 {
            return None;
        }
        loop {
            let t = along(at)?;
            at += RING;
            if t >= 1.0 {
                break;
            }
        }
        at += CAP_VERTICES;
        ranges.push(start..at);
    }
    (at <= mesh.positions.len()).then_some(ranges)
}
