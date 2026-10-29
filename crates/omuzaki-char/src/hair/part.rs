//! The `hair` part kind: a hairstyle's groups compiled to clumps ([`compile`]) and drawn as lens tubes
//! ([`tube`](super::tube)) with the hair shading program.
//!
//! **Meshes.** Static groups (not simulated) go into one mesh, `<part>-static`, skinned wholly to the head joint.
//! Simulated groups go into `<part>-dynamic`, an unskinned, dynamic mesh in character space that the hair runtime
//! ([`HairRuntime`]) rewrites every step from its Verlet chains, which collide with the head, neck, upper back and
//! shoulders ([`colliders`](super::physics::colliders)). An export build (`rig_export`) has no runtime: the dynamic
//! mesh is skinned as the static one is, for the spring chains to move in other apps. Within a mesh the tubes come first, in group order, and any
//! generator-made shells (the cap) after them, so a mesh can be walked clump by clump from the front
//! ([`recolour`](Hair::recolour) does).
//!
//! **Material.** One double-sided `hair` material per part: the `hair` shading program (`HairShading::shading`), vertex
//! colours for streak tints, the outline in the hair's outline colour, and the brows overlay when `brows_through`. A palette
//! change re-resolves the colours without compiling a clump ([`Hair::recolour`]).
//!
//! **Why tubes.** A lock is a closed lens, wound to face out of the tube, so the renderer's inverted-hull outline finds
//! a back face from every side (a flat ribbon wound outward gets no outline from outside).
//!
//! **The curtain.** A group that compiles to a strip (the curtain) is meshed by
//! `curtain::group_mesh`: each guide's strip of columns, vertices of its own, in
//! order of `u`. A mesh holds its tubes first, then the strips, then the shells; the strips carry their clump's streak
//! tint like a tube, so [`recolour`](Hair::recolour) finds every clump.
//!
//! **Tails and buns.** An enabled tail or bun group also publishes its anchors (`<group>_tail_root`, `<group>_bun_centre`,
//! `<group>_bun_tie`) for accessories to attach to, and a tail adds a spring chain per side to the rig
//! (`publish_ties`). A tail's locks are skinned along that chain (the root blending into the head), so the hair runtime
//! (#322) and an exported avatar's spring bones move them.

use std::collections::BTreeMap;
use std::ops::Range;

use omuzaki_char_api::{
    AnchorsFact, BodyRegion, BuildCtx, BuildError, FaceOverlay, Generator, GroupId, HAIR_UV, HairParams, HeadSurface,
    JointRef, KindProvenance, MaterialName, MaterialSpec, PartCategory, PartId, PartKind, PartMesh, PartMeta,
    PartOutput, RecolourCtx, Recoloured, StatureFact,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::{FaceLayerId, Palette, narrow};
use omuzaki_render_api::{AlphaMode, Outline};

use super::bake;
use super::compile::{self, CompiledEntry, Dispatch, GroupGenerator, GroupGeometry, enabled};
use super::r#gen::bangs;
use super::r#gen::curtain::{self, BodyKeepOut, StripError};
use super::r#gen::tail::Chain;
use super::r#gen::{ahoge, bun, tail};
use super::physics::{self, runtime::HairRuntime};
use super::reach::{HairReach, HairReachFact};
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
    // The body the hair hangs over: the curtain keeps off it. A character without one still builds.
    let body = ctx.fit().ok();
    let keep_out = body.as_deref().map(BodyKeepOut::new);
    let fail = |message: String| BuildError::Part { part: ctx.part.clone(), message };
    let mut stream =
        compile::stream(params.rng, &mut ctx.rng).map_err(|seed| fail(format!("invalid legacy seed {seed}")))?;
    let mut entries = compile::compile_groups_over(
        params,
        generators,
        head.as_ref(),
        body.as_deref(),
        ctx.palette,
        ctx.quality,
        stream.as_mut(),
    )
    .map_err(|e| fail(e.to_string()))?;
    drop(stream);
    apply_streaks(params, ctx.palette, &mut entries).map_err(|e| ctx.fail(e.to_string()))?;

    let frame = OutwardFrame { head: head.centre(), body_axis: body_axis(ctx, head.centre()) };
    clear_fringes(params, head.as_ref(), &frame, &mut entries).map_err(|e| ctx.fail(e))?;
    let mut warnings = Vec::new();
    let chains = tail_chains(params, ctx, head.as_ref())?;
    let Meshes { stat, dynamic, ties } = mesh_groups(&entries, &frame, keep_out.as_ref(), &chains, &mut warnings)
        .map_err(|e| ctx.fail(e.to_string()))?;

    let material_name =
        MaterialName::new(format!("{}-hair", ctx.part)).map_err(|e| ctx.fail(format!("material name: {e}")))?;
    // The brows when they show through, and the face's overlay (sweat drop, gloom hatch), which is in front of the hair.
    let overlays = || {
        let mut overlays = Vec::with_capacity(2);
        if params.shading.brows_through {
            overlays.push(ctx.face_overlay(FaceLayerId::Brows)?);
        }
        overlays.push(ctx.face_overlay(FaceLayerId::Overlay)?);
        Ok(overlays)
    };
    let material = hair_material(ctx.part, params, ctx.palette, overlays)?;
    let mut out = PartOutput::new().with_materials(vec![(material_name.clone(), material)]).with_warnings(warnings);
    // Built for export, the simulated groups other than tails hang on strand chains of their own.
    let strands = if ctx.rig_export {
        bake::strand_chains(ctx.part, params, &entries, &chains).map_err(|e| ctx.fail(e))?
    } else {
        Vec::new()
    };
    // Static hair follows the head, except a tail's locks, which follow their chain's bones. Simulated hair is the
    // runtime's to move, unskinned in character space; an export build skins it as static hair is, for its spring
    // chains to move in other apps.
    for (simulated, built) in [(false, stat), (true, dynamic)] {
        let Some(Skinned { mut geometry, runs }) = built else { continue };
        let mesh = u16::try_from(out.meshes.len()).map_err(|_| ctx.fail("too many meshes".to_owned()))?;
        let runtime = simulated && !ctx.rig_export;
        let joint_bones = if runtime {
            geometry.joints.clear();
            geometry.weights.clear();
            let clumps: Vec<_> =
                entries.iter().filter(|e| e.simulate).flat_map(|e| e.group.clumps.iter().cloned()).collect();
            let colliders = physics::colliders::from_character(head.as_ref(), body.as_deref());
            // The head is the template's, scaled to the character's stature: that scale is its size against the
            // prototype's, whose head the chest-to-head band was set under (#898).
            let head_scale = ctx.published.get::<StatureFact>().map_or(1.0, |s| s.scale);
            let head_y = head.centre().y;
            let runtime = HairRuntime::new(&clumps, &params.physics, &colliders, head_y, head_scale, mesh, &geometry)
                .map_err(|e| ctx.fail(format!("hair physics: {e}")))?;
            out.runtime = Some(Box::new(runtime));
            Vec::new()
        } else {
            let mut runs = runs;
            if simulated {
                bind_to_strands(&geometry, &strands, &mut runs);
            }
            skin_runs(&mut geometry, &runs).map_err(|e| ctx.fail(e))?
        };
        out.meshes.push(PartMesh {
            name: mesh_name(ctx.part, simulated),
            geometry,
            joint_bones,
            material: material_name.clone(),
            casts_shadow: true,
            render_order: 1,
            dynamic: runtime,
        });
    }
    if let Some(mut geometry) = ties {
        // A band rides the chain it sits on (a braid's end band); one at the root stays with the head.
        let all: Vec<Chain> = chains.values().flatten().cloned().collect();
        let mut runs = Vec::new();
        bind_to_strands(&geometry, &all, &mut runs);
        let joint_bones = skin_runs(&mut geometry, &runs).map_err(|e| ctx.fail(e))?;
        let tie_name = tie_material_name(ctx.part).map_err(|e| ctx.fail(format!("material name: {e}")))?;
        out.materials.push((tie_name.clone(), tie_material(ctx.part, params, ctx.palette)?));
        out.meshes.push(PartMesh {
            name: ties_mesh_name(ctx.part),
            geometry,
            joint_bones,
            material: tie_name,
            casts_shadow: true,
            render_order: 2,
            dynamic: false,
        });
    }
    let colliders: Vec<String> = ctx.colliders().iter().map(|group| group.name.clone()).collect();
    tail::add_chains(strands, &colliders, &mut out.rig);
    let reach = HairReach::from_points(head.as_ref(), out.meshes.iter().flat_map(|m| &m.geometry.positions));
    out.publishes.put::<HairReachFact>(reach);
    publish_ties(params, ctx, head.as_ref(), &mut out)?;
    Ok(out)
}

/// Adds a run for every vertex of `mesh` no run holds yet, on the strand chain nearest it (none without strands).
fn bind_to_strands<'a>(mesh: &MeshData, strands: &'a [Chain], runs: &mut Vec<Run<'a>>) {
    if strands.is_empty() {
        return;
    }
    let mut held = vec![false; mesh.positions.len()];
    for run in runs.iter() {
        held[run.range.clone()].iter_mut().for_each(|h| *h = true);
    }
    let distance = |chain: &Chain, p: Vec3| {
        chain
            .rest
            .windows(2)
            .map(|w| {
                let d = w[1] - w[0];
                let t = if d.length_squared() > 0.0 {
                    ((p - w[0]).dot(d) / d.length_squared()).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                p.distance_squared(w[0] + d * t)
            })
            .fold(f32::INFINITY, f32::min)
    };
    for (v, p) in mesh.positions.iter().enumerate().filter(|(v, _)| !held[*v]) {
        let nearest = strands.iter().min_by(|a, b| distance(a, *p).total_cmp(&distance(b, *p)));
        if let Some(chain) = nearest {
            runs.push(Run { range: v..v + 1, chain });
        }
    }
}

/// Lifts every bangs group's locks off the head, measured in `frame`, the frame the tubes are drawn in. This runs here,
/// not in the generator, because the frame reads the torso as well as the head, and a lock cleared in the head's own
/// frame can still enter the skull in the drawn one (below the frame's blend height, 78 mm under the head's centre).
/// Only bangs groups are touched: the other generators' tubes are as their generators make them.
fn clear_fringes(
    params: &HairParams,
    head: &dyn omuzaki_char_api::HeadSurface,
    frame: &OutwardFrame,
    entries: &mut [CompiledEntry],
) -> Result<(), String> {
    for entry in entries {
        if let Some(group) = params.groups.get(&entry.id)
            && matches!(group.generator, Generator::Bangs(_))
        {
            bangs::keep_clear(head, frame, &mut entry.group.clumps)
                .map_err(|e| format!("the bangs group '{}': {e}", entry.id))?;
        }
    }
    Ok(())
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
    let body = ctx.fit().ok();
    for (id, group) in enabled(params) {
        let anchors = match &group.generator {
            Generator::Tail(g) => {
                let prefix = chain_prefix(ctx.part, id);
                tail::rig(g, head, body.as_deref(), &prefix, group.mirror, &colliders, &mut out.rig)
                    .map_err(|e| ctx.fail(format!("group '{id}': a tail bone name: {e}")))?;
                tail::anchors(g, head, group.mirror)
            }
            Generator::Ahoge(g) => {
                let chains = ahoge::chains(g, head, &chain_prefix(ctx.part, id), group.mirror)
                    .map_err(|e| ctx.fail(format!("group '{id}': an ahoge bone name: {e}")))?;
                tail::add_chains(chains, &colliders, &mut out.rig);
                continue;
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

/// Why the hair's meshes could not be written.
#[derive(Debug, thiserror::Error)]
enum MeshError {
    /// A lock's tube.
    #[error(transparent)]
    Tube(#[from] TubeError),
    /// The curtain's strips.
    #[error(transparent)]
    Strip(#[from] StripError),
}

/// The static and the dynamic mesh of the entries (`None` for a mesh with nothing in it): tubes first, in group order,
/// then strips, then shells. Pushes a warning for each group that made nothing.
fn mesh_groups<'a>(
    entries: &[CompiledEntry],
    frame: &OutwardFrame,
    keep_out: Option<&BodyKeepOut>,
    chains: &'a BTreeMap<GroupId, Vec<Chain>>,
    warnings: &mut Vec<String>,
) -> Result<Meshes<'a>, MeshError> {
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
            GroupGeometry::Strip { .. } | GroupGeometry::Shell(_) => {}
        }
    }
    for entry in entries {
        if let GroupGeometry::Strip { columns, u } = &entry.group.geometry {
            if entry.group.clumps.is_empty() {
                warnings.push(format!("the '{}' group made no clumps", entry.id));
                continue;
            }
            let strips = curtain::group_mesh(&entry.group.clumps, u, *columns, frame, keep_out)?;
            if strips.indices.is_empty() {
                warnings.push(format!(
                    "the '{}' group has {columns} columns, so there is nothing to draw between them",
                    entry.id
                ));
            }
            let target = if entry.simulate { &mut dynamic } else { &mut stat };
            if !target.append(&strips) {
                warnings.push(format!("the '{}' strips lack valid hair-uv and hair-info and were left out", entry.id));
            }
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
        let base = if let Some((_, base)) = bases.iter().find(|(name, _)| *name == run.chain.name) {
            *base
        } else {
            let base = joints.len();
            joints.extend(run.chain.bones.iter().cloned().map(JointRef::Named));
            bases.push((run.chain.name.as_str(), base));
            base
        };
        let index =
            |bone: usize| u16::try_from(base + bone).map_err(|_| "more joints than a mesh can index".to_owned());
        for v in run.range.clone() {
            let s = tail::skin(run.chain, mesh.positions[v]);
            let [head, here, next] = s.weights();
            mesh.joints[v] = [0, index(s.bone)?, index(s.bone + 1)?, 0];
            mesh.weights[v] = [head, here, next, 0.0];
        }
    }
    Ok(joints)
}

/// The chains of every enabled tail group and springy ahoge group, by group id, for the skin and the rig.
fn tail_chains(
    params: &HairParams,
    ctx: &BuildCtx<'_>,
    head: &dyn HeadSurface,
) -> Result<BTreeMap<GroupId, Vec<Chain>>, BuildError> {
    let mut out = BTreeMap::new();
    let body = ctx.fit().ok();
    for (id, group) in enabled(params) {
        let prefix = chain_prefix(ctx.part, id);
        let chains = match &group.generator {
            Generator::Tail(g) => tail::chains(g, head, body.as_deref(), &prefix, group.mirror)
                .map_err(|e| ctx.fail(format!("group '{id}': a tail bone name: {e}")))?,
            Generator::Ahoge(g) => ahoge::chains(g, head, &prefix, group.mirror)
                .map_err(|e| ctx.fail(format!("group '{id}': an ahoge bone name: {e}")))?,
            _ => continue,
        };
        if !chains.is_empty() {
            out.insert(id.clone(), chains);
        }
    }
    Ok(out)
}

/// The start of every bone and chain name of a tail group: `hair_<part>_<group id>`.
pub(crate) fn chain_prefix(part: &PartId, group: &GroupId) -> String {
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
    let colour =
        palette.resolve(&params.shading.tie).map_err(|error| BuildError::Colour { part: part.clone(), error })?;
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
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// How a clump is drawn, as a recolour sees it.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Drawn {
    /// A tube.
    Tube,
    /// A strip of this many columns, one clump's worth of a curtain.
    Strip(u8),
    /// No vertices of its own to tint (the cap's shell).
    Shell,
}

/// One clump of the hair as a recolour sees it: from the parameters alone.
struct Slot<'a> {
    group: &'a GroupId,
    simulate: bool,
    drawn: Drawn,
}

/// Every clump, in compile order.
fn layout<'a>(params: &'a HairParams, generators: &dyn GroupGenerator) -> Vec<Slot<'a>> {
    enabled(params)
        .flat_map(|(id, group)| {
            let drawn = match &group.generator {
                Generator::Cap(_) => Drawn::Shell,
                Generator::Curtain(g) => Drawn::Strip(g.columns),
                _ => Drawn::Tube,
            };
            let slot = Slot { group: id, simulate: group.dynamics.simulate, drawn };
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
        let expects_tubes = layout.iter().any(|s| s.drawn != Drawn::Shell && s.simulate == simulated);
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
        let mine: Vec<(Drawn, Option<Colour>)> = layout
            .iter()
            .zip(&tints)
            .filter(|(s, _)| s.drawn != Drawn::Shell && s.simulate == simulated)
            .map(|(s, t)| (s.drawn, *t))
            .collect();
        if !paint_clumps(&mut mesh.geometry, &mine) {
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

/// Gives the vertices of each clump of `mesh` the colour of its tint. The mesh holds the tubes first and then the
/// strips, each in the order of `clumps` (which mixes them in group order). Returns `false`, changing nothing, if the
/// mesh isn't those clumps followed by (at most) shells.
fn paint_clumps(mesh: &mut MeshData, clumps: &[(Drawn, Option<Colour>)]) -> bool {
    if mesh.colours.len() != mesh.positions.len() {
        return false;
    }
    let Some(ranges) = clump_ranges(mesh, clumps.iter().map(|c| c.0)) else { return false };
    for (range, (_, tint)) in ranges.into_iter().zip(clumps_in_mesh_order(clumps)) {
        mesh.colours[range].fill(vertex_colour(tint));
    }
    true
}

/// The clumps in the order the mesh holds them: every tube, then every strip.
fn clumps_in_mesh_order(clumps: &[(Drawn, Option<Colour>)]) -> impl Iterator<Item = (Drawn, Option<Colour>)> + '_ {
    let tubes = clumps.iter().filter(|c| c.0 == Drawn::Tube);
    let strips = clumps.iter().filter(|c| matches!(c.0, Drawn::Strip(_)));
    tubes.chain(strips).copied()
}

/// The vertex ranges of `clumps` in `mesh`, in mesh order (see [`clumps_in_mesh_order`]). A tube starts at a ring whose
/// `hair-uv.y` is 0 and ends after the ring where it reaches 1 (the last ring of every tube) and its two caps, which
/// take the tube's tint too. A strip is `columns` runs of vertices, each from `hair-uv.y` 0 to 1. `None` if the mesh
/// doesn't hold that many.
fn clump_ranges(mesh: &MeshData, clumps: impl Iterator<Item = Drawn> + Clone) -> Option<Vec<Range<usize>>> {
    let uv = &mesh.extra_attribute(HAIR_UV)?.data;
    let along = |vertex: usize| uv.get(2 * vertex + 1).copied();
    let tubes = clumps.clone().filter(|d| *d == Drawn::Tube);
    let strips = clumps.filter(|d| matches!(d, Drawn::Strip(_)));
    let mut ranges = Vec::new();
    let mut at = 0;
    for drawn in tubes.chain(strips) {
        let start = at;
        if along(at)? != 0.0 {
            return None;
        }
        // A tube's run of rings, or one of a strip's columns, ends at `hair-uv.y` 1.
        let runs = if let Drawn::Strip(columns) = drawn { usize::from(columns) } else { 1 };
        let step = if drawn == Drawn::Tube { RING } else { 1 };
        for _ in 0..runs {
            loop {
                let t = along(at)?;
                at += step;
                if t >= 1.0 {
                    break;
                }
            }
        }
        if drawn == Drawn::Tube {
            at += CAP_VERTICES;
        }
        ranges.push(start..at);
    }
    (at <= mesh.positions.len()).then_some(ranges)
}
