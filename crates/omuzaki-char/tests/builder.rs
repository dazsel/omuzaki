//! The caching builder against the reference builder: 50 seeded edit sequences must give byte-identical characters (or
//! the same error), a cached build must rebuild exactly the parts an edit reaches, and no thread count, run or cache
//! limit may change a single byte.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // small loop counters become floats

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::{self, Debug, Write as _};
use std::hash::{DefaultHasher, Hasher};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[path = "common/palette_facts.rs"]
mod palette_facts;

use omuzaki_char::builder::PartSource::{Built, Cached};
use omuzaki_char::builder::{BuildTimings, CachingBuilder};
use omuzaki_char_api::{
    Anchor, AnchorId, AnchorsFact, Attach, AttachOffset, BrushFalloff, BuildCtx, BuildError, BuildOptions,
    BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, CollidersFact, ContentHash, DynamicMesh,
    FitFact, HeadFact, JointRef, Last, MaterialName, MaterialSpec, MeshData, PartCategory, PartDef, PartId, PartKind,
    PartKindId, PartMesh, PartMeta, PartOutput, PartRuntime, PoseSource, Publishes, Quality, RecolourCtx, Recoloured,
    RegistryBuilder, RuntimeCost, StepCtx, TextureData, TextureName, VariantId, VariantSet, WeightBrush, find_kind,
    resolve_variant,
};
use omuzaki_char_mock::{MemAssets, RestPose, mock_character, mock_variants};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Mat4, UVec2, Vec2, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_face_api::{Check, ColourRef, DrawList, FaceRect, Paint, PaletteSlot, Validate};
use omuzaki_face_mock::MockRasterizer;
use omuzaki_render_api::TextureDesc;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ test part kinds

fn grid_mesh(name: &str, material: &MaterialName, cells: u32, rng: &mut Rng) -> PartMesh {
    let side = cells + 1;
    let mut positions = Vec::new();
    for j in 0..side {
        for i in 0..side {
            positions.push(Vec3::new(i as f32 * 0.01, j as f32 * 0.01, rng.range_f32(-0.001, 0.001)));
        }
    }
    let mut indices = Vec::new();
    for j in 0..cells {
        for i in 0..cells {
            let a = j * side + i;
            indices.extend([a, a + 1, a + side, a + 1, a + side + 1, a + side]);
        }
    }
    let normals = vec![Vec3::Z; positions.len()];
    PartMesh {
        name: name.to_owned(),
        geometry: MeshData { positions, normals, indices, ..MeshData::default() },
        joint_bones: Vec::new(),
        material: material.clone(),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    }
}

fn material_name(ctx: &BuildCtx<'_>, prefix: &str) -> Result<MaterialName, BuildError> {
    MaterialName::new(format!("{prefix}-{}", ctx.part)).map_err(|e| ctx.fail(e.to_string()))
}

/// What the kinds below did for one test. The tests of this file share a process and run concurrently, so a test that
/// counts gives its parts a `tag` of its own; parts with the empty tag (the sequences) are not counted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Counts {
    /// Generators run (geometry made).
    builds: usize,
    /// Recolours run.
    recolours: usize,
    /// Textures painted, by a build or a recolour.
    paints: usize,
}

static COUNTS: Mutex<BTreeMap<String, Counts>> = Mutex::new(BTreeMap::new());

fn count(tag: &str, bump: impl FnOnce(&mut Counts)) {
    if !tag.is_empty() {
        bump(COUNTS.lock().unwrap().entry(tag.to_owned()).or_default());
    }
}

fn counts(tag: &str) -> Counts {
    COUNTS.lock().unwrap().get(tag).copied().unwrap_or_default()
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct PropParams {
    rings: u32,
    tint: ColourRef,
    tag: String,
}

impl Default for PropParams {
    fn default() -> Self {
        Self { rings: 6, tint: ColourRef::slot_or("outfit", ColourRef::rgb(200, 200, 200)), tag: String::new() }
    }
}

impl Validate for PropParams {
    fn check(&self, c: &mut Check) {
        c.range("rings", f64::from(self.rings), 2.0, 64.0);
    }
}

fn build_prop(
    prefix: &str,
    extra_cells: u32,
    params: &PropParams,
    ctx: &mut BuildCtx<'_>,
) -> Result<PartOutput, BuildError> {
    count(&params.tag, |c| c.builds += 1);
    let id = material_name(ctx, prefix)?;
    let mesh = grid_mesh("prop", &id, params.rings + extra_cells, &mut ctx.rng);
    let material = MaterialSpec::toon(ctx.colour(&params.tint)?);
    Ok(PartOutput { meshes: vec![mesh], materials: vec![(id, material)], ..PartOutput::default() })
}

/// A silent part: a grid whose jitter comes from the part's stream and whose colour from a palette slot. Its colour
/// reaches the output only through its material, so it recolours by re-resolving it.
struct Prop;

impl PartKind for Prop {
    type Params = PropParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-prop", summary: "A jittered grid.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &PropParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_prop("t-prop", 0, params, ctx)
    }

    fn recolour(params: &PropParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        count(&params.tag, |c| c.recolours += 1);
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        *material = MaterialSpec::toon(ctx.colour(&params.tint)?);
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Prop);

/// A second kind with the same parameter type and a different mesh: swapping a part from one to the other keeps its
/// parameters (and so everything but the kind) identical. It doesn't recolour, so a palette change rebuilds it.
struct PropB;

impl PartKind for PropB {
    type Params = PropParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "t-prop-b",
            summary: "A larger jittered grid.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(params: &PropParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        build_prop("t-prop-b", 3, params, ctx)
    }
}
omuzaki_char_api::register_part_kind!(PropB);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct LampParams {
    publish: bool,
}

impl Validate for LampParams {
    fn check(&self, _: &mut Check) {}
}

/// A part whose output depends on everything published before it, on the face profile and on `rig_export`, and that
/// publishes an anchor only when asked to: the guess that it is silent can be wrong, and a stale view changes its mesh.
struct Lamp;

impl PartKind for Lamp {
    type Params = LampParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "t-lamp",
            summary: "Reads the published view.",
            category: PartCategory::Accessory,
            requires: &[],
        }
    }

    fn build(params: &LampParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let p = ctx.published;
        let seen = p.get::<AnchorsFact>().len()
            + p.get::<FitFact>().fits.len()
            + p.get::<CollidersFact>().len()
            + usize::from(p.get::<HeadFact>().is_some());
        let id = material_name(ctx, "t-lamp")?;
        let mut mesh = grid_mesh("lamp", &id, 2 + seen as u32, &mut ctx.rng);
        mesh.render_order = (ctx.face.layout.mouth_y * 1000.0) as i32; // parts read the face profile
        let material = MaterialSpec::toon(Colour::rgb(seen as u8 * 20, 0, 0));
        let anchor =
            Anchor { bone: JointRef::HEAD, position: Vec3::ZERO, rotation: omuzaki_core::glam::Quat::IDENTITY };
        let mut publishes = Publishes::default();
        if params.publish {
            publishes.put::<AnchorsFact>((AnchorId::new("light").unwrap(), anchor));
        }
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, material)],
            publishes,
            warnings: if ctx.rig_export { vec!["built for export".to_owned()] } else { Vec::new() },
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(Lamp);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct Empty {}

impl Validate for Empty {
    fn check(&self, _: &mut Check) {}
}

static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

/// A silent part that takes 30 ms and counts how many are building at once.
struct Sleeper;

impl PartKind for Sleeper {
    type Params = Empty;

    fn meta() -> PartMeta {
        PartMeta { id: "t-sleeper", summary: "Slow.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(_: &Empty, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let now = IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
        PEAK.fetch_max(now, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(30));
        IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
        let id = material_name(ctx, "t-sleeper")?;
        let mesh = grid_mesh("sleeper", &id, 2, &mut ctx.rng);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, MaterialSpec::toon(Colour::WHITE))],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(Sleeper);

/// A part that always names the same material: two of them in one character is a `Duplicate` error at the merge.
struct Twin;

impl PartKind for Twin {
    type Params = Empty;

    fn meta() -> PartMeta {
        PartMeta { id: "t-twin", summary: "Collides.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(_: &Empty, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let id = MaterialName::new("twin").map_err(|e| ctx.fail(e.to_string()))?;
        let mesh = grid_mesh("twin", &id, 2, &mut ctx.rng);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, MaterialSpec::toon(Colour::WHITE))],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(Twin);

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct StripeParams {
    asset: String,
    /// A palette colour for the material; without one the colour comes from the asset.
    tint: Option<ColourRef>,
}

impl Validate for StripeParams {
    fn check(&self, _: &mut Check) {}
}

/// A part that reads an asset and paints a texture, so it needs both build services.
struct Stripe;

impl PartKind for Stripe {
    type Params = StripeParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-stripe", summary: "Uses the services.", category: PartCategory::Garment, requires: &[] }
    }

    fn build(params: &StripeParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let hash: ContentHash = params.asset.parse().map_err(|e| ctx.fail(format!("{e}")))?;
        let bytes = ctx.asset_or_fail(&hash)?;
        let colour = Colour::rgb(bytes.first().copied().unwrap_or(0), 0, 0);
        let rect = FaceRect { x0: 0.0, x1: 0.08, y0: 0.0, y1: 0.08 };
        let mut list = DrawList::new();
        let pts = [Vec2::new(0.0, 0.0), Vec2::new(0.04, 0.0), Vec2::new(0.04, 0.08), Vec2::new(0.0, 0.08)];
        let path = list.path().polyline(&pts).close().finish();
        list.fill(path, Paint::Solid(colour));
        let texture: TextureData = ctx.paint_texture(UVec2::splat(64), rect, &list, TextureDesc::colour(UVec2::ONE))?;
        let (id, tex) = (material_name(ctx, "t-stripe")?, TextureName::new("t-stripe-tex").unwrap());
        let mut material =
            MaterialSpec::toon(params.tint.as_ref().map(|t| ctx.colour(t)).transpose()?.unwrap_or(colour));
        material.base_texture = Some(tex.clone());
        let mesh = grid_mesh("stripe", &id, 2, &mut ctx.rng);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, material)],
            textures: vec![(tex, texture)],
            ..PartOutput::default()
        })
    }

    /// Only the material follows the palette; the texture comes from the asset, which a recolour does not read again.
    fn recolour(
        params: &StripeParams,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        let [(_, material)] = output.materials.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        if let Some(tint) = &params.tint {
            material.base = ctx.colour(tint)?;
        }
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Stripe);

/// How a [`Wobble`] answers [`PartRuntime::fork`].
#[derive(Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Forks {
    /// Always: the contract.
    #[default]
    Always,
    /// Never: the trait's default, "not copyable".
    Never,
    /// Once, then never: breaks the contract's "consistent", to check a builder is not fooled by it.
    Once,
}

#[derive(Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct WobbleParams {
    forks: Forks,
    amplitude: f32,
}

impl Validate for WobbleParams {
    fn check(&self, c: &mut Check) {
        c.range("amplitude", f64::from(self.amplitude), 0.0, 1.0);
    }
}

/// How many times each `t-wobble` part's generator has run, by part id: a count the builder cannot influence. Tests use
/// part ids of their own, because tests run side by side.
static GENERATED: Mutex<BTreeMap<String, usize>> = Mutex::new(BTreeMap::new());

fn generated(part: &str) -> usize {
    GENERATED.lock().unwrap().get(part).copied().unwrap_or(0)
}

/// A simulation with state beyond what it exposes (a velocity), standing in for hair, cloth and tails.
#[derive(Clone)]
struct Sim {
    rest: Vec<Vec3>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    velocity: Vec<Vec3>,
    revision: u64,
    amplitude: f32,
    forks: Forks,
    /// Shared by a runtime and its copies, so that [`Forks::Once`] counts across them.
    asked: Arc<AtomicUsize>,
}

impl PartRuntime for Sim {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        let swing = (ctx.time.0 as f32).sin();
        for (p, v) in self.positions.iter_mut().zip(&mut self.velocity) {
            *v = *v * 0.9 + (Vec3::splat(swing) + ctx.wind) * (self.amplitude * ctx.dt);
            *p += *v;
        }
        self.revision += 1;
    }

    fn reset(&mut self, _: &dyn PoseSource) {
        self.velocity.fill(Vec3::ZERO);
        self.revision += 1;
    }

    fn dynamic_count(&self) -> usize {
        1
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        (index == 0).then_some(DynamicMesh {
            mesh: 0,
            positions: &self.positions,
            normals: &self.normals,
            revision: self.revision,
        })
    }

    fn cost(&self) -> RuntimeCost {
        RuntimeCost { particles: self.positions.len() as u32, colliders: 0, vertices: self.positions.len() as u32 }
    }

    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        let first = self.asked.fetch_add(1, Ordering::SeqCst) == 0;
        let copy = || {
            // The initial state, whatever has happened to `self`.
            let mut initial = self.clone();
            initial.positions.clone_from(&self.rest);
            initial.velocity.fill(Vec3::ZERO);
            initial.revision = 0;
            Box::new(initial) as Box<dyn PartRuntime>
        };
        match self.forks {
            Forks::Always => true,
            Forks::Never => false,
            Forks::Once => first,
        }
        .then(copy)
    }

    fn heap_bytes(&self) -> usize {
        // rest, positions, normals and velocity.
        4 * self.positions.len() * std::mem::size_of::<Vec3>()
    }
}

/// A silent part with a simulation, counting its generator runs.
struct Wobble;

impl PartKind for Wobble {
    type Params = WobbleParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-wobble", summary: "A simulated mesh.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &WobbleParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        *GENERATED.lock().unwrap().entry(ctx.part.to_string()).or_default() += 1;
        let id = material_name(ctx, "t-wobble")?;
        let mut mesh = grid_mesh("wobble", &id, 6, &mut ctx.rng);
        mesh.dynamic = true;
        let positions = mesh.geometry.positions.clone();
        let sim = Sim {
            normals: mesh.geometry.normals.clone(),
            velocity: vec![Vec3::ZERO; positions.len()],
            rest: positions.clone(),
            positions,
            revision: 0,
            amplitude: params.amplitude,
            forks: params.forks,
            asked: Arc::new(AtomicUsize::new(0)),
        };
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, MaterialSpec::toon(Colour::WHITE))],
            runtime: Some(Box::new(sim)),
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(Wobble);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct DyedParams {
    rings: u32,
    root: ColourRef,
    tip: ColourRef,
    tag: String,
}

impl Default for DyedParams {
    fn default() -> Self {
        Self {
            rings: 8,
            root: ColourRef::slot_or("hair_root", ColourRef::rgb(90, 60, 40)),
            tip: ColourRef::slot_or("trim", ColourRef::rgb(230, 230, 230)),
            tag: String::new(),
        }
    }
}

impl Validate for DyedParams {
    fn check(&self, c: &mut Check) {
        c.range("rings", f64::from(self.rings), 2.0, 64.0);
    }
}

/// A gradient from `root` at the bottom of the mesh to `tip` at the top, in linear light, as vertex colours.
fn gradient(positions: &[Vec3], root: Colour, tip: Colour) -> Vec<[f32; 4]> {
    let top = positions.iter().map(|p| p.y).fold(f32::EPSILON, f32::max);
    let (root, tip) = (root.to_linear(), tip.to_linear());
    positions
        .iter()
        .map(|p| {
            let c = root.lerp(tip, (p.y / top).clamp(0.0, 1.0));
            [c.x, c.y, c.z, 1.0]
        })
        .collect()
}

/// Stands in for hair: a part whose palette colours are baked into its vertex colours, with no simulation (so it is
/// cached today). It recolours by rewriting the colours; the positions it reads are not touched.
struct Dyed;

impl PartKind for Dyed {
    type Params = DyedParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-dyed", summary: "Bakes colours into vertices.", category: PartCategory::Hair, requires: &[] }
    }

    fn build(params: &DyedParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        count(&params.tag, |c| c.builds += 1);
        let id = material_name(ctx, "t-dyed")?;
        let mut mesh = grid_mesh("dyed", &id, params.rings, &mut ctx.rng);
        mesh.geometry.colours = gradient(&mesh.geometry.positions, ctx.colour(&params.root)?, ctx.colour(&params.tip)?);
        let mut material = MaterialSpec::toon(Colour::WHITE);
        material.vertex_colours = true;
        Ok(PartOutput { meshes: vec![mesh], materials: vec![(id, material)], ..PartOutput::default() })
    }

    fn recolour(params: &DyedParams, output: &mut PartOutput, ctx: &RecolourCtx<'_>) -> Result<Recoloured, BuildError> {
        count(&params.tag, |c| c.recolours += 1);
        let [mesh] = output.meshes.as_mut_slice() else { return Ok(Recoloured::NeedsRebuild) };
        let colours = gradient(&mesh.geometry.positions, ctx.colour(&params.root)?, ctx.colour(&params.tip)?);
        if colours.len() != mesh.geometry.colours.len() {
            return Ok(Recoloured::NeedsRebuild);
        }
        mesh.geometry.colours = colours;
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Dyed);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct PlaidParams {
    size: u32,
    cloth: ColourRef,
    stripe: ColourRef,
    tag: String,
}

impl Default for PlaidParams {
    fn default() -> Self {
        Self {
            size: 64,
            cloth: ColourRef::slot_or("cloth", ColourRef::rgb(200, 190, 170)),
            stripe: ColourRef::slot_or("trim", ColourRef::rgb(120, 40, 40)),
            tag: String::new(),
        }
    }
}

impl Validate for PlaidParams {
    fn check(&self, c: &mut Check) {
        c.range("size", f64::from(self.size), 8.0, 512.0);
    }
}

const PLAID_RECT: FaceRect = FaceRect { x0: 0.0, x1: 0.08, y0: 0.0, y1: 0.08 };

/// A cloth ground with four stripes.
fn plaid_list(cloth: Colour, stripe: Colour) -> DrawList {
    let mut list = DrawList::new();
    let mut fill = |x0: f32, x1: f32, colour: Colour| {
        let pts = [Vec2::new(x0, 0.0), Vec2::new(x1, 0.0), Vec2::new(x1, 0.08), Vec2::new(x0, 0.08)];
        let path = list.path().polyline(&pts).close().finish();
        list.fill(path, Paint::Solid(colour));
    };
    fill(0.0, 0.08, cloth);
    for k in 0..4 {
        let x = 0.005 + 0.02 * k as f32;
        fill(x, x + 0.008, stripe);
    }
    list
}

/// Stands in for a garment: a procedural texture painted through the injected rasteriser from two palette colours. It
/// recolours by repainting the texture at the size it has; the mesh is not touched.
struct Plaid;

impl PartKind for Plaid {
    type Params = PlaidParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-plaid", summary: "Paints cloth.", category: PartCategory::Garment, requires: &[] }
    }

    fn build(params: &PlaidParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        count(&params.tag, |c| {
            c.builds += 1;
            c.paints += 1;
        });
        let (cloth, stripe) = (ctx.colour(&params.cloth)?, ctx.colour(&params.stripe)?);
        let list = plaid_list(cloth, stripe);
        let texture =
            ctx.paint_texture(UVec2::splat(params.size), PLAID_RECT, &list, TextureDesc::colour(UVec2::ONE))?;
        let (id, tex) = (material_name(ctx, "t-plaid")?, TextureName::new("t-plaid-tex").unwrap());
        let mut material = MaterialSpec::toon(cloth);
        material.base_texture = Some(tex.clone());
        let mesh = grid_mesh("plaid", &id, 3, &mut ctx.rng);
        Ok(PartOutput {
            meshes: vec![mesh],
            materials: vec![(id, material)],
            textures: vec![(tex, texture)],
            ..PartOutput::default()
        })
    }

    fn recolour(
        params: &PlaidParams,
        output: &mut PartOutput,
        ctx: &RecolourCtx<'_>,
    ) -> Result<Recoloured, BuildError> {
        count(&params.tag, |c| {
            c.recolours += 1;
            c.paints += 1;
        });
        let ([(_, material)], [(_, texture)]) = (output.materials.as_mut_slice(), output.textures.as_mut_slice())
        else {
            return Ok(Recoloured::NeedsRebuild);
        };
        let (cloth, stripe) = (ctx.colour(&params.cloth)?, ctx.colour(&params.stripe)?);
        *texture = ctx.paint_texture(texture.desc.size, PLAID_RECT, &plaid_list(cloth, stripe), texture.desc)?;
        material.base = cloth;
        Ok(Recoloured::Done)
    }
}
omuzaki_char_api::register_part_kind!(Plaid);

// ------------------------------------------------------------------------------------------------ helpers

fn pid(s: &str) -> PartId {
    PartId::new(s).unwrap()
}

fn slot(s: &str) -> PaletteSlot {
    PaletteSlot::new(s).unwrap()
}

fn char_id() -> CharId {
    CharId::new("mock").unwrap()
}

fn add(def: &mut CharacterDef, part: &str, kind: &str, layer: i32, params: Value) {
    let part_def =
        PartDef { kind: PartKindId::new(kind).unwrap(), parent: None, enabled: true, layer, attach: None, params };
    def.parts.insert(pid(part), part_def);
}

/// The mock character plus a lamp, a prop at each end of the layers, a dyed (hair-like) part and a plaid (garment-like)
/// one, and the `hair_root`, `cloth` and `trim` colours they name.
fn dressed() -> CharacterDef {
    let mut def = mock_character().unwrap();
    for (name, colour) in
        [("hair_root", [0x5a, 0x3b, 0x2e]), ("cloth", [0xd8, 0xc8, 0xb0]), ("trim", [0x70, 0x30, 0x30])]
    {
        def.palette.0.insert(slot(name), Colour::rgb(colour[0], colour[1], colour[2]));
    }
    add(&mut def, "lamp", "t-lamp", 10, json!({}));
    add(&mut def, "prop-a", "t-prop", 0, json!({}));
    add(&mut def, "prop-b", "t-prop", 40, json!({ "tint": "@hair_root", "rings": 9 }));
    add(&mut def, "dyed", "t-dyed", 45, json!({}));
    add(&mut def, "plaid", "t-plaid", 25, json!({}));
    def
}

/// The services of a build that can paint textures.
fn painting() -> BuildServices {
    BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer))
}

struct HashSink(DefaultHasher);

impl fmt::Write for HashSink {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}

/// A hash of the exact `Debug` text, which prints every float in a form that tells -0.0 from 0.0 and round-trips.
fn digest(value: &dyn Debug) -> u64 {
    let mut sink = HashSink(DefaultHasher::new());
    write!(sink, "{value:?}").unwrap();
    sink.0.finish()
}

/// Every part of a built character, hashed in sections so a mismatch says which section differs. The simulations are
/// compared through what the frame loop reads from them.
fn signature(b: &BuiltCharacter) -> Vec<(&'static str, u64)> {
    let runtimes: Vec<_> = b
        .runtimes
        .iter()
        .map(|slot| {
            let meshes: Vec<_> = (0..slot.runtime.dynamic_count()).map(|i| slot.runtime.dynamic_mesh(i)).collect();
            (slot.part.to_string(), format!("{meshes:?}"), format!("{:?}", slot.runtime.cost()))
        })
        .collect();
    vec![
        ("id", digest(&(&b.id, &b.variant))),
        ("def", digest(&b.def)),
        ("palette", digest(&b.palette)),
        ("meshes", digest(&b.meshes)),
        ("materials", digest(&b.materials)),
        ("textures", digest(&b.textures)),
        ("face", digest(&b.face)),
        ("anchors", digest(&b.anchors)),
        ("rig", digest(&b.rig)),
        ("report", digest(&b.report)),
        ("runtimes", digest(&runtimes)),
    ]
}

// ------------------------------------------------------------------------------------------------ facts and the palette

omuzaki_char_api::fact! {
    /// A byte a part publishes: constant, or (on purpose, to be refused) a channel of a palette colour.
    pub NoteFact = "t-note", Last<u8>;
}
omuzaki_char_api::register_fact!(NoteFact);

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
struct NoterParams {
    /// Publish the red channel of `tint` instead of 7: a fact that depends on the palette.
    from_palette: bool,
    tint: ColourRef,
}

impl Default for NoterParams {
    fn default() -> Self {
        Self { from_palette: false, tint: ColourRef::slot_or("outfit", ColourRef::rgb(200, 200, 200)) }
    }
}

impl Validate for NoterParams {
    fn check(&self, _: &mut Check) {}
}

/// Publishes a [`NoteFact`] and names a palette slot (so a palette change rebuilds it), without a recolour.
struct Noter;

impl PartKind for Noter {
    type Params = NoterParams;

    fn meta() -> PartMeta {
        PartMeta { id: "t-noter", summary: "Publishes a note.", category: PartCategory::Accessory, requires: &[] }
    }

    fn build(params: &NoterParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        let tint = ctx.colour(&params.tint)?;
        let note = if params.from_palette { tint.r } else { 7 };
        Ok(PartOutput { publishes: Publishes::default().with::<NoteFact>(note), ..PartOutput::default() })
    }
}
omuzaki_char_api::register_part_kind!(Noter);

/// Reads the [`NoteFact`] into its report: a stale view shows in the report of a cached build.
struct Reader;

impl PartKind for Reader {
    type Params = LampParams;

    fn meta() -> PartMeta {
        PartMeta {
            id: "t-note-reader",
            summary: "Reports the note.",
            category: PartCategory::Accessory,
            requires: &["t-noter"],
        }
    }

    fn build(_: &LampParams, ctx: &mut BuildCtx<'_>) -> Result<PartOutput, BuildError> {
        Ok(PartOutput {
            warnings: vec![format!("note {:?}", ctx.published.get::<NoteFact>())],
            ..PartOutput::default()
        })
    }
}
omuzaki_char_api::register_part_kind!(Reader);

/// The mock character with a `noter` (publishing from the palette or not), and a `reader` after it.
fn noted(from_palette: bool) -> CharacterDef {
    let mut def = mock_character().unwrap();
    add(&mut def, "noter", "t-noter", 10, json!({ "from_palette": from_palette }));
    add(&mut def, "reader", "t-note-reader", 40, json!({ "publish": false }));
    def
}

#[test]
fn a_palette_dependent_fact_is_refused_by_the_conformance_check_naming_the_kind_and_the_fact() {
    let probes = palette_facts::Probes::default().fact::<NoteFact>();
    let report = palette_facts::check(&noted(true), Some(&["t-noter"]), &probes);
    let [violation] = report.violations.as_slice() else { panic!("one violation expected: {report:?}") };
    assert!(violation.contains("kind 't-noter'") && violation.contains("part 'noter'"), "{violation}");
    assert!(violation.contains("fact 't-note' depends on the palette"), "{violation}");

    // Why the check exists: the reader is keyed on the publisher's geometry, so the cache serves it a stale note.
    let (options, services) = live();
    let (builder, mut def) = (CachingBuilder::new(), noted(true));
    cached(&builder, &def, &options, &services).unwrap();
    def.palette.0.insert(slot("outfit"), Colour::rgb(9, 8, 7));
    let stale = cached(&builder, &def, &options, &services).unwrap().0;
    let fresh = reference(&def, &options, &services).unwrap();
    assert_ne!(
        signature(&stale),
        signature(&fresh),
        "a palette-dependent fact is served stale: the check must refuse it"
    );
}

#[test]
fn a_palette_independent_fact_gives_the_same_cached_build_after_a_palette_change_and_never_rebuilds_the_reader() {
    let probes = palette_facts::Probes::default().fact::<NoteFact>();
    palette_facts::check(&noted(false), Some(&["t-noter", "t-note-reader"]), &probes).assert_independent();

    let (options, services) = live();
    let (builder, mut def) = (CachingBuilder::new(), noted(false));
    same_as_reference("first", &builder, &def, &options, &services).unwrap();
    def.palette.0.insert(slot("outfit"), Colour::rgb(9, 8, 7));
    let t = same_as_reference("after a palette change", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).contains("noter"), "the publisher names the slot: {:?}", built(&t));
    assert!(!built(&t).contains("reader") && !recoloured(&t).contains("reader"), "Q6: the reader is served");
}

/// A pose that never moves: the simulations here ignore it.
struct Still;

impl PoseSource for Still {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(Mat4::IDENTITY)
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// Steps every simulation of `b` through `frames` frames of a windy scene and returns what they look like afterwards.
fn simulate(b: &mut BuiltCharacter, frames: u32) -> u64 {
    for frame in 0..frames {
        let ctx = StepCtx {
            dt: 1.0 / 60.0,
            time: Seconds(f64::from(frame) / 60.0),
            pose: &Still,
            wind: Vec3::new(1.0, 0.0, 0.5),
        };
        for slot in &mut b.runtimes {
            slot.runtime.step(&ctx);
        }
    }
    signature(b).iter().find(|(section, _)| *section == "runtimes").map_or(0, |(_, d)| *d)
}

/// What a build gave, for comparing: the signature, or the error.
fn outcome(r: &Result<BuiltCharacter, BuildError>) -> Result<Vec<(&'static str, u64)>, BuildError> {
    r.as_ref().map(signature).map_err(Clone::clone)
}

fn reference(
    def: &CharacterDef,
    options: &BuildOptions,
    services: &BuildServices,
) -> Result<BuiltCharacter, BuildError> {
    RegistryBuilder.build(&char_id(), def, options, services)
}

fn cached(
    builder: &CachingBuilder,
    def: &CharacterDef,
    options: &BuildOptions,
    services: &BuildServices,
) -> Result<(BuiltCharacter, BuildTimings), BuildError> {
    builder.build_timed(&char_id(), def, options, services)
}

fn built(t: &BuildTimings) -> BTreeSet<String> {
    t.built().map(ToString::to_string).collect()
}

fn recoloured(t: &BuildTimings) -> BTreeSet<String> {
    t.recoloured().map(ToString::to_string).collect()
}

fn names(parts: &[&str]) -> BTreeSet<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

/// Builds with both builders and requires the same outcome; returns the cached build's timings.
fn same_as_reference(
    label: &str,
    builder: &CachingBuilder,
    def: &CharacterDef,
    options: &BuildOptions,
    services: &BuildServices,
) -> Option<BuildTimings> {
    let mut want = reference(def, options, services);
    let mut got = cached(builder, def, options, services);
    assert_eq!(outcome(&want), got.as_ref().map(|(b, _)| signature(b)).map_err(Clone::clone), "{label}");
    if let (Ok(a), Ok((b, _))) = (&mut want, &mut got) {
        assert!(a.meshes == b.meshes && a.materials == b.materials && a.textures == b.textures, "{label}: ==");
        assert!(a.anchors == b.anchors && a.rig == b.rig && a.report == b.report && a.face == b.face, "{label}: ==");
        // A simulation served from the cache must also behave like a fresh one once the frame loop runs it.
        assert_eq!(simulate(a, 12), simulate(b, 12), "{label}: after 12 frames");
    }
    got.ok().map(|(_, t)| t)
}

// ------------------------------------------------------------------------------------------------ edit sequences

fn normalised(def: &CharacterDef, part: &str) -> Option<Value> {
    let p = def.parts.get(&pid(part))?;
    let reg = find_kind(p.kind.as_str())?;
    Some((reg.check)(&p.params).unwrap_or_else(|_| p.params.clone()))
}

/// Sets one value, by JSON pointer, in a part's normalised parameters (nothing happens if the part or path is gone).
fn set(def: &mut CharacterDef, part: &str, pointer: &str, value: Value) {
    let Some(mut params) = normalised(def, part) else { return };
    if let Some(target) = params.pointer_mut(pointer) {
        *target = value;
    }
    if let Some(p) = def.parts.get_mut(&pid(part)) {
        p.params = params;
    }
}

fn pick<'a, T>(rng: &mut Rng, items: &'a [T]) -> &'a T {
    &items[(rng.next_u64() % items.len() as u64) as usize]
}

fn float(rng: &mut Rng, lo: f32, hi: f32) -> Value {
    json!(f64::from(rng.range_f32(lo, hi)))
}

fn colour_ref(rng: &mut Rng, slots: &[&str]) -> Value {
    if rng.next_f32() < 0.6 {
        json!(format!("@{}", pick(rng, slots)))
    } else {
        json!(format!("#{:06x}", rng.next_u64() & 0xff_ffff))
    }
}

fn brush(radius: f64, strength: f64) -> WeightBrush {
    WeightBrush { centre: [0.0, 0.5, 0.0], radius, joint: JointRef::CHEST, strength, falloff: BrushFalloff::Smooth }
}

/// The variants the colour edits apply: two colourways (palette slots and the hair's streaks, as path overrides) and the
/// mock's close-up.
fn colour_variants() -> VariantSet {
    let mut variants = mock_variants().unwrap();
    let extra: VariantSet = serde_json::from_value(json!({
        "noir": {
            "description": "A dark colourway.",
            "overrides": {
                "palette/skin": "#c8b0a8", "palette/outfit": "#101820", "palette/trim": "#d0d0d8",
                "parts/hair/params/shading/mid": "@outfit"
            }
        },
        "streaked": {
            "description": "Streaks from the palette.",
            "overrides": {
                "palette/cloth": "#2060a0",
                "parts/hair/params/shading/streaks": [
                    { "fraction": 0.6, "colour": "@cloth", "strength": 0.9, "seed": 4, "blend": "replace" }
                ]
            }
        }
    }))
    .unwrap();
    variants.extend(extra);
    variants
}

/// How many different edits `edit` knows.
const EDITS: u64 = 39;

/// Any edit.
fn any_edit(rng: &mut Rng) -> u64 {
    rng.next_u64() % EDITS
}

/// An edit that changes a colour, or the parts that bake colours: the palette (set a slot, remove one, a whole colourway),
/// a colour reference in a part, the hair's streaks, its simulation and its fringe, and a part of a kind that doesn't
/// recolour.
fn colour_edit(rng: &mut Rng) -> u64 {
    *pick(rng, &[6, 10, 12, 12, 12, 13, 29, 30, 30, 30, 31, 32, 33, 34, 35, 37, 38, 38, 38])
}

/// One edit of the character or of how it is built, chosen by `choose`; returns which one, for the failure message.
#[allow(clippy::too_many_lines)] // a flat table of edits reads better than a dozen one-line functions
fn edit(rng: &mut Rng, def: &mut CharacterDef, options: &mut BuildOptions, choose: fn(&mut Rng) -> u64) -> u64 {
    const SLOTS: [&str; 7] = ["skin", "skin_shade", "skin_line", "outfit", "hair_root", "cloth", "trim"];
    if def.parts.is_empty() {
        add(def, "body", "mock-body", 0, json!({}));
    }
    let parts: Vec<String> = def.parts.keys().map(ToString::to_string).collect();
    let which = choose(rng);
    let target = pick(rng, &parts).clone();
    match which {
        0 => set(def, "body", "/proportions/torso_length", float(rng, 0.85, 1.15)),
        1 => set(def, "body", "/proportions/shoulder_width", float(rng, 0.85, 1.15)),
        2 => set(def, "head", "/cranium/half_width", float(rng, 0.16, 0.21)),
        3 => set(def, "hair", "/groups/c-curtain/generator/count", json!(10 + rng.next_u64() % 50)),
        4 => set(def, "hair", "/groups/c-curtain/generator/length_back", float(rng, 0.3, 0.7)),
        5 => set(def, "hair", "/shading/ring_strength", float(rng, 0.0, 1.0)),
        6 => set(def, "hair", "/shading/root", colour_ref(rng, &SLOTS)),
        7 => set(def, "hair", "/shading/brows_through", json!(rng.next_f32() < 0.5)),
        8 => set(def, "shirt", "/ease", float(rng, 0.001, 0.02)),
        9 => set(def, "vest", "/ease", float(rng, 0.001, 0.02)),
        10 => set(def, "vest", "/colour", colour_ref(rng, &SLOTS)),
        11 => set(def, &target, "/ease", json!(5.0)), // out of range for a garment: a Params error
        12 => {
            let c = Colour::rgb(rng.next_u64() as u8, rng.next_u64() as u8, rng.next_u64() as u8);
            def.palette.0.insert(slot(pick(rng, &SLOTS)), c);
        }
        13 => {
            def.palette.0.remove(&slot(pick(rng, &SLOTS)));
        }
        14 => def.seed = rng.next_u64() as u32 % 100,
        15 => {
            if let Some(p) = def.parts.get_mut(&pid(&target)) {
                p.enabled = !p.enabled;
            }
        }
        16 => {
            if let Some(p) = def.parts.get_mut(&pid(&target)) {
                p.layer = (rng.next_u64() % 60) as i32;
            }
        }
        17 => {
            let layer = 25 + (rng.next_u64() % 40) as i32;
            let ease = rng.range_f32(0.002, 0.02);
            add(def, &format!("coat-{}", rng.next_u64() % 4), "mock-garment", layer, json!({ "ease": ease }));
        }
        18 => {
            if def.parts.len() > 1 {
                def.parts.remove(&pid(&target));
            }
        }
        19 => def.face.layout.mouth_y += f64::from(rng.range_f32(-0.02, 0.02)),
        20 => {
            let b = brush(f64::from(rng.range_f32(0.05, 0.4)), f64::from(rng.next_f32()));
            def.weight_brushes.entry(pid(&target)).or_default().push(b);
        }
        21 => set(def, &target, "/rings", json!(2 + rng.next_u64() % 14)),
        22 => set(def, "lamp", "/publish", json!(rng.next_f32() < 0.5)),
        23 => options.quality = if options.quality == Quality::Live { Quality::Offline } else { Quality::Live },
        24 => options.rig_export = !options.rig_export,
        25 => {
            if let (Some(params), Some(p)) = (normalised(def, &target), def.parts.get_mut(&pid(&target))) {
                p.params = params; // the same part written out in full: not an edit
            }
        }
        26 => add(def, "ghost", "t-no-such-kind", 5, json!({})),
        27 => {
            if let Some(p) = def.parts.get_mut(&pid(&target)) {
                p.parent = Some(pid("nobody"));
            }
        }
        28 => def.weight_brushes.entry(pid(&target)).or_default().push(brush(-1.0, 0.5)),
        29 => {
            // The same parameters under the other prop kind (which doesn't recolour).
            let prop = if rng.next_f32() < 0.5 { "prop-a" } else { "prop-b" };
            if let Some(p) = def.parts.get_mut(&pid(prop)) {
                let swapped = if p.kind.as_str() == "t-prop" { "t-prop-b" } else { "t-prop" };
                p.kind = PartKindId::new(swapped).unwrap();
            }
        }
        30 => {
            // A colourway: several slots change at once.
            for _ in 0..3 + rng.next_u64() % 3 {
                let c = Colour::rgb(rng.next_u64() as u8, rng.next_u64() as u8, rng.next_u64() as u8);
                def.palette.0.insert(slot(pick(rng, &SLOTS)), c);
            }
        }
        31 => {
            let streak = |rng: &mut Rng| {
                json!({
                    "fraction": f64::from(rng.range_f32(0.1, 0.9)),
                    "colour": colour_ref(rng, &SLOTS),
                    "strength": f64::from(rng.range_f32(0.2, 1.0)),
                    "seed": rng.next_u64() % 50,
                    "blend": if rng.next_f32() < 0.5 { "replace" } else { "multiply" },
                })
            };
            let streaks: Vec<Value> = (0..rng.next_u64() % 3).map(|_| streak(rng)).collect();
            set(def, "hair", "/shading/streaks", Value::Array(streaks));
        }
        32 => set(def, "hair", "/groups/c-curtain/dynamics/simulate", json!(rng.next_f32() < 0.5)),
        33 => set(def, "hair", "/shading/mid", colour_ref(rng, &SLOTS)),
        34 => set(def, "dyed", if rng.next_f32() < 0.5 { "/root" } else { "/tip" }, colour_ref(rng, &SLOTS)),
        35 => set(def, "plaid", if rng.next_f32() < 0.5 { "/cloth" } else { "/stripe" }, colour_ref(rng, &SLOTS)),
        36 => set(def, "plaid", "/size", json!(16 + 8 * (rng.next_u64() % 6))),
        38 => {
            // A variant applied over the character: colourways as path overrides (palette slots, the hair's shading), and
            // the mock's own close-up (a fringe and a different face).
            let variants = colour_variants();
            let id = pick(rng, &["noir", "streaked", "close-up"]);
            if let Ok(applied) = resolve_variant(def, &variants, &VariantId::new(*id).unwrap()) {
                *def = applied;
            }
        }
        _ => {
            // The fringe the close-up variant adds: more clumps, in a group the streaks can single out.
            if let Some(mut params) = normalised(def, "hair") {
                if let Some(groups) = params.pointer_mut("/groups").and_then(Value::as_object_mut)
                    && groups.remove("b-bangs").is_none()
                {
                    groups.insert("b-bangs".to_owned(), json!({ "generator": { "type": "bangs" } }));
                }
                if let Some(p) = def.parts.get_mut(&pid("hair")) {
                    p.params = params;
                }
            }
        }
    }
    which
}

const STEPS: usize = 12;

/// Runs one seeded sequence of `choose`n edits from `start` against `builder`, checking every step against the reference
/// builder (twice: the second build of an unchanged character is served from the cache). Returns the digest of every
/// step's outcome and how many parts the builder recoloured.
fn run_edits(
    seed: u64,
    builder: &CachingBuilder,
    roomy: bool,
    start: CharacterDef,
    choose: fn(&mut Rng) -> u64,
) -> (Vec<u64>, usize) {
    let (mut rng, mut def, mut options) = (Rng::new(seed), start, BuildOptions::default());
    let services = painting();
    let (mut digests, mut recoloured_parts) = (Vec::new(), 0);
    for step in 0..STEPS {
        let which = edit(&mut rng, &mut def, &mut options, choose);
        let label = format!("sequence {seed} step {step} (edit {which}), {} parts", def.parts.len());
        let timings = same_as_reference(&label, builder, &def, &options, &services);
        recoloured_parts += timings.map_or(0, |t| recoloured(&t).len());
        let again = same_as_reference(&format!("{label}, again"), builder, &def, &options, &services);
        if let (true, Some(again)) = (roomy, again) {
            // An unchanged character comes entirely from the cache, the hair's simulation included.
            assert!(built(&again).is_empty(), "{label}: {:?}", built(&again));
        }
        digests.push(digest(&outcome(&cached(builder, &def, &options, &services).map(|(b, _)| b))));
    }
    (digests, recoloured_parts)
}

fn run_sequence(seed: u64, builder: &CachingBuilder, roomy: bool) -> Vec<u64> {
    run_edits(seed, builder, roomy, dressed(), any_edit).0
}

/// The dressed character with hair that has streaks from the palette over it and a fringe, so the hair's tints are among what a palette change has to rewrite.
fn tinted() -> CharacterDef {
    let mut def = dressed();
    let streak = |colour: &str, blend: &str, seed: u32| json!({ "fraction": 0.5, "colour": colour, "strength": 0.8, "seed": seed, "blend": blend });
    let hair = json!({
        "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "b-bangs": { "generator": { "type": "bangs" } },
            "c-curtain": { "generator": { "type": "curtain" }, "dynamics": { "simulate": true, "follow": "body" } },
        },
        "shading": { "streaks": [streak("@trim", "replace", 3), streak("@cloth", "multiply", 9)] },
    });
    def.parts.get_mut(&pid("hair")).unwrap().params = hair;
    def
}

#[test]
fn fifty_seeded_edit_sequences_match_the_reference_builder_byte_for_byte() {
    for seed in 0..50_u64 {
        let threads = [1, 2, 4, 8][(seed % 4) as usize];
        // Every fifth sequence keeps only a few parts, so the cache evicts constantly.
        let roomy = seed % 5 != 4;
        let builder = CachingBuilder::new().with_threads(threads);
        let builder = if roomy { builder } else { builder.with_limits(5, usize::MAX) };
        run_sequence(seed, &builder, roomy);
        assert!(roomy || builder.cached_parts() <= 5);
    }
}

#[test]
fn colour_sequences_match_the_reference_and_mostly_recolour() {
    let mut recoloured_parts = 0;
    for seed in 200..230_u64 {
        let builder = CachingBuilder::new().with_threads([1, 4][(seed % 2) as usize]);
        recoloured_parts += run_edits(seed, &builder, true, tinted(), colour_edit).1;
    }
    // 30 sequences of 12 steps, most of which change a colour that several parts name: the recolour path is the norm.
    assert!(recoloured_parts > 400, "only {recoloured_parts} parts were recoloured");
}

#[test]
fn two_runs_and_every_thread_count_give_identical_bytes() {
    for seed in 100..106_u64 {
        let serial = run_sequence(seed, &CachingBuilder::new().with_threads(1), true);
        let again = run_sequence(seed, &CachingBuilder::new().with_threads(1), true);
        assert_eq!(serial, again, "sequence {seed}: two runs");
        for threads in [2, 3, 8] {
            let parallel = run_sequence(seed, &CachingBuilder::new().with_threads(threads), true);
            assert_eq!(serial, parallel, "sequence {seed}: {threads} threads against one");
        }
    }
}

// ------------------------------------------------------------------------------------------------ what rebuilds

fn live() -> (BuildOptions, BuildServices) {
    (BuildOptions::default(), painting())
}

#[test]
fn an_edit_rebuilds_the_part_and_those_after_a_publisher_it_changed_and_nothing_else() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = mock_character().unwrap();
    let step = |label: &str, def: &CharacterDef, options: &BuildOptions, expect: &[&str]| {
        let t = same_as_reference(label, &builder, def, options, &services).unwrap();
        assert_eq!(built(&t), names(expect), "{label}");
    };
    // The hair has a simulation. It is served from the cache like any part: it appears below only when an edit reaches it.
    step("first build", &def, &options, &["body", "head", "hair", "shirt", "vest"]);
    step("nothing changed", &def, &options, &[]);

    set(&mut def, "hair", "/shading/ring_strength", json!(0.9));
    step("one hair parameter", &def, &options, &["hair"]);
    set(&mut def, "vest", "/ease", json!(0.012));
    step("the outermost garment", &def, &options, &["vest"]);
    set(&mut def, "shirt", "/ease", json!(0.012));
    step("the garment under it", &def, &options, &["shirt", "vest"]);
    set(&mut def, "head", "/cranium/half_width", json!(0.19));
    step("the head, which publishes", &def, &options, &["head", "hair", "shirt", "vest"]);
    set(&mut def, "body", "/proportions/torso_length", json!(1.1));
    step("the body, which everything follows", &def, &options, &["body", "head", "hair", "shirt", "vest"]);

    // A palette change builds nothing a kind can recolour, and recolours only the parts that name the slot.
    let swap = |label: &str, def: &CharacterDef, built_parts: &[&str], recoloured_parts: &[&str]| {
        let t = same_as_reference(label, &builder, def, &options, &services).unwrap();
        assert_eq!((built(&t), recoloured(&t)), (names(built_parts), names(recoloured_parts)), "{label}");
    };
    def.palette.0.insert(slot("outfit"), Colour::rgb(1, 2, 3));
    swap("a slot only the vest names", &def, &[], &["vest"]);
    def.palette.0.insert(slot("skin"), Colour::rgb(250, 200, 190));
    swap("a skin colour: the body and head, not the garments or the hair over them", &def, &[], &["body", "head"]);
    def.weight_brushes.insert(pid("shirt"), vec![brush(0.3, 0.5)]);
    step("a weight brush, applied when merging", &def, &options, &[]);
    let before = def.clone();
    def.parts.get_mut(&pid("shirt")).unwrap().params = normalised(&def, "shirt").unwrap();
    step("the same parameters written out in full", &def, &options, &[]);
    def.face.layout.mouth_y += 0.01;
    step("the face, which every part sees", &def, &options, &["body", "head", "hair", "shirt", "vest"]);
    def.seed += 1;
    step("the seed", &def, &options, &["body", "head", "hair", "shirt", "vest"]);
    step("back to an earlier character, whose parts are still cached", &before, &options, &[]);
    let offline = BuildOptions { quality: Quality::Offline, ..options.clone() };
    step("another quality", &before, &offline, &["body", "head", "hair", "shirt", "vest"]);
    step("and the first quality again, still cached", &before, &options, &[]);
    let export = BuildOptions { rig_export: true, ..options.clone() };
    step("built for export", &before, &export, &["body", "head", "hair", "shirt", "vest"]);
}

#[test]
fn editing_a_parts_attachment_rebuilds_it() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = dressed();
    cached(&builder, &def, &options, &services).unwrap();
    let to_head = |x: f64| Attach::Bone {
        bone: JointRef::HEAD,
        offset: AttachOffset { translation: [x, 0.0, 0.0], ..AttachOffset::default() },
    };
    def.parts.get_mut(&pid("prop-a")).unwrap().attach = Some(to_head(0.0));
    let t = same_as_reference("prop-a is attached", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).contains("prop-a"), "{:?}", built(&t));
    def.parts.get_mut(&pid("prop-a")).unwrap().attach = Some(to_head(0.01));
    let t = same_as_reference("its offset changes", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).contains("prop-a"), "{:?}", built(&t));
    let t = same_as_reference("nothing changes", &builder, &def, &options, &services).unwrap();
    assert!(!built(&t).contains("prop-a"), "{:?}", built(&t));
}

#[test]
fn a_part_changing_kind_with_the_same_parameters_is_rebuilt_as_the_new_kind() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = dressed();
    let (before, _) = cached(&builder, &def, &options, &services).unwrap();
    def.parts.get_mut(&pid("prop-a")).unwrap().kind = PartKindId::new("t-prop-b").unwrap();
    let t = same_as_reference("prop-a is now the other kind", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).contains("prop-a"), "{:?}", built(&t));
    let (after, _) = cached(&builder, &def, &options, &services).unwrap();
    let vertices = |c: &BuiltCharacter| c.report.parts.iter().find(|p| p.part.as_str() == "prop-a").unwrap().vertices;
    assert_ne!(vertices(&before), vertices(&after), "the new kind's mesh, not the old kind's");
    def.parts.get_mut(&pid("prop-a")).unwrap().kind = PartKindId::new("t-prop").unwrap();
    let t = same_as_reference("and back, from the cache", &builder, &def, &options, &services).unwrap();
    assert!(!built(&t).contains("prop-a"), "{:?}", built(&t));
}

#[test]
fn a_palette_change_recolours_only_the_parts_that_name_the_slot() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = mock_character().unwrap();
    let (mut character, _) = cached(&builder, &def, &options, &services).unwrap();

    let mut palette = character.palette.clone();
    palette.0.insert(slot("outfit"), Colour::rgb(200, 30, 30));
    let timings = builder.rebuild_materials(&mut character, &palette, &options, &services).unwrap();
    assert!(built(&timings).is_empty(), "{:?}", built(&timings));
    assert_eq!(recoloured(&timings), names(&["vest"]));
    assert_eq!(timings.parts.len(), 5);
    assert_eq!(character.palette, palette);

    let mut recoloured_def = def.clone();
    recoloured_def.palette = palette.clone();
    let want = reference(&recoloured_def, &options, &services).unwrap();
    assert_eq!(signature(&character), signature(&want), "the same as building the new palette from scratch");

    // A colourway swap changes the skin and the outfit: the body, head and vest are recoloured, the shirt is served.
    let mut colourway = palette.clone();
    for (name, c) in [
        ("skin", [250, 210, 200]),
        ("skin_shade", [240, 180, 170]),
        ("skin_line", [190, 120, 110]),
        ("outfit", [20, 60, 90]),
    ] {
        colourway.0.insert(slot(name), Colour::rgb(c[0], c[1], c[2]));
    }
    let timings = builder.rebuild_materials(&mut character, &colourway, &options, &services).unwrap();
    assert!(built(&timings).is_empty(), "{:?}", built(&timings));
    assert_eq!(recoloured(&timings), names(&["body", "head", "vest"]));
    recoloured_def.palette = colourway;
    assert_eq!(signature(&character), signature(&reference(&recoloured_def, &options, &services).unwrap()));

    // A palette that breaks the character leaves the built character as it was.
    let mut broken = character.palette.clone();
    broken.0.remove(&slot("outfit"));
    let before = signature(&character);
    let err = builder.rebuild_materials(&mut character, &broken, &options, &services).unwrap_err();
    assert!(matches!(err, BuildError::Colour { .. }), "{err:?}");
    assert_eq!(signature(&character), before);
}

/// `dressed()` with the parts that count what they do (the props, the dyed part and the plaid) tagged `tag`.
fn counted(tag: &str) -> CharacterDef {
    let mut def = dressed();
    for part in ["prop-a", "prop-b", "dyed", "plaid"] {
        set(&mut def, part, "/tag", json!(tag));
    }
    def
}

/// A colourway that changes every slot the character names.
fn recoloured_palette(character: &BuiltCharacter, shift: u8) -> omuzaki_face_api::Palette {
    let mut palette = character.palette.clone();
    for (i, name) in ["skin", "skin_shade", "skin_line", "outfit", "hair_root", "cloth", "trim"].into_iter().enumerate()
    {
        let n = shift.wrapping_add(i as u8 * 17);
        palette.0.insert(slot(name), Colour::rgb(n, n.wrapping_mul(3), 255 - n));
    }
    palette
}

#[test]
fn a_colourway_swap_recolours_from_the_cache_and_builds_no_geometry() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = counted("swap");
    let (mut character, first) = cached(&builder, &def, &options, &services).unwrap();
    assert!(built(&first).is_superset(&names(&["prop-a", "prop-b", "dyed", "plaid"])));
    assert_eq!(counts("swap"), Counts { builds: 4, recolours: 0, paints: 1 });

    let colourway = recoloured_palette(&character, 40);
    let timings = builder.rebuild_materials(&mut character, &colourway, &options, &services).unwrap();
    // The counters: no generator ran for any counted part, each was recoloured once, and the plaid was repainted.
    assert_eq!(counts("swap"), Counts { builds: 4, recolours: 4, paints: 2 });
    assert!(built(&timings).is_empty(), "no generator ran: {:?}", built(&timings));
    assert_eq!(recoloured(&timings), names(&["body", "head", "vest", "prop-a", "prop-b", "dyed", "plaid"]));
    let mut want = def.clone();
    want.palette = colourway.clone();
    assert_eq!(signature(&character), signature(&reference(&want, &options, &services).unwrap()));

    // Swapping back is served from the cache: the recoloured entries were kept under their new keys.
    let original = def.palette.clone();
    let before = counts("swap");
    let timings = builder.rebuild_materials(&mut character, &original, &options, &services).unwrap();
    assert_eq!(counts("swap"), before, "nothing built, recoloured or painted");
    assert!(built(&timings).is_empty() && recoloured(&timings).is_empty());
    assert_eq!(signature(&character), signature(&reference(&def, &options, &services).unwrap()));

    // A geometry edit and a palette change together: the edited part builds, the others recolour.
    let mut both = def.clone();
    both.palette = colourway;
    set(&mut both, "dyed", "/rings", json!(12));
    let t = same_as_reference("an edit with a palette change", &builder, &both, &options, &services).unwrap();
    assert!(built(&t).contains("dyed") && !built(&t).contains("plaid"), "{:?}", built(&t));
}

#[test]
fn a_simulated_hair_is_re_tinted_from_the_cache_with_no_regeneration() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = tinted(); // streaks from @trim and @cloth over a hair whose curtain is simulated
    let (mut character, _) = cached(&builder, &def, &options, &services).unwrap();
    let steps = |c: &BuiltCharacter| {
        // The frame loop's view of the simulations: a fresh one per build, at its initial state.
        c.runtimes.iter().map(|r| r.runtime.dynamic_count()).sum::<usize>()
    };
    assert_eq!(steps(&character), 1, "the curtain is simulated");
    let colourway = recoloured_palette(&character, 77);
    let timings = builder.rebuild_materials(&mut character, &colourway, &options, &services).unwrap();
    assert!(built(&timings).is_empty(), "the hair was not regenerated: {:?}", built(&timings));
    assert!(recoloured(&timings).contains("hair"), "{:?}", recoloured(&timings));
    assert_eq!(steps(&character), 1, "and it still has its simulation");
    let mut want = def.clone();
    want.palette = colourway;
    assert_eq!(signature(&character), signature(&reference(&want, &options, &services).unwrap()));
}

#[test]
fn a_texture_is_repainted_through_the_rasteriser_and_a_vertex_gradient_rewritten_in_place() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = counted("pixels");
    let (mut character, _) = cached(&builder, &def, &options, &services).unwrap();
    let texture = |c: &BuiltCharacter| c.textures[&TextureName::new("t-plaid-tex").unwrap()].rgba8.clone();
    let gradient = |c: &BuiltCharacter| {
        c.meshes.iter().find(|m| m.part.as_str() == "dyed").map(|m| m.mesh.geometry.colours.clone()).unwrap()
    };
    let positions = |c: &BuiltCharacter| {
        c.meshes.iter().find(|m| m.part.as_str() == "dyed").map(|m| m.mesh.geometry.positions.clone()).unwrap()
    };
    let (pixels, colours, points) = (texture(&character), gradient(&character), positions(&character));

    let colourway = recoloured_palette(&character, 90);
    builder.rebuild_materials(&mut character, &colourway, &options, &services).unwrap();
    assert_ne!(texture(&character), pixels, "the plaid was repainted with the new colours");
    assert_ne!(gradient(&character), colours, "the dyed part's vertex colours were rewritten");
    assert_eq!(positions(&character), points, "and its geometry is the one the build made");
    assert_eq!(counts("pixels").builds, 4);
}

#[test]
fn a_kind_that_cannot_recolour_is_built_while_its_neighbours_are_recoloured() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = counted("mixed");
    def.parts.get_mut(&pid("prop-a")).unwrap().kind = PartKindId::new("t-prop-b").unwrap(); // declines
    let (mut character, _) = cached(&builder, &def, &options, &services).unwrap();
    assert_eq!(counts("mixed").builds, 4);

    let colourway = recoloured_palette(&character, 7);
    let timings = builder.rebuild_materials(&mut character, &colourway, &options, &services).unwrap();
    assert!(built(&timings).contains("prop-a") && !built(&timings).contains("prop-b"), "{:?}", built(&timings));
    assert_eq!(recoloured(&timings), names(&["body", "head", "vest", "prop-b", "dyed", "plaid"]));
    assert_eq!(counts("mixed"), Counts { builds: 5, recolours: 3, paints: 2 }, "prop-a built; prop-b, dyed, plaid not");
    let mut want = def.clone();
    want.palette = colourway;
    assert_eq!(signature(&character), signature(&reference(&want, &options, &services).unwrap()));
}

#[test]
fn a_recolour_that_fails_gives_the_error_a_build_gives_and_is_never_cached() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = counted("broken");
    let (mut character, _) = cached(&builder, &def, &options, &services).unwrap();
    let before = signature(&character);

    // The plaid's recolour fails on the missing slot; the part is built instead, and the build fails the same way.
    let mut palette = character.palette.clone();
    palette.0.remove(&slot("cloth"));
    let mut broken = def.clone();
    broken.palette = palette.clone();
    let want = reference(&broken, &options, &services).unwrap_err();
    let got = builder.rebuild_materials(&mut character, &palette, &options, &services).unwrap_err();
    assert_eq!(got, want);
    assert!(matches!(&got, BuildError::Colour { part, .. } if part.as_str() == "plaid"), "{got:?}");
    assert_eq!(signature(&character), before, "the character is as it was");
    assert_eq!(cached(&builder, &broken, &options, &services).unwrap_err(), want, "and again, not from the cache");

    // The cache is still good: the same character at its original palette is served.
    let t = same_as_reference("the original palette", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).is_empty(), "{:?}", built(&t));
}

#[test]
fn a_recoloured_part_remembers_the_services_it_needs() {
    let options = BuildOptions::default();
    let (full, none) = (painting(), BuildServices::default());
    let builder = CachingBuilder::new();
    let def = counted("services");
    let (character, _) = cached(&builder, &def, &options, &full).unwrap();

    // Recoloured with a rasteriser, the plaid is kept; a build without one must still fail like the reference.
    let mut next = def.clone();
    next.palette = recoloured_palette(&character, 30);
    let t = same_as_reference("recoloured", &builder, &next, &options, &full).unwrap();
    assert!(recoloured(&t).contains("plaid"), "{:?}", recoloured(&t));
    assert!(same_as_reference("and without a rasteriser", &builder, &next, &options, &none).is_none());

    // A colourway swap without a rasteriser cannot recolour the plaid, and fails like a build does.
    let mut third = def.clone();
    third.palette = recoloured_palette(&character, 60);
    assert!(same_as_reference("a swap without a rasteriser", &builder, &third, &options, &none).is_none());
    let t = same_as_reference("and with one again", &builder, &third, &options, &full).unwrap();
    assert!(recoloured(&t).contains("plaid"), "{:?}", recoloured(&t));
}

#[test]
fn timings_name_every_part_in_build_order_and_say_where_it_came_from() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    assert_eq!(builder.last_timings(), None);
    let def = mock_character().unwrap();
    let (character, first) = cached(&builder, &def, &options, &services).unwrap();
    let order: Vec<_> = first.parts.iter().map(|t| t.part.to_string()).collect();
    assert_eq!(order, character.report.parts.iter().map(|p| p.part.to_string()).collect::<Vec<_>>());
    assert!(first.parts.iter().all(|t| t.source == Built));
    let (_, second) = cached(&builder, &def, &options, &services).unwrap();
    let sources: Vec<_> = second.parts.iter().map(|t| t.source).collect();
    assert_eq!(sources, [Cached; 5], "the hair's simulation is served by forking, like every part");
    assert_eq!(builder.last_timings(), Some(second.clone()));
    assert!(second.total >= second.parts.iter().map(|t| t.elapsed).min().unwrap());
}

#[test]
fn every_build_has_its_own_simulations() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = mock_character().unwrap();
    let mut a = builder.build(&char_id(), &def, &options, &services).unwrap();
    let b = builder.build(&char_id(), &def, &options, &services).unwrap();
    let revisions = |c: &BuiltCharacter| -> Vec<u64> {
        c.runtimes
            .iter()
            .flat_map(|s| (0..s.runtime.dynamic_count()).filter_map(|i| s.runtime.dynamic_mesh(i)))
            .map(|m| m.revision)
            .collect()
    };
    let ctx = StepCtx { dt: 1.0 / 60.0, time: Seconds(1.0), pose: &RestPose, wind: Vec3::ZERO };
    for slot in &mut a.runtimes {
        slot.runtime.step(&ctx);
    }
    assert_eq!(revisions(&a), [1]);
    assert_eq!(revisions(&b), [0], "stepping one character leaves the other at rest");
    let c = builder.build(&char_id(), &def, &options, &services).unwrap();
    assert_eq!(signature(&c), signature(&reference(&def, &options, &services).unwrap()));
}

/// A character with one `t-wobble` part, `id`, beside the mock character's hair.
fn with_wobble(id: &str, forks: &str) -> CharacterDef {
    let mut def = dressed();
    add(&mut def, id, "t-wobble", 5, json!({ "forks": forks, "amplitude": 0.5 }));
    def
}

/// Builds with `builder` and returns the timings and how many times `part`'s generator ran for it (the reference
/// builder is not involved: it would run the generator too).
fn build_counting(
    builder: &CachingBuilder,
    def: &CharacterDef,
    options: &BuildOptions,
    services: &BuildServices,
    part: &str,
) -> (BuildTimings, usize) {
    let before = generated(part);
    let (_, timings) = cached(builder, def, options, services).unwrap();
    (timings, generated(part) - before)
}

#[test]
fn a_simulated_part_is_generated_once_and_served_by_forking_after_that() {
    // The generator's own counter, which no builder can influence, proves what the timings claim.
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = with_wobble("wob-served", "always");
    for build in 1..=4 {
        let (t, ran) = build_counting(&builder, &def, &options, &services, "wob-served");
        assert_eq!(ran, usize::from(build == 1), "build {build}: the generator ran only for the first");
        assert_eq!(built(&t).contains("wob-served"), build == 1, "build {build}");
        same_as_reference(&format!("build {build}"), &builder, &def, &options, &services).unwrap();
    }

    // Edits that do not reach the part leave it alone, whatever they rebuild.
    set(&mut def, "vest", "/ease", json!(0.015));
    set(&mut def, "hair", "/shading/ring_strength", json!(0.7));
    def.palette.0.insert(slot("outfit"), Colour::rgb(9, 9, 9));
    def.weight_brushes.insert(pid("shirt"), vec![brush(0.3, 0.5)]);
    let (t, ran) = build_counting(&builder, &def, &options, &services, "wob-served");
    // The vest and the hair changed, and the parts above the vest follow its new envelope; the `outfit` slot, which the
    // first prop names, only recolours it.
    assert_eq!(built(&t), names(&["hair", "vest", "prop-b", "dyed"]));
    assert_eq!(recoloured(&t), names(&["prop-a"]));
    assert_eq!(ran, 0, "unrelated edits do not regenerate the simulated part");
    same_as_reference("edits elsewhere", &builder, &def, &options, &services).unwrap();

    // An edit to the part itself does.
    set(&mut def, "wob-served", "/amplitude", json!(0.25));
    let (t, ran) = build_counting(&builder, &def, &options, &services, "wob-served");
    assert_eq!((built(&t), ran), (names(&["wob-served"]), 1));
    same_as_reference("its own parameter", &builder, &def, &options, &services).unwrap();
}

#[test]
fn every_build_from_the_cache_has_a_fresh_simulation_however_far_the_others_ran() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = with_wobble("wob-fresh", "always");
    let before = generated("wob-fresh");
    let mut first = builder.build(&char_id(), &def, &options, &services).unwrap();
    let mut runs_by_cached_builds = generated("wob-fresh") - before;
    let want = |frames| simulate(&mut reference(&def, &options, &services).unwrap(), frames);
    assert_eq!(simulate(&mut first, 200), want(200));
    for _ in 0..3 {
        let at = generated("wob-fresh");
        let mut served = builder.build(&char_id(), &def, &options, &services).unwrap();
        runs_by_cached_builds += generated("wob-fresh") - at;
        let revisions: Vec<_> =
            served.runtimes.iter().filter_map(|s| s.runtime.dynamic_mesh(0)).map(|m| m.revision).collect();
        assert_eq!(revisions, [0, 0], "at rest, though the first character ran 200 frames");
        assert_eq!(signature(&served), signature(&reference(&def, &options, &services).unwrap()));
        assert_eq!(simulate(&mut served, 30), want(30));
    }
    assert_eq!(runs_by_cached_builds, 1, "four builds, one generator run");
}

#[test]
fn a_simulation_that_cannot_be_forked_is_generated_every_time_and_still_matches() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = with_wobble("wob-never", "never");
    for build in 1..=3 {
        let (t, ran) = build_counting(&builder, &def, &options, &services, "wob-never");
        let expect: &[&str] = if build == 1 {
            &["body", "head", "hair", "lamp", "prop-a", "prop-b", "shirt", "vest", "dyed", "plaid", "wob-never"]
        } else {
            &["wob-never"]
        };
        assert_eq!(built(&t), names(expect), "build {build}: the only part the cache cannot keep");
        assert_eq!(ran, 1, "build {build}");
        same_as_reference(&format!("build {build}"), &builder, &def, &options, &services).unwrap();
    }
    assert_eq!(builder.cached_parts(), def.parts.len() - 1, "it is never stored");
}

#[test]
fn a_simulation_that_stops_forking_is_rebuilt_and_never_served_wrongly() {
    // `Forks::Once` breaks the contract on purpose (the first fork works, later ones do not). The builder must notice
    // when it serves a hit, build the part and carry on, with the reference's bytes.
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let def = with_wobble("wob-once", "once");
    for build in 1..=4 {
        let (_, ran) = build_counting(&builder, &def, &options, &services, "wob-once");
        assert_eq!(
            ran, 1,
            "build {build}: from the second on, the cache's copy no longer forks, so the part is rebuilt"
        );
        same_as_reference(&format!("build {build}"), &builder, &def, &options, &services).unwrap();
    }
}

// ------------------------------------------------------------------------------------------------ parallelism

fn sleepers(n: usize) -> CharacterDef {
    let mut def = mock_character().unwrap();
    for i in 0..n {
        add(&mut def, &format!("sleeper-{i}"), "t-sleeper", 0, json!({}));
    }
    def
}

fn peak_while_rebuilding(threads: usize) -> usize {
    let (options, services) = live();
    let def = sleepers(4);
    let builder = CachingBuilder::new().with_threads(threads);
    // The first build learns which kinds publish nothing; only then can their parts be built side by side.
    same_as_reference("learning", &builder, &def, &options, &services).unwrap();
    builder.clear();
    PEAK.store(0, Ordering::SeqCst);
    same_as_reference("after clear", &builder, &def, &options, &services).unwrap();
    PEAK.load(Ordering::SeqCst)
}

#[test]
fn independent_parts_build_side_by_side_and_one_thread_builds_in_order() {
    // One test, because the kind counts through statics and tests run concurrently.
    assert_eq!(peak_while_rebuilding(1), 1);
    assert!(peak_while_rebuilding(4) >= 2, "four silent parts should overlap");
}

#[test]
fn a_wrong_guess_that_a_part_is_silent_never_leaks_into_the_result() {
    let (options, services) = live();
    let mut def = mock_character().unwrap();
    for (name, layer) in [("lamp-a", 10), ("lamp-b", 11), ("lamp-c", 12)] {
        add(&mut def, name, "t-lamp", layer, json!({}));
    }
    let builder = CachingBuilder::new().with_threads(4);
    same_as_reference("all silent", &builder, &def, &options, &services).unwrap();
    // The lamps are now guessed silent, and lamp-b and lamp-c are cached against a view without lamp-a's anchor.
    set(&mut def, "lamp-a", "/publish", json!(true));
    let t = same_as_reference("lamp-a publishes", &builder, &def, &options, &services).unwrap();
    assert!(built(&t).is_superset(&names(&["lamp-a", "lamp-b", "lamp-c"])), "{:?}", built(&t));
    set(&mut def, "lamp-a", "/publish", json!(false));
    same_as_reference("and stops again", &builder, &def, &options, &services).unwrap();
    set(&mut def, "lamp-b", "/publish", json!(true));
    same_as_reference("lamp-b publishes", &builder, &def, &options, &services).unwrap();
}

// ------------------------------------------------------------------------------------------------ errors and limits

#[test]
fn errors_are_the_references_errors_and_are_never_cached() {
    let (options, services) = live();
    let builder = CachingBuilder::new();
    let mut def = mock_character().unwrap();
    same_as_reference("fine", &builder, &def, &options, &services).unwrap();

    set(&mut def, "vest", "/ease", json!(5.0));
    assert!(same_as_reference("bad parameters", &builder, &def, &options, &services).is_none());
    assert!(same_as_reference("bad parameters again", &builder, &def, &options, &services).is_none());

    // The first error is the first part's in build order: the shirt (layer 20) comes before the vest (layer 30).
    set(&mut def, "shirt", "/ease", json!(-1.0));
    let err = cached(&builder, &def, &options, &services).unwrap_err();
    assert!(matches!(&err, BuildError::Params { part, .. } if part.as_str() == "shirt"), "{err:?}");
    same_as_reference("two bad parts", &builder, &def, &options, &services);

    add(&mut def, "ghost", "t-no-such-kind", 0, json!({}));
    same_as_reference("an unknown kind", &builder, &def, &options, &services);
    def.parts.remove(&pid("ghost"));

    add(&mut def, "twin-a", "t-twin", 0, json!({}));
    add(&mut def, "twin-b", "t-twin", 0, json!({}));
    set(&mut def, "shirt", "/ease", json!(0.01));
    set(&mut def, "vest", "/ease", json!(0.01));
    let err = cached(&builder, &def, &options, &services).unwrap_err();
    assert!(matches!(err, BuildError::Duplicate { .. }), "{err:?}");
    same_as_reference("a clash found while merging", &builder, &def, &options, &services);

    def.parts.remove(&pid("twin-b"));
    same_as_reference("and fixed", &builder, &def, &options, &services).unwrap();
}

#[test]
fn the_cache_stays_within_its_limits_and_the_result_does_not_depend_on_them() {
    let (options, services) = live();
    let def = dressed();
    let few = CachingBuilder::new().with_limits(3, usize::MAX);
    for _ in 0..3 {
        same_as_reference("three parts", &few, &def, &options, &services).unwrap();
        assert!(few.cached_parts() <= 3);
    }
    let none = CachingBuilder::new().with_limits(usize::MAX, 1);
    same_as_reference("no bytes", &none, &def, &options, &services).unwrap();
    assert_eq!(none.cached_parts(), 0, "every part is bigger than one byte");
    let all = CachingBuilder::new();
    same_as_reference("unbounded", &all, &def, &options, &services).unwrap();
    assert_eq!(all.cached_parts(), def.parts.len(), "every part, the hair's simulation included");
    all.clear();
    assert_eq!(all.cached_parts(), 0);
}

// ------------------------------------------------------------------------------------------------ services

#[test]
fn a_cached_part_fails_like_the_reference_when_the_services_it_used_are_missing() {
    let options = BuildOptions::default();
    let mut assets = MemAssets::new();
    let hash = assets.put(&[200, 30, 30]);
    let mut def = mock_character().unwrap();
    add(&mut def, "stripe", "t-stripe", 5, json!({ "asset": hash.to_string() }));
    let full = BuildServices::default().with_assets(assets.clone()).with_rasterizer(|| Box::new(MockRasterizer));
    let builder = CachingBuilder::new();

    same_as_reference("everything wired", &builder, &def, &options, &full).unwrap();
    let t = same_as_reference("and again", &builder, &def, &options, &full).unwrap();
    assert!(!built(&t).contains("stripe"), "served from the cache");

    // Same character, fewer services: the reference fails, so the cache must not hand back the stripe.
    let none = BuildServices::default();
    let no_paint = BuildServices::default().with_assets(assets);
    let no_assets = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    let other = BuildServices::default().with_assets(MemAssets::new()).with_rasterizer(|| Box::new(MockRasterizer));
    for (label, services) in
        [("nothing", &none), ("no rasteriser", &no_paint), ("no assets", &no_assets), ("empty store", &other)]
    {
        assert!(same_as_reference(label, &builder, &def, &options, services).is_none(), "{label}");
    }
    let t = same_as_reference("wired again", &builder, &def, &options, &full).unwrap();
    assert!(!built(&t).contains("stripe"), "the entry survived the failed builds");
}

#[test]
fn a_recoloured_part_still_needs_the_assets_its_build_read() {
    let options = BuildOptions::default();
    let mut assets = MemAssets::new();
    let hash = assets.put(&[200, 30, 30]);
    let mut def = mock_character().unwrap();
    add(&mut def, "stripe", "t-stripe", 5, json!({ "asset": hash.to_string(), "tint": "@outfit" }));
    let full = BuildServices::default().with_assets(assets).with_rasterizer(|| Box::new(MockRasterizer));
    let builder = CachingBuilder::new();
    same_as_reference("built", &builder, &def, &options, &full).unwrap();

    // The recolour reads no asset, but the entry it makes is only good where the asset can be read.
    def.palette.0.insert(slot("outfit"), Colour::rgb(9, 8, 7));
    let t = same_as_reference("recoloured", &builder, &def, &options, &full).unwrap();
    assert!(recoloured(&t).contains("stripe"), "{:?}", recoloured(&t));
    let empty = BuildServices::default().with_assets(MemAssets::new()).with_rasterizer(|| Box::new(MockRasterizer));
    assert!(same_as_reference("an empty store", &builder, &def, &options, &empty).is_none());

    // And a swap where the asset can't be read does not recolour the cached stripe, it fails like a build.
    def.palette.0.insert(slot("outfit"), Colour::rgb(1, 1, 1));
    assert!(same_as_reference("a swap with an empty store", &builder, &def, &options, &empty).is_none());
}
