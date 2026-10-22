//! `mock-hair`: the real [`HairParams`] compiled to [`HairClump`]s and drawn as ribbons, with a simulation that
//! sways the simulated groups deterministically. It understands `cap`, `bangs` and `curtain` groups; other generators
//! are skipped with a warning (a stand-in is allowed to know less than the real thing).

use omuzaki_anim_api::{HumanoidBone, SpringChain, SpringParams, Transform};
use omuzaki_char_api::{
    BangLayer, BuildCtx, BuildError, Bulge, CurtainGen, DynamicMesh, ExtraBone, FaceOverlay, Generator, GroupId,
    GuidePoint, HairClump, HairGroup, HairParams, HairRng, HeadSurface, JointRef, LegacyLcg, MaterialSpec, MeshData,
    PartCategory, PartId, PartKind, PartMesh, PartMeta, PartOutput, PartRuntime, PoseSource, RecolourCtx, Recoloured,
    RigContribution, RuntimeCost, StepCtx, Taper, humanoid_id,
};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::Vec3;
use omuzaki_core::rng::Rng;
use omuzaki_face_api::{FaceLayerId, Palette, narrow};
use omuzaki_render_api::{AlphaMode, Outline};

use crate::{shapes, util};

/// Points per clump.
const POINTS: usize = 6;

/// One clump a group makes, before it has a position: which strand of which generator part it is. The single source
/// of a group's clump order and count: [`compile`] gives each its geometry, and a recolour (which has no head to
/// compile against) only needs to know how many there are.
enum Strand<'a> {
    /// Strand `k` of a curtain.
    Curtain(&'a CurtainGen, u16),
    /// Strand `k` of a bangs layer.
    Bangs(&'a BangLayer, u8),
}

/// The strands of a group, in the order they are compiled; groups of other generators make none.
fn strands(group: &HairGroup) -> Vec<Strand<'_>> {
    match &group.generator {
        Generator::Curtain(curtain) => (0..curtain.count).map(|k| Strand::Curtain(curtain, k)).collect(),
        Generator::Bangs(bangs) => {
            bangs.layers.iter().flat_map(|layer| (0..layer.count).map(move |k| Strand::Bangs(layer, k))).collect()
        }
        _ => Vec::new(),
    }
}

/// Compiles one enabled group into clumps. The curtain hangs straight down from the back of the head and the bangs fan
/// across the forehead; both follow the guide-point conventions in `omuzaki_char_api::hair`.
fn compile(gid: &GroupId, group: &HairGroup, head: &dyn HeadSurface) -> Vec<HairClump> {
    let dynamics = &group.dynamics;
    let clump = |rest: Vec<Vec3>, width, thickness, shade| HairClump {
        group: gid.clone(),
        rest,
        width,
        thickness,
        stiffness: dynamics.stiffness,
        shade,
        simulate: dynamics.simulate,
        follow: dynamics.follow,
        tint: None,
    };
    strands(group)
        .into_iter()
        .map(|strand| match strand {
            Strand::Curtain(curtain, k) => {
                let across = util::frac_usize(usize::from(k), usize::from(curtain.count.saturating_sub(1)));
                let phi = 180.0 + (2.0 * across - 1.0) * narrow(curtain.spread_deg);
                let backness = phi.to_radians().cos().abs();
                let (front, back) = (narrow(curtain.length_front), narrow(curtain.length_back));
                let length = front + (back - front) * backness;
                let root = GuidePoint::Surface([100.0, f64::from(phi), 0.02]).resolve(head);
                let rest = (0..POINTS).map(|i| root - Vec3::Y * (length * util::frac_usize(i, POINTS - 1))).collect();
                clump(rest, curtain.width, curtain.thickness, 1.0)
            }
            Strand::Bangs(layer, k) => {
                let across = util::frac_usize(usize::from(k), usize::from(layer.count.saturating_sub(1)));
                let phi = f64::from((2.0 * across - 1.0) * narrow(layer.spread_deg));
                let tip =
                    narrow(layer.tip_heights.get(usize::from(k)).or(layer.tip_heights.last()).copied().unwrap_or(0.0));
                let theta_tip = f64::from((90.0 - tip.atan2(head.radius(Vec3::Z)).to_degrees()).clamp(20.0, 90.0));
                let root = GuidePoint::Surface([14.0, phi, layer.root_offset]).resolve(head);
                let tip_point = GuidePoint::Surface([theta_tip, phi, layer.tip_offset]).resolve(head);
                let rest = (0..POINTS).map(|i| root.lerp(tip_point, util::frac_usize(i, POINTS - 1))).collect();
                let width =
                    Taper { start: layer.width * 0.6, mid: layer.width, tip: 0.002, tip_at: 0.5, ..Taper::default() };
                clump(rest, width, Bulge::default(), narrow(layer.shade))
            }
        })
        .collect()
}

/// The part's random draws, in authoring order: one strand seed per clump, then one sway phase per simulated clump. A
/// forked part draws from its own stream; `LegacyLcg` draws from the prototype's generator, which every group shares
/// (parity mode, see [`HairRng`]).
fn draws(rng: HairRng, forked: &mut Rng, clumps: &[HairClump]) -> Result<(Vec<f32>, Vec<f32>), String> {
    let mut legacy = match rng {
        HairRng::Forked => None,
        HairRng::LegacyLcg { seed } => Some(LegacyLcg::new(seed).ok_or_else(|| format!("invalid legacy seed {seed}"))?),
    };
    let mut next = || match legacy.as_mut() {
        Some(lcg) => narrow(lcg.next_unit()),
        None => forked.next_f32(),
    };
    let seeds = clumps.iter().map(|_| next()).collect();
    let phases = clumps.iter().filter(|c| c.simulate).map(|_| next() * std::f32::consts::TAU).collect();
    Ok((seeds, phases))
}

/// Vertices a clump adds to its mesh: two per rest point.
const CLUMP_VERTICES: usize = 2 * POINTS;

/// The vertex colour of a clump: its tint, or white where it is untinted, so untinted hair renders exactly as it did
/// before tints existed.
fn vertex_colour(tint: Option<Colour>) -> [f32; 4] {
    tint.map_or([1.0; 4], |c| {
        let linear = c.to_linear();
        [linear.x, linear.y, linear.z, 1.0]
    })
}

/// Gives the vertices a clump has just added to `mesh` (from index `from`) its vertex colour.
fn paint(mesh: &mut MeshData, clump: &HairClump, from: usize) {
    mesh.colours.resize(from, [1.0; 4]);
    mesh.colours.resize(mesh.positions.len(), vertex_colour(clump.tint));
}

/// Every clump of the hair, in the order [`compile`] makes them ([`strands`]): its group, and whether it is simulated (which mesh it
/// lands in). Streaks pick clumps by this order.
fn clump_layout(params: &HairParams) -> Vec<(&GroupId, bool)> {
    params
        .groups
        .iter()
        .filter(|(_, group)| group.enabled)
        .flat_map(|(id, group)| std::iter::repeat_n((id, group.dynamics.simulate), strands(group).len()))
        .collect()
}

/// The hair material. Every colour it takes comes from `palette`, and `build` and `recolour` share this one function so
/// they cannot drift apart. `overlays` is asked for after the shading, which is the order `build` always had, so the
/// first error is the same.
fn hair_material(
    part: &PartId,
    params: &HairParams,
    palette: &Palette,
    overlays: impl FnOnce() -> Result<Vec<FaceOverlay>, BuildError>,
) -> Result<MaterialSpec, BuildError> {
    let shading =
        params.shading.shading(palette).map_err(|e| BuildError::Part { part: part.clone(), message: e.to_string() })?;
    let overlays = overlays()?;
    let line =
        palette.resolve(&params.shading.line).map_err(|error| BuildError::Colour { part: part.clone(), error })?;
    Ok(MaterialSpec {
        base: Colour::WHITE,
        base_texture: None,
        shading,
        overlays,
        alpha: AlphaMode::Opaque,
        double_sided: true,
        vertex_colours: true,
        outline: Some(Outline { colour: line, width: narrow(params.shading.outline_width) }),
        receives_shadows: true,
        uv_transform: omuzaki_render_api::UvTransform::IDENTITY,
    })
}

/// The simulation: a standing sway of simulated clumps, stiff at the root and loose at the tip, from the shot's clock.
struct Sway {
    rest: Vec<Vec3>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    /// Per vertex: how much it moves (the clump's stiffness falloff, inverted) and its strand's phase.
    free: Vec<f32>,
    phase: Vec<f32>,
    revision: u64,
    mesh: u16,
}

impl PartRuntime for Sway {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        #[allow(clippy::cast_possible_truncation)] // the shot clock stays far below f32 range
        let t = ctx.time.0 as f32;
        for (i, p) in self.positions.iter_mut().enumerate() {
            let swing = 0.02 * self.free[i] * (t * 2.0 + self.phase[i]).sin();
            *p = self.rest[i] + Vec3::new(swing, 0.0, 0.5 * swing) + ctx.wind * (0.01 * self.free[i]);
        }
        self.revision += 1;
    }

    fn reset(&mut self, _: &dyn PoseSource) {
        self.positions.copy_from_slice(&self.rest);
        self.revision += 1;
    }

    fn dynamic_count(&self) -> usize {
        1
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        (index == 0).then_some(DynamicMesh {
            mesh: self.mesh,
            positions: &self.positions,
            normals: &self.normals,
            revision: self.revision,
        })
    }

    fn cost(&self) -> RuntimeCost {
        let n = u32::try_from(self.positions.len()).unwrap_or(u32::MAX);
        RuntimeCost { particles: n / 2, colliders: 0, vertices: n }
    }

    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        // The initial state is the rest shape at revision 0, whatever this runtime has done since. (`normals`, `free`,
        // `phase` and `mesh` never change after the build.)
        Some(Box::new(Self {
            rest: self.rest.clone(),
            positions: self.rest.clone(),
            normals: self.normals.clone(),
            free: self.free.clone(),
            phase: self.phase.clone(),
            revision: 0,
            mesh: self.mesh,
        }))
    }

    fn heap_bytes(&self) -> usize {
        let vectors = self.rest.len() + self.positions.len() + self.normals.len();
        vectors * std::mem::size_of::<Vec3>() + (self.free.len() + self.phase.len()) * std::mem::size_of::<f32>()
    }
}

/// How far each bone of the mock's one spring chain hangs below the first, metres.
const CHAIN_DROPS: [f32; 3] = [0.0, 0.06, 0.12];

/// One chain of three bones hanging from the back of the head, colliding with every group published so far (the
/// body's head and neck). Real hair makes a chain per simulated clump; the mock only shows the contract.
fn spring_chain(ctx: &BuildCtx<'_>, head: &dyn HeadSurface) -> Result<RigContribution, BuildError> {
    let mut rig = RigContribution::default();
    let root = head.point(-Vec3::Z, -0.01);
    let (mut parent, mut ids) = (JointRef::HEAD, Vec::new());
    for (i, drop) in CHAIN_DROPS.into_iter().enumerate() {
        let name = util::bone(ctx, &format!("hair_{}_{i}", ctx.part))?;
        let rest = Transform::from_translation(root - Vec3::Y * drop);
        ids.push(rig.add_bone(ExtraBone { name: name.clone(), parent, rest }));
        parent = JointRef::Named(name);
    }
    let (stiff, loose) = (
        SpringParams { stiffness: 1.0, gravity_power: 0.1, hit_radius: 0.01, ..SpringParams::default() },
        SpringParams { stiffness: 0.2, gravity_power: 0.3, hit_radius: 0.01, ..SpringParams::default() },
    );
    let mut chain = SpringChain::tapered(format!("hair_{}", ctx.part), &ids, stiff, loose);
    chain.center = Some(humanoid_id(HumanoidBone::Head));
    chain.collider_groups = ctx.colliders().iter().map(|g| g.name.clone()).collect();
    rig.chains.push(chain);
    Ok(rig)
}

/// The `mock-hair` kind.
pub struct MockHair;

impl PartKind for MockHair {
    type Params = HairParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "mock-hair",
            summary: "Build ribbon hair from the hairstyle's groups.",
            category: PartCategory::Hair,
            requires: &["mock-head"],
        }
    }

    fn build(params: &HairParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let head = ctx.head()?;
        let (mut stat, mut dynamic) = (
            shapes::hair_mesh().map_err(|e| ctx.fail(e.to_string()))?,
            shapes::hair_mesh().map_err(|e| ctx.fail(e.to_string()))?,
        );
        let (mut clumps, mut warnings) = (Vec::new(), Vec::new());
        for (id, group) in params.groups.iter().filter(|(_, g)| g.enabled) {
            let compiled = compile(id, group, head.as_ref());
            if compiled.is_empty() {
                warnings.push(format!("the mock hair doesn't build the '{id}' group"));
            }
            clumps.extend(compiled);
        }
        params.shading.apply_to_clumps(ctx.palette, &mut clumps).map_err(|e| ctx.fail(e.to_string()))?;
        let (seeds, phases) = draws(params.rng, &mut ctx.rng, &clumps).map_err(|e| ctx.fail(e))?;
        for (clump, seed) in clumps.iter().zip(&seeds) {
            let target = if clump.simulate { &mut dynamic } else { &mut stat };
            let first_vertex = target.positions.len();
            shapes::ribbon(clump, *seed, head.centre(), target);
            paint(target, clump, first_vertex);
        }
        let material_id = util::material(ctx, "mock-hair")?;
        let overlays = || {
            if params.shading.brows_through { Ok(vec![ctx.face_overlay(FaceLayerId::Brows)?]) } else { Ok(Vec::new()) }
        };
        let material = hair_material(ctx.part, params, ctx.palette, overlays)?;
        let mut out = PartOutput::new()
            .with_materials(vec![(material_id.clone(), material)])
            .with_rig(spring_chain(ctx, head.as_ref())?)
            .with_warnings(warnings);
        let mesh = |name: &str, geometry: MeshData, dynamic: bool| PartMesh {
            name: name.to_owned(),
            geometry,
            joint_bones: Vec::new(),
            material: material_id.clone(),
            casts_shadow: true,
            render_order: 1,
            dynamic,
        };
        if !stat.positions.is_empty() {
            out.meshes.push(mesh("hair-static", stat, false));
        }
        if !dynamic.positions.is_empty() {
            let free: Vec<f32> = clumps
                .iter()
                .filter(|c| c.simulate)
                .flat_map(|c| (0..POINTS).flat_map(move |i| [1.0 - c.stiffness.at(util::frac_usize(i, POINTS - 1)); 2]))
                .collect();
            // One phase per simulated clump, repeated for each of its vertices.
            let phase = phases.iter().flat_map(|p| [*p; 2 * POINTS]).collect();
            out.runtime = Some(Box::new(Sway {
                rest: dynamic.positions.clone(),
                positions: dynamic.positions.clone(),
                normals: dynamic.normals.clone(),
                free,
                phase,
                revision: 0,
                mesh: u16::try_from(out.meshes.len()).map_err(|_| ctx.fail("too many meshes"))?,
            }));
            out.meshes.push(mesh("hair-dynamic", dynamic, true));
        }
        Ok(out)
    }

    /// Re-tints the vertex colours of the streaked clumps and re-resolves the material: no clump is compiled and no
    /// ribbon drawn. The streak tints come from the groups alone ([`HairShading::streak_tints`]), and a clump's vertices
    /// follow from the order clumps were made in.
    ///
    /// [`HairShading::streak_tints`]: omuzaki_char_api::HairShading::streak_tints
    fn recolour(params: &HairParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        let layout = clump_layout(params);
        let groups: Vec<&GroupId> = layout.iter().map(|(group, _)| *group).collect();
        let tints = params.shading.streak_tints(ctx.palette, &groups).map_err(|e| ctx.fail(e.to_string()))?;
        for mesh in &mut output.meshes {
            let mine = || layout.iter().zip(&tints).filter(|((_, simulate), _)| *simulate == mesh.dynamic);
            if mesh.geometry.colours.len() != mine().count() * CLUMP_VERTICES {
                return Ok(Recoloured::NeedsRebuild); // not the hair `build` makes: don't guess
            }
            let tints = mine().map(|(_, tint)| *tint).collect::<Vec<_>>();
            for (vertices, tint) in mesh.geometry.colours.as_chunks_mut::<CLUMP_VERTICES>().0.iter_mut().zip(tints) {
                vertices.fill(vertex_colour(tint));
            }
        }
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = hair_material(ctx.part, params, ctx.palette, || Ok(std::mem::take(&mut material.overlays)))?;
        Ok(Recoloured::Done)
    }
}

omuzaki_char_api::register_part_kind!(MockHair);
