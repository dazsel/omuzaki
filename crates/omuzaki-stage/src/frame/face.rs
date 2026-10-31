//! The face system's per-frame update: layer painting, variant selection by head coverage, settling for goldens, and
//! palette changes (#201).
//!
//! A stage gets a [`FaceRig`] with [`Stage::set_face`]; from then on [`Stage::tick`] does two things for it:
//!
//! 1. **variant swap**, first, before the rig solve: a variant prepared earlier (see below) replaces the character in
//!    one step, with pointer swaps and nothing built, allocated or locked. The frame then solves, steps and draws the
//!    new character with its own input, so the swapping frame shows no skeleton or simulation from an earlier frame;
//! 2. **map and paint**, in step 5: [`FaceSystem::update`] maps the bus with the palette-resolved profile and says which
//!    layers changed; exactly those are painted into their textures with [`FaceLayerPainter`]. With the
//!    [`standard`](FaceRig::standard) rig on the wgpu renderer and a hardware adapter, that paint is the GPU rasteriser
//!    recording into the renderer's own encoder through `PaintTarget::native` (#2074): no pixels cross to the CPU. A
//!    layer the GPU rasteriser asks for ([`FaceSystem::gpu_repaint_needed`]: an earlier GPU paint of it turned out, a
//!    frame or two later, to have outgrown a buffer) is painted too, although it did not change.
//!
//! # Variant switches never run inside `tick`
//!
//! The head's size on screen ([`head_coverage`](Stage::head_coverage)) goes through `select_variant` (the rule behind
//! `VariantSelector`, which borrows its set and so can't live in the stage). The frame thread calls
//! [`Stage::service_variants`] once per frame, outside `tick` (before or after it), and that does the switch in three
//! stages while the current variant keeps drawing:
//!
//! 1. **build** (off the frame thread): a new pick goes to the rig's [`SwitchExecutor`] as a job that builds the variant
//!    through the [`CachingBuilder`], resets the part runtimes and takes the [`SETTLE_STEPS`] warm-up steps (480 steps
//!    at once is far too long for a frame). [`ThreadExecutor`] is the default; tests use [`InlineExecutor`] or their own;
//! 2. **stage** (frame thread, in `service_variants`, once the job is done): the renderer sync
//!    (only what differs is sent to the renderer, as in [`StageCharacter::sync`]), the frame plans, the face system and a refresh of
//!    the dynamic meshes' vertices are prepared beside the live ones, which are untouched. The scene's instance and joint
//!    buffers grow here to the new character's size, so the swapping frame's scene fill doesn't;
//! 3. **swap** (at the start of the next `tick`): the staged character replaces the live one by moving tables and
//!    handles. The old one goes to a retiring slot and is released by the next `service_variants`.
//!
//! The switch costs the frame thread the staging work in one `service_variants` call and a swap in the next `tick`. The
//! warmed simulations are the ones of the request: hair and cloth are a few frames behind when the swap happens.
//! A pending switch is dropped by [`Stage::apply_palette`] and [`Stage::resize_face`] (both change what a staged
//! character was prepared against); the next `service_variants` asks again. [`Stage::set_face`] drops the old rig's
//! pending switch and releases what its last swap retired.
//!
//! A layer whose paint failed stays marked and is retried every frame until it succeeds, so a failing painter keeps the
//! old pixels on screen and never leaves the layer stale for good.

use std::mem;
use std::sync::Arc;
use std::sync::mpsc;

use omuzaki_anim_api::{ChannelBuf, HumanoidPose, IkGoals, Reaches, Tick};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, CharacterRig, JointRef,
    StepCtx, VariantId, VariantSet, resolve_variant, select_variant,
};
use omuzaki_core::glam::{Mat4, Vec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face::raster::cpu::SkiaRasterizer;
#[cfg(not(target_arch = "wasm32"))]
use omuzaki_face::raster::gpu::VelloRasterizer;
use omuzaki_face::{FaceSystem, GpuRasterizer, RealPainter};
use omuzaki_face_api::{ExpressionSet, FaceLayerId, FaceProfile, FaceProjection, FaceView, Palette};
use omuzaki_render_api::{Projection, Renderer};

use super::rig::{RigFrame, SkinPose};
use super::runtimes::DynamicMeshes;
use super::scene::ScenePlan;
use super::{FrameInput, Stage};
use crate::error::StageError;
use crate::upload::{MeshLookup, PreparedSync, Retired, SyncReport};

/// Steps [`Stage::settle`] takes to warm hair and cloth before a golden frame.
pub const SETTLE_STEPS: u32 = 480;
/// The fixed step of [`Stage::settle`], seconds (90 Hz).
pub const SETTLE_DT: f32 = 1.0 / 90.0;

/// Makes the face system for a projection and a base density: called when the stage binds a face, when the head's
/// projection changes with a variant, and when [`Stage::resize_face`] changes the density.
pub type FaceFactory = Box<dyn Fn(&FaceProjection, f32) -> FaceSystem + Send>;

/// A unit of work for a [`SwitchExecutor`].
pub type Job = Box<dyn FnOnce() + Send + 'static>;

/// Where a variant build runs. The stage hands it one [`Job`] per switch and polls for the result, so an executor only
/// has to run the job somewhere, now or later.
pub trait SwitchExecutor: Send {
    /// Runs `job`, on this thread before returning or on another one. A job that is dropped without running makes the
    /// switch fail with [`StageError::Rebuild`].
    fn run(&self, job: Job);
}

/// Runs the job on the calling thread, before `run` returns: deterministic, for tests and offline renders.
#[derive(Copy, Clone, Debug, Default)]
pub struct InlineExecutor;

impl SwitchExecutor for InlineExecutor {
    fn run(&self, job: Job) {
        job();
    }
}

/// Runs each job on a new thread: the live default.
#[derive(Copy, Clone, Debug, Default)]
pub struct ThreadExecutor;

impl SwitchExecutor for ThreadExecutor {
    fn run(&self, job: Job) {
        if let Err(error) = std::thread::Builder::new().name("omuzaki-variant-build".into()).spawn(job) {
            // The job is gone with the failed spawn; its sender drops, which the stage reports as a failed switch.
            tracing::warn!(%error, "starting the variant build thread failed");
        }
    }
}

/// What a build job answers: the warmed character and its face's expression table, or why it could not be made.
type BuildResult = Result<Warmed, String>;

/// A variant built and warmed off the frame thread.
struct Warmed {
    built: BuiltCharacter,
    /// The new face's expressions, shared: built in the job so that the swap only moves a pointer. `None` without a face.
    expressions: Option<Arc<ExpressionSet>>,
}

/// A character's face expressions, shared: what [`Stage::face_expressions`] hands out.
fn expressions_of(built: &BuiltCharacter) -> Option<Arc<ExpressionSet>> {
    built.face.as_ref().map(|face| Arc::new(face.profile.expressions.clone()))
}

/// Everything a variant build needs, owned so it can run off the frame thread.
struct WarmJob {
    builder: Arc<CachingBuilder>,
    id: CharId,
    def: CharacterDef,
    options: BuildOptions,
    services: BuildServices,
    /// The live character's rig: decides whether the new one needs its own solve to warm against.
    old_rig: CharacterRig,
    /// The live character's skinning matrices, for a new character with the same rig.
    skin: Vec<Mat4>,
    root: Mat4,
    tick: Tick,
    pose: HumanoidPose,
    goals: IkGoals,
    reach: Reaches,
    /// The frame's channels, for the new rig's bone drivers (boxed: 512 bytes).
    channels: Box<ChannelBuf>,
    steps: u32,
}

impl WarmJob {
    /// Builds the variant, warms its runtimes (reset under the pose, then `steps` steps) and shares its expressions.
    fn run(self) -> BuildResult {
        let mut built =
            self.builder.build(&self.id, &self.def, &self.options, &self.services).map_err(|e| e.to_string())?;
        // The same rule as the live install: a new rig is solved against the frame's pose, an equal one keeps the
        // live skin (its spring bones carry state a fresh solve would lose).
        let solved = if built.rig == self.old_rig {
            None
        } else {
            let mut frame = RigFrame::for_character(&built).map_err(|e| e.to_string())?;
            frame.set_reaches(&self.reach);
            frame.solve_with_channels(self.tick, &self.pose, &self.goals, &self.channels);
            Some(frame.skin().to_vec())
        };
        let skin = solved.as_deref().unwrap_or(&self.skin);
        warm_runtimes(&mut built, skin, self.root, self.steps);
        let expressions = expressions_of(&built);
        Ok(Warmed { built, expressions })
    }
}

/// How the face binds once a staged character is live.
enum NextBound {
    /// The projection is the live one's: the face system stays, with the new profile and head.
    Keep(Box<Resolved>),
    /// A new projection: a face system made while staging.
    Fresh(Box<Bound>),
    /// The new character has no face.
    Unbound,
}

/// A variant fully prepared beside the live character: swapping it in only moves things.
struct Staged {
    new: BuiltCharacter,
    new_rig: Option<RigFrame>,
    bound: NextBound,
    prepared: PreparedSync,
    dynamic: DynamicMeshes,
    plan: ScenePlan,
    /// Face layers whose texture is new and so owed a paint.
    fresh: [bool; FaceLayerId::COUNT],
}

/// What a swap replaced, kept until the next [`Stage::service_variants`] drops it and releases its renderer resources,
/// so the swap itself calls no renderer release and frees no large table.
#[derive(Default)]
struct Retiring {
    built: Option<BuiltCharacter>,
    rig: Option<RigFrame>,
    dynamic: Option<DynamicMeshes>,
    plan: Option<ScenePlan>,
    bound: Option<Bound>,
    /// What a kept face system's old profile and expressions were.
    face: Option<(FaceProfile, Arc<ExpressionSet>)>,
    retired: Option<Retired>,
}

/// What rebuilds the character: the builder with its cache, the character before any variant, and the variants.
pub struct CharacterSource {
    builder: Arc<CachingBuilder>,
    base: CharacterDef,
    variants: VariantSet,
    options: BuildOptions,
    services: BuildServices,
}

impl CharacterSource {
    /// A source building `base` (the character without a variant) and its `variants` with `builder`.
    pub fn new(
        builder: Arc<CachingBuilder>,
        base: CharacterDef,
        variants: VariantSet,
        options: BuildOptions,
        services: BuildServices,
    ) -> Self {
        Self { builder, base, variants, options, services }
    }
}

/// Where the head is and how big, to read how much of the frame it fills.
struct Head {
    /// The head bone's index in the skin matrices.
    bone: usize,
    /// The bone's position in bind space.
    bind_position: Vec3,
    /// The head's height in metres: the face window's, which the head part fits to the head.
    height: f32,
}

/// The face bound to one built character.
struct Bound {
    system: FaceSystem,
    /// The face profile with palette slots resolved.
    profile: FaceProfile,
    /// The profile's expressions, shared ([`Stage::face_expressions`]).
    expressions: Arc<ExpressionSet>,
    projection: FaceProjection,
    head: Option<Head>,
}

/// What binding a rig to a character needs from it, worked out before anything is committed.
struct Resolved {
    profile: FaceProfile,
    expressions: Arc<ExpressionSet>,
    projection: FaceProjection,
    head: Option<Head>,
}

/// The face of a stage: the face system and everything that rebuilds the character when the head's size or the palette
/// asks for it.
pub struct FaceRig {
    factory: FaceFactory,
    /// The GPU rasteriser waiting for the first face system. From then on it moves from each system to the one that
    /// replaces it (variant switches, density changes), so its compiled pipelines outlive face systems.
    gpu: Option<Box<dyn GpuRasterizer>>,
    source: Option<CharacterSource>,
    settle_steps: u32,
    bound: Option<Bound>,
    /// Layers whose picture changed and whose texture was not painted yet (or failed to be).
    unpainted: [bool; FaceLayerId::COUNT],
    /// How many times each layer was painted since the rig was made: what a timing tool reads to say what a frame's face
    /// paint did.
    paints: [u64; FaceLayerId::COUNT],
    /// Where variant builds run.
    executor: Box<dyn SwitchExecutor>,
    /// The tick of the last frame: the build job solves a changed rig at it.
    last_tick: Option<Tick>,
    /// The bus of the last frame: a face system made between frames warms against it.
    last_channels: ChannelBuf,
    /// The answer to the build job in flight, if any.
    building: Option<mpsc::Receiver<BuildResult>>,
    /// A prepared variant waiting for the next `tick` to swap it in.
    staged: Option<Staged>,
    retiring: Retiring,
}

impl FaceRig {
    /// A rig making its face systems with `factory`.
    pub fn new(factory: FaceFactory) -> Self {
        Self {
            factory,
            gpu: None,
            source: None,
            settle_steps: SETTLE_STEPS,
            bound: None,
            unpainted: [false; FaceLayerId::COUNT],
            paints: [0; FaceLayerId::COUNT],
            executor: Box::new(ThreadExecutor),
            last_tick: None,
            last_channels: ChannelBuf::NEUTRAL,
            building: None,
            staged: None,
            retiring: Retiring::default(),
        }
    }

    /// The production rig: the real painter (kept by value, so the GPU rasteriser repaints only the sections that
    /// changed), the CPU rasteriser as the fallback, and on native targets the GPU rasteriser, which draws straight into
    /// the renderer's face textures through `PaintTarget::native` when the renderer is the wgpu one on a hardware adapter
    /// (`VelloRasterizer::hardware_only`); anywhere else the CPU raster is written instead.
    pub fn standard() -> Self {
        let rig = Self::new(Box::new(|projection, px_per_m| {
            FaceSystem::real(RealPainter::new(*projection, px_per_m), Box::new(SkiaRasterizer::new()))
        }));
        #[cfg(not(target_arch = "wasm32"))]
        let rig = rig.with_gpu_rasterizer(Box::new(VelloRasterizer::hardware_only()));
        rig
    }

    /// Draws the face layers with `gpu`: it is installed on the first face system (replacing any rasteriser the factory
    /// gave that system), then handed from each system to the one that replaces it. It replaces the rig's own, so
    /// `FaceRig::standard().with_gpu_rasterizer(..)` swaps the production GPU rasteriser for another.
    #[must_use]
    pub fn with_gpu_rasterizer(mut self, gpu: Box<dyn GpuRasterizer>) -> Self {
        self.gpu = Some(gpu);
        self
    }

    /// Gives `new` the GPU rasteriser of the system it replaces, or the rig's own if this is the first system or the
    /// last one had none. Moving a box allocates nothing, so this is safe inside the swap.
    fn hand_over_gpu(&mut self, old: Option<&mut FaceSystem>, new: &mut FaceSystem) {
        if let Some(gpu) = old.and_then(FaceSystem::take_gpu_rasterizer).or_else(|| self.gpu.take()) {
            new.set_gpu_rasterizer(Some(gpu));
        }
    }

    /// Keeps the GPU rasteriser of a face system that goes away with no successor, for the next one.
    fn keep_gpu(&mut self, old: &mut FaceSystem) {
        if let Some(gpu) = old.take_gpu_rasterizer() {
            self.gpu = Some(gpu);
        }
    }

    /// Lets the stage switch variants and apply palettes by rebuilding from `source`.
    #[must_use]
    pub fn with_source(mut self, source: CharacterSource) -> Self {
        self.source = Some(source);
        self
    }

    /// Steps taken to settle after a variant or palette change (default [`SETTLE_STEPS`]).
    #[must_use]
    pub fn with_settle_steps(mut self, steps: u32) -> Self {
        self.settle_steps = steps;
        self
    }

    /// Runs variant builds on `executor` instead of a new thread per switch ([`ThreadExecutor`], the default).
    #[must_use]
    pub fn with_executor(mut self, executor: Box<dyn SwitchExecutor>) -> Self {
        self.executor = executor;
        self
    }

    /// Binds the rig to `built` at `px_per_m`: [`resolve`](Self::resolve), then [`commit_bound`](Self::commit_bound).
    fn bind(&mut self, built: &BuiltCharacter, px_per_m: f32) -> Result<(), StageError> {
        let resolved = Self::resolve(built, None)?;
        self.commit_bound(resolved, px_per_m);
        Ok(())
    }

    /// The part of binding that can fail: the face profile with its palette slots resolved, its expressions shared
    /// (`expressions`, when a build job already made them, else made here) and the head. `None` for a character without
    /// a face.
    fn resolve(built: &BuiltCharacter, expressions: Option<Arc<ExpressionSet>>) -> Result<Option<Resolved>, StageError> {
        let Some(face) = &built.face else { return Ok(None) };
        let profile = face
            .profile
            .resolve_colours(&built.palette)
            .map_err(|source| StageError::Face { reason: source.to_string() })?;
        let expressions = expressions.unwrap_or_else(|| Arc::new(face.profile.expressions.clone()));
        Ok(Some(Resolved { profile, expressions, projection: face.projection, head: head_of(built, &face.projection) }))
    }

    /// Binds `resolved` (cannot fail): makes a new face system if there is none or the projection changed (else the old
    /// one keeps its fingerprints, so only layers whose picture changed repaint). No face unbinds it.
    fn commit_bound(&mut self, resolved: Option<Resolved>, px_per_m: f32) {
        let Some(Resolved { profile, expressions, projection, head }) = resolved else {
            if let Some(mut old) = self.bound.take() {
                self.keep_gpu(&mut old.system);
            }
            return;
        };
        let system = match self.bound.take() {
            Some(old) if old.projection == projection => old.system,
            old => {
                let mut system = (self.factory)(&projection, px_per_m);
                let mut old = old.map(|old| old.system);
                self.hand_over_gpu(old.as_mut(), &mut system);
                system
            }
        };
        self.bound = Some(Bound { system, profile, expressions, projection, head });
    }

    /// Readies a face system made between frames for the next [`tick`](Stage::tick): its first update (which sets up
    /// its layer slots, drawing buffers and rasters) runs here against the last frame's bus, and every layer is owed a
    /// paint, as its fingerprints now say the picture was shown. A no-op without a bound face.
    ///
    /// # Errors
    /// [`StageError::Face`] if the painter or the rasteriser fails; the layers stay owed and the next frame retries.
    fn warm_bound(&mut self) -> Result<(), StageError> {
        let time = self.last_tick.map_or(Seconds(0.0), |tick| tick.time);
        let Some(bound) = self.bound.as_mut() else { return Ok(()) };
        self.unpainted = [true; FaceLayerId::COUNT];
        warm_face(&mut bound.system, &bound.profile, &self.last_channels, time)
    }
}

/// A new face system's first update, run outside `tick` so the frame's update allocates nothing.
fn warm_face(
    system: &mut FaceSystem,
    profile: &FaceProfile,
    channels: &ChannelBuf,
    time: Seconds,
) -> Result<(), StageError> {
    let view = FaceView { highlight_shift: Vec2::ZERO, head_coverage: None };
    system.update(channels, profile, time, view).map(|_| ()).map_err(face_error)
}

/// The head bone and size of `built`, if it has a head bone.
fn head_of(built: &BuiltCharacter, projection: &FaceProjection) -> Option<Head> {
    let bone = built.rig.bone(&JointRef::HEAD)?.index();
    let bind = built.rig.inverse_bind().get(bone)?.inverse();
    Some(Head { bone, bind_position: bind.w_axis.truncate(), height: projection.rect.y1 - projection.rect.y0 })
}

fn face_error(source: impl std::fmt::Display) -> StageError {
    StageError::Face { reason: source.to_string() }
}

fn rebuild_error(source: impl std::fmt::Display) -> StageError {
    StageError::Rebuild { reason: source.to_string() }
}

/// Steps every runtime `steps` times at [`SETTLE_DT`] under the given pose, with no wind.
fn step_runtimes(built: &mut BuiltCharacter, skin: &[Mat4], root: Mat4, steps: u32) {
    let pose = SkinPose { character: &built.rig, skin, root };
    for step in 0..steps {
        let time = Seconds(f64::from(step) * f64::from(SETTLE_DT));
        let ctx = StepCtx::new(SETTLE_DT, time, &pose, Vec3::ZERO);
        DynamicMeshes::step(&mut built.runtimes, &ctx);
    }
}

/// Resets every runtime to the rest shape under the given pose, then steps them `steps` times ([`step_runtimes`]).
fn warm_runtimes(built: &mut BuiltCharacter, skin: &[Mat4], root: Mat4, steps: u32) {
    let pose = SkinPose { character: &built.rig, skin, root };
    for slot in &mut built.runtimes {
        slot.runtime.reset(&pose);
    }
    step_runtimes(built, skin, root, steps);
}

/// Throws away a pending variant switch: aborts what was staged on the renderer and releases what a swap retired.
pub(super) fn discard_switch(renderer: &mut dyn Renderer, rig: &mut FaceRig) {
    rig.building = None;
    if let Some(staged) = rig.staged.take() {
        staged.prepared.abort(renderer);
    }
    release_retiring(renderer, rig);
}

/// Drops what the last swap replaced and gives its renderer resources back. A failed release is logged: the new
/// character is long in place and there is no one left to tell.
fn release_retiring(renderer: &mut dyn Renderer, rig: &mut FaceRig) {
    let Retiring { built, rig: old_rig, dynamic, plan, bound, face, retired } = mem::take(&mut rig.retiring);
    // Dropped here, off the tick.
    drop((built, old_rig, dynamic, plan, bound, face));
    if let Some(retired) = retired
        && let Err(error) = retired.release(renderer)
    {
        tracing::warn!(%error, "releasing the old variant's resources failed");
    }
}

impl Stage {
    /// How many times each face layer has been painted since the face was set, indexed by [`FaceLayerId`]; zeros
    /// without a face.
    #[must_use]
    pub fn face_paints(&self) -> [u64; FaceLayerId::COUNT] {
        self.face.as_ref().map_or([0; FaceLayerId::COUNT], |rig| rig.paints)
    }

    /// Attaches the face system and binds it to the current character. Replaces an earlier rig: its pending variant
    /// switch is aborted on `renderer` (staged resources given back, repointed materials restored) and what its last
    /// swap retired is released.
    ///
    /// # Errors
    /// [`StageError::Face`] if the face profile names a palette slot the palette lacks; the earlier rig then stays,
    /// with its pending switch.
    pub fn set_face(&mut self, renderer: &mut dyn Renderer, mut rig: FaceRig) -> Result<(), StageError> {
        rig.bind(&self.built, self.character.face_px_per_m())?;
        if let Some(mut old) = self.face.replace(Box::new(rig)) {
            discard_switch(renderer, &mut old);
        }
        Ok(())
    }

    /// The live face's expression table, shared; `None` without a face rig or for a character without a face.
    ///
    /// For the triggers (`perform`), which bake key bindings against it: holding a clone of the `Arc` costs a counter
    /// increment, with no copy of the table, no lock and no allocation, so it can be taken in the frame loop's cold step
    /// after every variant swap. The table is built where the character is: by a variant switch's build job, off the
    /// frame thread, or by the cold calls that bind a face ([`set_face`](Self::set_face),
    /// [`apply_palette`](Self::apply_palette), [`resize_face`](Self::resize_face)); the swap only stores the pointer.
    /// A new table is a new `Arc` (`Arc::ptr_eq` tells whether it changed); the old one is dropped by the next
    /// [`service_variants`](Self::service_variants), not in the swapping tick.
    pub fn face_expressions(&self) -> Option<&Arc<ExpressionSet>> {
        self.face.as_ref()?.bound.as_ref().map(|bound| &bound.expressions)
    }

    /// Tells the face that the renderer was rebuilt after a device loss: its GPU rasteriser drops everything it made on
    /// the old device (`GpuRasterizer::device_lost`), and every face layer is painted again on the next
    /// [`tick`](Stage::tick). Call it before that tick: the rasteriser cannot always tell a rebuilt device from the old one
    /// (wgpu numbers resources per instance). A no-op without a face rig.
    pub fn face_device_lost(&mut self) {
        let Some(rig) = self.face.as_mut() else { return };
        if let Some(gpu) = rig.gpu.as_mut() {
            gpu.device_lost();
        }
        if let Some(bound) = rig.bound.as_mut() {
            bound.system.gpu_device_lost();
        }
        rig.unpainted = [true; FaceLayerId::COUNT];
    }

    /// Warms the rig's spring bones and the part runtimes (hair, cloth, tails) by `steps` steps of [`SETTLE_DT`], under
    /// the last frame's pose, goals, root and bone-driver channels, so the first frame doesn't start from a rest shape
    /// ([`SETTLE_STEPS`] is the usual count). Each step solves the rig and then steps the runtimes on the solved
    /// skeleton. The stage clock doesn't move and nothing is sent to the renderer: the next [`tick`](Stage::tick)
    /// uploads the meshes that moved.
    pub fn settle(&mut self, steps: u32) {
        for step in 0..steps {
            let time = Seconds(f64::from(step) * f64::from(SETTLE_DT));
            let tick = Tick { index: u64::from(step), time, dt: SETTLE_DT };
            self.rig.solve(tick, &self.pose, &self.goals);
            let pose = SkinPose { character: &self.built.rig, skin: self.rig.skin(), root: self.root };
            let ctx = StepCtx::new(SETTLE_DT, time, &pose, Vec3::ZERO);
            DynamicMeshes::step(&mut self.built.runtimes, &ctx);
        }
    }

    /// [`settle`](Stage::settle) from a clean start: the spring bones and the part runtimes are reset to rest under the
    /// last frame's pose, goals, root and bone-driver channels, so the result depends on those and `steps` alone, not
    /// on the frames before it (a sheet or a still must not depend on history). The stage clock doesn't move and
    /// nothing is sent to the renderer: [`redraw`](Stage::redraw) or the next [`tick`](Stage::tick) uploads the meshes
    /// that moved.
    pub fn settle_from_rest(&mut self, steps: u32) {
        self.rig.reset();
        // One solve at the last frame's pose, for no time at all: the springs start from rest and the runtimes reset
        // against that skin, and only the requested steps advance anything, whatever the stage clock's frame time is.
        let still = Tick { dt: 0.0, ..self.tick };
        self.rig.solve(still, &self.pose, &self.goals);
        let pose = SkinPose { character: &self.built.rig, skin: self.rig.skin(), root: self.root };
        for slot in &mut self.built.runtimes {
            slot.runtime.reset(&pose);
        }
        self.settle(steps);
    }

    /// Changes the face texture density (the frame-budget scheduler's knob) and repaints every layer on the next frame.
    ///
    /// # Errors
    /// As [`StageCharacter::resize_face`], and [`StageError::Face`] if the profile can't be resolved.
    pub fn resize_face(&mut self, renderer: &mut dyn Renderer, px_per_m: f32) -> Result<(), StageError> {
        if let Some(rig) = self.face.as_mut() {
            // A staged character was prepared against the old density.
            discard_switch(renderer, rig);
        }
        let before = self.character.face_px_per_m();
        let resized = self.character.resize_face(renderer, &self.built, px_per_m);
        // A failed release of the old textures comes back after the new ones are in force: the face system must follow
        // the density that actually changed, and the release error is still the caller's to hear.
        if self.character.face_px_per_m().to_bits() != before.to_bits()
            && let Some(rig) = self.face.as_mut()
        {
            // A new system, because the painter's layer specs carry the density. It is warmed here, not in the next
            // tick, and every layer is owed a paint: the new textures are empty. The GPU rasteriser carries over.
            if let Some(mut old) = rig.bound.take() {
                rig.keep_gpu(&mut old.system);
            }
            let bound = rig.bind(&self.built, self.character.face_px_per_m()).and_then(|()| rig.warm_bound());
            resized?;
            return bound;
        }
        resized
    }

    /// Applies `palette` to the character: materials and face colours are re-resolved, the renderer's materials are
    /// updated in place, and no mesh is created unless a part baked a colour into its vertices. The simulations restart
    /// and settle, as after any rebuild. Needs a [`CharacterSource`] on the rig.
    ///
    /// # Errors
    /// [`StageError::Rebuild`] without a source or if the builder refuses the palette (a part names a slot it lacks),
    /// [`StageError::Render`] if the renderer refuses a call. On any error but a failed release of the old resources
    /// nothing has changed (the previous palette stays in force and a retry is sound); that one comes back after the
    /// palette took effect.
    pub fn apply_palette(&mut self, renderer: &mut dyn Renderer, palette: &Palette) -> Result<SyncReport, StageError> {
        let mut rig = self.face.take().ok_or_else(|| rebuild_error("no face rig attached: call set_face first"))?;
        // A staged variant was built with the old palette.
        discard_switch(renderer, &mut rig);
        let result = self.rebuild_for_palette(renderer, &mut rig, palette);
        self.face = Some(rig);
        result
    }

    fn rebuild_for_palette(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        palette: &Palette,
    ) -> Result<SyncReport, StageError> {
        let source = rig.source.as_ref().ok_or_else(|| rebuild_error("the face rig has no CharacterSource"))?;
        let mut def = self.built.def.clone();
        def.palette = palette.clone();
        let options = BuildOptions { variant: self.built.variant.clone(), ..source.options.clone() };
        let new = source.builder.build(&self.built.id, &def, &options, &source.services).map_err(rebuild_error)?;
        let (report, cleanup) = self.install(renderer, rig, new)?;
        // Only now is the palette the character's: a rejected install must not leak it into the next variant switch.
        if let Some(source) = rig.source.as_mut() {
            source.base.palette = palette.clone();
        }
        // The palette is in effect; a failed release of the old resources is the caller's to hear about.
        cleanup.map(|()| report)
    }

    /// The start of [`tick`](Stage::tick): swaps in a variant staged by [`service_variants`](Self::service_variants),
    /// before the rig solve, so the frame solves, steps and draws the new character with this frame's input. A no-op
    /// without a rig or a staged variant.
    ///
    /// # Errors
    /// As [`swap_in_staged`](Self::swap_in_staged); the live character then stays, untouched.
    pub(super) fn swap_variant(&mut self, renderer: &mut dyn Renderer) -> Result<(), StageError> {
        let Some(mut rig) = self.face.take() else { return Ok(()) };
        let result = self.swap_in_staged(renderer, &mut rig);
        self.face = Some(rig);
        result
    }

    /// The face channels the last frame painted and that frame's tick; `None` without a face rig or before any frame.
    /// What a caller that borrows the stage for its own frames (a character sheet) gives back with
    /// [`repaint_face`](Stage::repaint_face).
    pub(crate) fn face_state(&self) -> Option<(ChannelBuf, Tick)> {
        let rig = self.face.as_ref()?;
        Some((rig.last_channels, rig.last_tick?))
    }

    /// Maps and paints the face for `channels` at `tick`, as a frame would, without drawing a frame: the face textures
    /// and the rig's last channels are then what they were when [`face_state`](Stage::face_state) was read. A no-op
    /// without a rig.
    ///
    /// # Errors
    /// What painting the face reports.
    pub(crate) fn repaint_face(
        &mut self,
        renderer: &mut dyn Renderer,
        channels: &ChannelBuf,
        tick: Tick,
    ) -> Result<(), StageError> {
        self.update_face(renderer, tick, &FrameInput { channels: *channels, ..FrameInput::default() })
    }

    /// Step 5 of [`tick`](Stage::tick): map and paint. A no-op without a rig.
    pub(super) fn update_face(
        &mut self,
        renderer: &mut dyn Renderer,
        tick: Tick,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        let Some(mut rig) = self.face.take() else { return Ok(()) };
        let result = self.drive_face(renderer, &mut rig, tick, input);
        self.face = Some(rig);
        result
    }

    fn drive_face(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        tick: Tick,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        rig.last_tick = Some(tick);
        rig.last_channels = input.channels;
        let head = rig.bound.as_ref().and_then(|b| b.head.as_ref());
        let highlight_shift = head.map_or(Vec2::ZERO, |h| self.highlight(h));
        // A share of the frame, well inside f32.
        #[allow(clippy::cast_possible_truncation)]
        let head_coverage = head.and_then(|h| self.head_coverage(h)).map(|c| c as f32);
        let Some(bound) = rig.bound.as_mut() else { return Ok(()) };
        let view = FaceView { highlight_shift, head_coverage };
        let update = bound.system.update(&input.channels, &bound.profile, tick.time, view).map_err(face_error)?;
        for layer in update.changed_layers() {
            rig.unpainted[layer as usize] = true;
        }
        let mut first_error = None;
        for layer in FaceLayerId::ALL {
            // A GPU paint of an earlier frame can turn out, a frame or two later, to have drawn the layer wrong (it
            // outgrew a GPU buffer): repaint it although the face did not change, or a face that stopped changing would
            // keep the wrong picture.
            if bound.system.gpu_repaint_needed(layer) {
                rig.unpainted[layer as usize] = true;
            }
            if !rig.unpainted[layer as usize] {
                continue;
            }
            // A layer no material composites has no texture: nothing to paint.
            let Some(texture) = self.character.face_layer(layer).map(|l| l.texture) else {
                rig.unpainted[layer as usize] = false;
                continue;
            };
            let Some(mut painter) = bound.system.layer_painter(layer) else { continue };
            match renderer.paint_texture(texture, &mut painter) {
                Ok(()) => {
                    rig.unpainted[layer as usize] = false;
                    rig.paints[layer as usize] += 1;
                }
                Err(source) => {
                    // What a GPU rasteriser recorded for this paint was dropped: its retry must redraw the layer whole.
                    bound.system.gpu_paint_failed(layer);
                    first_error.get_or_insert_with(|| {
                        StageError::render(format!("painting the {layer:?} face layer"), source)
                    });
                }
            }
        }
        first_error.map_or(Ok(()), Err)
    }

    /// The variant the head's size now calls for, if it differs from the one built. `None` without a source, a head, or
    /// a variant covering the size (the current one then stays).
    fn variant_to_switch_to(&self, rig: &FaceRig) -> Option<VariantId> {
        let source = rig.source.as_ref()?;
        let coverage = self.head_coverage(rig.bound.as_ref()?.head.as_ref()?)?;
        let pick = select_variant(&source.variants, coverage, self.built.variant.as_ref())?;
        (self.built.variant.as_ref() != Some(pick)).then(|| pick.clone())
    }

    /// Where the eyes' highlights slide for the scene's camera and the head of the last solve
    /// ([`highlight_shift`](omuzaki_anim::gaze::highlight_shift)): the camera seen from the head, in the head's bind
    /// space, which faces `+z` with the character's left at `+x`.
    fn highlight(&self, head: &Head) -> Vec2 {
        let Some(skin) = self.rig.skin().get(head.bone) else { return Vec2::ZERO };
        let to_head = (self.root * *skin).inverse();
        let camera = &self.scene.camera;
        let seen_from = match camera.projection {
            Projection::Perspective { .. } => to_head.transform_point3(camera.position()) - head.bind_position,
            Projection::Orthographic { .. } => to_head.transform_vector3(camera.transform.z_axis.truncate()),
        };
        omuzaki_anim::gaze::highlight_shift(seen_from)
    }

    /// The head's height over the frame's height at the head, from the camera and the head bone of the last solve.
    /// `None` for a head behind the camera.
    fn head_coverage(&self, head: &Head) -> Option<f64> {
        let model = self.rig.skin().get(head.bone)?.transform_point3(head.bind_position);
        let world = self.root.transform_point3(model);
        let camera = &self.scene.camera;
        let depth = (camera.position() - world).dot(camera.transform.z_axis.truncate());
        let frame_height = match camera.projection {
            Projection::Perspective { fov_y_deg, .. } if depth > 0.0 => {
                2.0 * depth * (fov_y_deg.to_radians() * 0.5).tan()
            }
            Projection::Perspective { .. } => return None,
            Projection::Orthographic { height, .. } => height,
        };
        let head_height = head.height * self.root.y_axis.truncate().length();
        (frame_height > f32::EPSILON).then(|| f64::from(head_height / frame_height))
    }

    /// Moves a variant switch along; call it once per frame, before or after [`tick`](Stage::tick), never inside it.
    /// This is where everything that builds, allocates or waits happens (see the module docs): it releases what the last
    /// swap retired, asks the rig's [`SwitchExecutor`] for a build when the head's size calls for another variant,
    /// and, once the build is done, prepares the renderer side. The next `tick` then swaps the new variant in; until
    /// then the current one keeps drawing. A no-op without a face rig, a [`CharacterSource`] or a frame run yet.
    ///
    /// # Errors
    /// [`StageError::Rebuild`] if the variant can't be built or the build job was lost, [`StageError::Render`] or
    /// others from preparing the renderer side. On an error nothing has changed and the next call asks again.
    pub fn service_variants(&mut self, renderer: &mut dyn Renderer, input: &FrameInput) -> Result<(), StageError> {
        let Some(mut rig) = self.face.take() else { return Ok(()) };
        let result = self.service(renderer, &mut rig, input);
        self.face = Some(rig);
        result
    }

    fn service(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        input: &FrameInput,
    ) -> Result<(), StageError> {
        release_retiring(renderer, rig);
        let Some(tick) = rig.last_tick else { return Ok(()) };
        if rig.building.is_none()
            && rig.staged.is_none()
            && let Some(target) = self.variant_to_switch_to(rig)
        {
            self.request_switch(rig, &target, input, tick)?;
        }
        let Some(answer) = rig.building.take() else { return Ok(()) };
        let warmed = match answer.try_recv() {
            Ok(result) => result.map_err(rebuild_error)?,
            Err(mpsc::TryRecvError::Empty) => {
                rig.building = Some(answer);
                return Ok(());
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                return Err(rebuild_error("the variant build ended without a result"));
            }
        };
        let staged = self.stage_switch(renderer, rig, warmed, input, tick)?;
        // The swapping frame fills the scene with the new plan: room for it is made here, not in that frame.
        staged.plan.reserve(&mut self.scene);
        rig.staged = Some(staged);
        Ok(())
    }

    /// Hands the build of `target` to the executor.
    fn request_switch(
        &self,
        rig: &mut FaceRig,
        target: &VariantId,
        input: &FrameInput,
        tick: Tick,
    ) -> Result<(), StageError> {
        let source = rig.source.as_ref().ok_or_else(|| rebuild_error("the face rig has no CharacterSource"))?;
        let def = resolve_variant(&source.base, &source.variants, target).map_err(rebuild_error)?;
        let job = WarmJob {
            builder: Arc::clone(&source.builder),
            id: self.built.id.clone(),
            def,
            options: BuildOptions { variant: Some(target.clone()), ..source.options.clone() },
            services: source.services.clone(),
            old_rig: self.built.rig.clone(),
            skin: self.rig.skin().to_vec(),
            root: self.root,
            tick,
            pose: input.pose,
            goals: input.goals,
            reach: input.reach,
            channels: Box::new(input.channels),
            steps: rig.settle_steps,
        };
        let (sender, answer) = mpsc::channel();
        rig.executor.run(Box::new(move || {
            // A receiver that is gone means the switch was discarded: nobody wants the answer.
            let _ = sender.send(job.run());
        }));
        rig.building = Some(answer);
        Ok(())
    }

    /// Prepares `warmed` (built and warmed by the job) beside the live character: everything that can fail or allocate.
    /// On an error nothing is left on the renderer and the live character is untouched.
    fn stage_switch(
        &self,
        renderer: &mut dyn Renderer,
        rig: &FaceRig,
        warmed: Warmed,
        input: &FrameInput,
        tick: Tick,
    ) -> Result<Staged, StageError> {
        let Warmed { built: new, expressions } = warmed;
        let mut new_rig = (new.rig != self.built.rig).then(|| RigFrame::for_character(&new)).transpose()?;
        if let Some(frame) = new_rig.as_mut() {
            // A first solve off the frame path; the swapping frame solves it again with its own input.
            frame.set_reaches(&input.reach);
            frame.solve_with_channels(tick, &input.pose, &input.goals, &input.channels);
        }
        let bound = match FaceRig::resolve(&new, expressions)? {
            None => NextBound::Unbound,
            Some(next) if rig.bound.as_ref().is_some_and(|live| live.projection == next.projection) => {
                NextBound::Keep(Box::new(next))
            }
            Some(Resolved { profile, expressions, projection, head }) => {
                let mut system = (rig.factory)(&projection, self.character.face_px_per_m());
                // The first update of a face system sets up its scratch; do it here so the swap tick's update does not.
                warm_face(&mut system, &profile, &input.channels, tick.time)?;
                NextBound::Fresh(Box::new(Bound { system, profile, expressions, projection, head }))
            }
        };
        let prepared = self.character.prepare_sync(renderer, &self.built, &new)?;
        let frames = DynamicMeshes::new(&new, &prepared).and_then(|dynamic| {
            let plan = ScenePlan::new(&new, &prepared)?;
            Ok((dynamic, plan))
        });
        let (dynamic, plan) = match frames {
            Ok(frames) => frames,
            Err(error) => {
                prepared.abort(renderer);
                return Err(error);
            }
        };
        // A new face system counts every layer's picture as shown (its warm-up update above), so all of them are owed a paint.
        let mut fresh = [matches!(bound, NextBound::Fresh(_)); FaceLayerId::COUNT];
        for layer in prepared.fresh_face_layers(&self.character) {
            fresh[layer as usize] = true;
        }
        Ok(Staged { new, new_rig, bound, prepared, dynamic, plan, fresh })
    }

    /// The swap: makes a staged variant the stage's character. Allocates and builds nothing: it sends the new
    /// character's current vertices to the renderer (a call per dynamic mesh), then only moves tables and handles; what
    /// it replaces waits in the retiring slot for [`service_variants`](Self::service_variants). Runs before the frame's
    /// rig solve ([`swap_variant`](Self::swap_variant)).
    ///
    /// # Errors
    /// [`StageError::Render`] if the renderer refuses the vertex refresh. The staged variant is then abandoned (its
    /// renderer resources are given back), the live character resends all its meshes, and the next
    /// `service_variants` asks again.
    fn swap_in_staged(&mut self, renderer: &mut dyn Renderer, rig: &mut FaceRig) -> Result<(), StageError> {
        let Some(staged) = rig.staged.take() else { return Ok(()) };
        // A mesh kept across the switch is shared with the live character: this is the first write of the new simulation.
        if let Err(error) = refresh_dynamic(&staged.new, &staged.prepared, renderer) {
            staged.prepared.abort(renderer);
            // The refresh may have rewritten meshes the live character shares: have its next step resend them.
            self.dynamic.resend_all();
            return Err(error);
        }
        let Staged { new, new_rig, bound, prepared, dynamic, plan, fresh } = staged;
        rig.retiring.retired = Some(self.character.commit_sync(prepared));
        self.stand = super::stand_matrix(&new);
        rig.retiring.built = Some(mem::replace(&mut self.built, new));
        if let Some(frame) = new_rig {
            rig.retiring.rig = Some(mem::replace(&mut self.rig, frame));
        }
        rig.retiring.dynamic = Some(mem::replace(&mut self.dynamic, dynamic));
        rig.retiring.plan = Some(mem::replace(&mut self.plan, plan));
        match bound {
            NextBound::Keep(next) => {
                if let Some(live) = rig.bound.take() {
                    let Resolved { profile, expressions, projection, head } = *next;
                    rig.bound = Some(Bound { system: live.system, profile, expressions, projection, head });
                    // The old profile and expression table are dropped with the rest of the old character, not here.
                    rig.retiring.face = Some((live.profile, live.expressions));
                } else {
                    // Staged against a face that has since gone: cannot be made here without allocating.
                    tracing::error!("a staged variant kept a face system the rig no longer has; the face is unbound");
                }
            }
            NextBound::Fresh(mut next) => {
                let mut live = rig.bound.take();
                rig.hand_over_gpu(live.as_mut().map(|live| &mut live.system), &mut next.system);
                rig.bound = Some(*next);
                rig.retiring.bound = live;
            }
            NextBound::Unbound => {
                let mut live = rig.bound.take();
                if let Some(live) = live.as_mut() {
                    rig.keep_gpu(&mut live.system);
                }
                rig.retiring.bound = live;
            }
        }
        // A new layer texture is empty, and the face system's fingerprints still say its picture was shown.
        for layer in FaceLayerId::ALL {
            if fresh[layer as usize] {
                rig.unpainted[layer as usize] = true;
            }
        }
        Ok(())
    }

    /// Makes `new` the stage's character, all or nothing.
    ///
    /// Everything that can fail runs first, against the prospective state: the rig, the profile, the renderer sync
    /// ([`StageCharacter::prepare_sync`]), the dynamic-mesh refresh, the frame plans. Then the commit swaps tables and
    /// cannot fail. On an `Err` the previous character is untouched and coherent (its handles live, its plans current),
    /// so a later retry starts from a sound stage.
    ///
    /// Returns the sync report and the outcome of releasing what the old character alone used: a failure there is
    /// reported but the new character is already in place.
    ///
    /// Only the palette change installs this way, off the frame thread's real-time path; variant switches are prepared
    /// by the build job and [`stage_switch`](Self::stage_switch) and swapped in by `tick`.
    fn install(
        &mut self,
        renderer: &mut dyn Renderer,
        rig: &mut FaceRig,
        mut new: BuiltCharacter,
    ) -> Result<(SyncReport, Result<(), StageError>), StageError> {
        let mut new_rig = (new.rig != self.built.rig).then(|| RigFrame::for_character(&new)).transpose()?;
        if let Some(frame) = new_rig.as_mut() {
            // The bone drivers go on reading the last frame's channels, as the live rig's do.
            frame.set_channels(self.rig.channels());
        }
        let resolved = FaceRig::resolve(&new, None)?;
        let prepared = self.character.prepare_sync(renderer, &self.built, &new)?;

        let skin = new_rig.as_ref().map_or_else(|| self.rig.skin(), RigFrame::skin);
        warm_runtimes(&mut new, skin, self.root, rig.settle_steps);
        let staged = DynamicMeshes::new(&new, &prepared).and_then(|dynamic| {
            let plan = ScenePlan::new(&new, &prepared)?;
            refresh_dynamic(&new, &prepared, renderer)?;
            Ok((dynamic, plan))
        });
        let (dynamic, plan) = match staged {
            Ok(staged) => staged,
            Err(error) => {
                prepared.abort(renderer);
                // The refresh may have rewritten meshes the old character shares: have its next step resend them.
                self.dynamic.resend_all();
                return Err(error);
            }
        };

        // Commit: nothing below can fail.
        let fresh: Vec<FaceLayerId> = prepared.fresh_face_layers(&self.character).collect();
        let retired = self.character.commit_sync(prepared);
        self.stand = super::stand_matrix(&new);
        self.built = new;
        if let Some(frame) = new_rig {
            self.rig = frame;
        }
        self.dynamic = dynamic;
        plan.reserve(&mut self.scene);
        self.plan = plan;
        rig.commit_bound(resolved, self.character.face_px_per_m());
        // A new layer texture is empty, and the face system's fingerprints still say its picture was shown.
        for layer in fresh {
            rig.unpainted[layer as usize] = true;
        }
        let report = retired.report;
        Ok((report, retired.release(renderer)))
    }
}

/// Sends the runtimes' current vertices for every dynamic mesh of `built`: a mesh kept across a rebuild still shows the
/// old simulation's last shape on the renderer.
fn refresh_dynamic(
    built: &BuiltCharacter,
    uploaded: &impl MeshLookup,
    renderer: &mut dyn Renderer,
) -> Result<(), StageError> {
    for slot in &built.runtimes {
        for index in 0..slot.runtime.dynamic_count() {
            let Some(view) = slot.runtime.dynamic_mesh(index) else { continue };
            let Some(mesh) = uploaded.mesh(&slot.part, view.mesh) else { continue };
            renderer
                .update_vertices(mesh.mesh, view.positions, view.normals)
                .map_err(|source| StageError::render("a dynamic mesh after a rebuild", source))?;
        }
    }
    Ok(())
}

/// How many steps [`Stage::face_points`] takes up from the outline's chin toward the mouth to find the head under it.
const CHIN_STEPS: u32 = 16;
/// How far up toward the mouth it looks at most, as a share of the way.
const CHIN_REACH: f32 = 0.5;

/// Where a painted face's layout points land in a picture (`docs/design/likeness-measures.md` D10, §5): pixels of the
/// target, origin top-left, x right, y down, pixel centres at .5. What `render.preview` reports as `face_landmarks`.
#[derive(Clone, Copy, Debug, PartialEq)]
#[non_exhaustive]
pub struct FacePoints {
    /// The centre of the character's right eye (on the picture's left in a front view).
    pub eye_right: Vec2,
    /// The centre of the character's left eye.
    pub eye_left: Vec2,
    /// The centre of the mouth line.
    pub mouth: Vec2,
    /// The chin: the bottom of the head outline, under the mouth.
    pub chin: Vec2,
}

impl FacePoints {
    /// Points given by hand, as a test or a caller that measured them elsewhere does.
    #[must_use]
    pub fn new(eye_right: Vec2, eye_left: Vec2, mouth: Vec2, chin: Vec2) -> Self {
        Self { eye_right, eye_left, mouth, chin }
    }

    /// The distance between the eye centres, pixels.
    #[must_use]
    pub fn iod(&self) -> f32 {
        self.eye_left.distance(self.eye_right)
    }
}

/// A mesh the base face layer is painted on, with the bind-space window that layer covers on it.
struct FaceCarrier<'a> {
    mesh: &'a omuzaki_char_api::BuiltMesh,
    window: omuzaki_face_api::PlanarWindow,
}

impl Stage {
    /// Where the face layout's eye centres, mouth centre and chin land in the picture the stage last drew for a target
    /// of `size`: each face-plane point is found on the head's front surface (the nearest surface the base face layer
    /// is projected onto, as the renderer paints it), skinned by the current joint palette like the triangle under it,
    /// placed by the character's root and projected through the scene's camera. Cold: walks the head's triangles.
    ///
    /// `None` for a character without a painted face (or whose head part's parameters don't read), a face point
    /// behind the camera or off the head, a face turned away from the camera, or an eye outside the picture.
    #[must_use]
    pub fn face_points(&self, size: omuzaki_core::glam::UVec2) -> Option<FacePoints> {
        let world = self.face_world_points()?;
        let camera = &self.scene.camera;
        #[allow(clippy::cast_precision_loss, reason = "target sides are far below 2^24")]
        let (width, height) = (size.x as f32, size.y as f32);
        let to_clip = camera.projection_matrix(width / height.max(1.0)) * camera.view();
        let view = camera.view();
        let mut pixels = [Vec2::ZERO; 4];
        for (pixel, world) in pixels.iter_mut().zip(world) {
            if view.transform_point3(world).z >= 0.0 {
                return None;
            }
            let ndc = to_clip.project_point3(world);
            *pixel = Vec2::new(ndc.x.midpoint(1.0) * width, (1.0 - ndc.y) * 0.5 * height);
        }
        // Turned away: the head's forward (+Z in bind space, where the face is projected from) points away from the
        // camera at the face.
        let head_bone = self.built.rig.bone(&JointRef::HEAD).map(omuzaki_anim_api::BoneId::index);
        let head_to_world = self.root * super::scene::joint_matrix(head_bone, self.rig.skin());
        let forward = head_to_world.transform_vector3(Vec3::Z);
        let mouth_world = world[2];
        let toward_camera = match camera.projection {
            Projection::Perspective { .. } => camera.position() - mouth_world,
            Projection::Orthographic { .. } => camera.transform.z_axis.truncate(),
        };
        if forward.dot(toward_camera) <= 0.0 {
            return None;
        }
        let inside = |p: Vec2| (0.0..=width).contains(&p.x) && (0.0..=height).contains(&p.y);
        if !(inside(pixels[0]) && inside(pixels[1])) {
            return None;
        }
        let [eye_right, eye_left, mouth, chin] = pixels;
        Some(FacePoints { eye_right, eye_left, mouth, chin })
    }

    /// Where the face layout's right eye, left eye, mouth and chin centres are in the world, in that order, as the
    /// stage last posed the character: each face-plane point found on the head's front surface (the nearest surface
    /// the base face layer is projected onto, as the renderer paints it), skinned by the current joint palette like
    /// the triangle under it and placed by the character's root. What a camera that places the face is solved
    /// against. Cold: walks the head's triangles.
    ///
    /// `None` for a character without a painted face (or whose head part's parameters don't read), or a point off
    /// the head.
    #[must_use]
    pub fn face_world_points(&self) -> Option<[Vec3; 4]> {
        let face = self.built.face.as_ref()?;
        let carriers: Vec<FaceCarrier<'_>> = self
            .built
            .meshes
            .iter()
            .filter_map(|mesh| {
                let material = self.built.materials.get(&mesh.mesh.material)?;
                let overlay = material.overlays.iter().find(|o| o.layer == FaceLayerId::Base)?;
                Some(FaceCarrier { mesh, window: overlay.window })
            })
            .collect();
        let part = self.built.def.parts.get(&carriers.first()?.mesh.part)?;
        let head: omuzaki_char_api::HeadParams = serde_json::from_value(part.params.clone()).ok()?;
        let layout = &face.profile.layout;
        let n = omuzaki_face_api::narrow;
        let plane = [
            Vec2::new(n(-layout.eye_spacing), n(layout.eye_y)),
            Vec2::new(n(layout.eye_spacing), n(layout.eye_y)),
            Vec2::new(0.0, n(layout.mouth_y)),
            Vec2::new(0.0, n(head.outline.chin_y())),
        ];
        let mut world = [Vec3::ZERO; 4];
        for (k, (point, at)) in world.iter_mut().zip(plane).enumerate() {
            // The chin sits on the outline's lowest point, which a head built at another size (a tall body, a long
            // neck) can leave just off its mesh: it is then taken at the lowest point of the head under it, found by
            // stepping up toward the mouth.
            *point = if k == 3 {
                (0..=CHIN_STEPS).find_map(|step| {
                    #[allow(clippy::cast_precision_loss, reason = "a few steps")]
                    let up = step as f32 / CHIN_STEPS as f32 * CHIN_REACH;
                    self.face_surface(&carriers, &face.projection, at.lerp(plane[2], up))
                })?
            } else {
                self.face_surface(&carriers, &face.projection, at)?
            };
        }
        Some(world)
    }

    /// The world position of face-plane point `at` (head-local, the painter's units) on the head's front: the surface
    /// nearest the front (largest bind `z`) of the carriers' triangles under the point's place in their window,
    /// skinned like that triangle. `None` when no triangle lies under it.
    fn face_surface(&self, carriers: &[FaceCarrier<'_>], projection: &FaceProjection, at: Vec2) -> Option<Vec3> {
        let rect = projection.rect;
        // Where `at` is across and up the face window, 0 to 1.
        let across = Vec2::new((at.x - rect.x0) / rect.width(), (at.y - rect.y0) / rect.height());
        // The nearest hit so far: its bind z, the carrier, the triangle's corners and their weights.
        let mut best: Option<(f32, usize, [usize; 3], Vec3)> = None;
        for (k, carrier) in carriers.iter().enumerate() {
            let window = &carrier.window;
            let point = window.min + across * (window.max - window.min);
            let positions = self.runtime_positions(carrier.mesh).unwrap_or(&carrier.mesh.mesh.geometry.positions);
            for triangle in carrier.mesh.mesh.geometry.indices.as_chunks::<3>().0 {
                let corners = triangle.map(|i| i as usize);
                let [Some(first), Some(second), Some(third)] = corners.map(|i| positions.get(i).copied()) else {
                    continue;
                };
                let Some(weights) = barycentric(point, first.truncate(), second.truncate(), third.truncate()) else {
                    continue;
                };
                let z = weights.dot(Vec3::new(first.z, second.z, third.z));
                if best.is_none_or(|(nearest, ..)| z > nearest) {
                    best = Some((z, k, corners, weights));
                }
            }
        }
        let (_, k, corners, weights) = best?;
        let mesh = &carriers.get(k)?.mesh.mesh;
        let positions = self.runtime_positions(carriers.get(k)?.mesh).unwrap_or(&mesh.geometry.positions);
        let bones = super::scene::joint_bones(&self.built, mesh);
        let skin = self.rig.skin();
        let mut model = Vec3::ZERO;
        for (corner, weight) in corners.into_iter().zip(weights.to_array()) {
            let p = *positions.get(corner)?;
            let posed = match (mesh.geometry.joints.get(corner), mesh.geometry.weights.get(corner)) {
                (Some(joints), Some(joint_weights)) => {
                    let blend = joints.iter().zip(joint_weights).fold(Mat4::ZERO, |sum, (joint, w)| {
                        let bone = bones.get(usize::from(*joint)).copied().flatten();
                        sum + super::scene::joint_matrix(bone, skin) * *w
                    });
                    blend.transform_point3(p)
                }
                _ => p,
            };
            model += posed * weight;
        }
        Some(self.root.transform_point3(model))
    }
}

/// The barycentric weights of `p` in the triangle `a b c` (in the plane), if `p` is inside it or on its edge; `None`
/// outside it or for a triangle with no area in the plane (seen edge on).
fn barycentric(p: Vec2, a: Vec2, b: Vec2, c: Vec2) -> Option<Vec3> {
    // A point on a shared edge belongs to both triangles; the small tolerance keeps it from falling between them.
    const EDGE: f32 = -1e-5;
    let area = (b - a).perp_dot(c - a);
    if area.abs() < 1e-12 {
        return None;
    }
    let wb = (p - a).perp_dot(c - a) / area;
    let wc = (b - a).perp_dot(p - a) / area;
    let wa = 1.0 - wb - wc;
    (wa >= EDGE && wb >= EDGE && wc >= EDGE).then_some(Vec3::new(wa, wb, wc))
}
