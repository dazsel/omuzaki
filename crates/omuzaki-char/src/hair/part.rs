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
//! **Not drawn yet.** The curtain's strip geometry is the curtain generator's and the runtime's: a group
//! that compiles to a strip reports a warning instead of a mesh.

use std::ops::Range;

use omuzaki_char_api::{
    BodyRegion, BuildCtx, BuildError, FaceOverlay, Generator, GroupId, HAIR_UV, HairParams, JointRef, KindProvenance,
    MaterialName, MaterialSpec, PartCategory, PartId, PartKind, PartMesh, PartMeta, PartOutput, RecolourCtx,
    Recoloured,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_face_api::{FaceLayerId, Palette, narrow};
use omuzaki_render_api::{AlphaMode, Outline};

use super::compile::{self, CompiledEntry, Dispatch, GroupGenerator, GroupGeometry, draws_tubes, enabled};
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
    let (stat, dynamic) = mesh_groups(&entries, &frame, &mut warnings).map_err(|e| ctx.fail(e.to_string()))?;

    let material_name =
        MaterialName::new(format!("{}-hair", ctx.part)).map_err(|e| ctx.fail(format!("material name: {e}")))?;
    let overlays = || {
        if params.shading.brows_through { Ok(vec![ctx.face_overlay(FaceLayerId::Brows)?]) } else { Ok(Vec::new()) }
    };
    let material = hair_material(ctx.part, params, ctx.palette, overlays)?;
    let mut out = PartOutput::new().with_materials(vec![(material_name.clone(), material)]).with_warnings(warnings);
    // Both meshes are rigid to the head: the simulated one only until the runtime moves it (see the module doc).
    let part_mesh = |simulated: bool, mut geometry: MeshData| {
        skin_to_head(&mut geometry);
        PartMesh {
            name: mesh_name(ctx.part, simulated),
            geometry,
            joint_bones: vec![JointRef::HEAD],
            material: material_name.clone(),
            casts_shadow: true,
            render_order: 1,
            dynamic: false,
        }
    };
    if let Some(geometry) = stat {
        out.meshes.push(part_mesh(false, geometry));
    }
    if let Some(geometry) = dynamic {
        out.meshes.push(part_mesh(true, geometry));
    }
    Ok(out)
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
fn mesh_groups(
    entries: &[CompiledEntry],
    frame: &OutwardFrame,
    warnings: &mut Vec<String>,
) -> Result<(Option<MeshData>, Option<MeshData>), TubeError> {
    let (mut stat, mut dynamic) = (TubeMesh::new(), TubeMesh::new());
    let (mut stat_clumps, mut dynamic_clumps) = (0usize, 0usize);
    for entry in entries {
        match &entry.group.geometry {
            GroupGeometry::Tube => {
                let (target, drawn) =
                    if entry.simulate { (&mut dynamic, &mut dynamic_clumps) } else { (&mut stat, &mut stat_clumps) };
                for clump in &entry.group.clumps {
                    let polar = polar_angles(&clump.rest, frame.head);
                    let look = TubeLook {
                        seed: strand_seed(*drawn, entry.simulate),
                        shade: clump.shade,
                        polar: &polar,
                        tint: clump.tint,
                        flip: 1.0,
                    };
                    target.push(&clump.rest, &clump.width, &clump.thickness, |p| frame.rest(p), &look)?;
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
    Ok((finish(stat)?, finish(dynamic)?))
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
    for mesh in &mut output.meshes {
        // Found by name: a mesh's `dynamic` flag is not the simulated groups' mark while no runtime moves them.
        let simulated = mesh.name == mesh_name(ctx.part, true);
        let mine = layout.iter().zip(&tints).filter(|(s, _)| s.tube && s.simulate == simulated).map(|(_, t)| *t);
        if !paint_tubes(&mut mesh.geometry, &mine.collect::<Vec<_>>()) {
            return Ok(Recoloured::NeedsRebuild); // not the hair `build` makes: don't guess
        }
    }
    let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
    *material = hair_material(ctx.part, params, ctx.palette, || Ok(std::mem::take(&mut material.overlays)))?;
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
