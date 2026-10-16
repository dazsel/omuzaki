//! The frame loop's face on the mock character: unchanged layers are not repainted, variants switch only at the
//! crossings and re-upload only what changed, a palette applies without a new mesh, a failing painter keeps its old
//! pixels, the face path allocates nothing in steady state or across a switch, the swapping frame shows this frame's
//! pose, and replacing a rig gives back what its switch held.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (as in tests/frame.rs).
#![allow(
    unsafe_code,
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation
)]

// The shared double has helpers (the mock character builder) that this file does not need.
#[allow(dead_code)]
#[path = "frame/double.rs"]
mod double;

use std::alloc::{GlobalAlloc, Layout, System};
use std::any::Any;
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use omuzaki_anim_api::{Channel, HumanoidBone, IkGoals, Limb};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef, PartId, VariantId,
    VariantSet, resolve_variant,
};
use omuzaki_char_mock::{mock_character, mock_variants};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::rng::Rng;
use omuzaki_face::{FaceSystem, GpuRasterizer};
use omuzaki_face_api::{DrawList, FaceError, FaceLayerId, FaceLayerSpec, Palette, PaletteSlot};
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_render_api::{
    AlphaEncoding, Background, Camera, Capabilities, FrameReport, Image, Light, Material, MaterialId, MeshData, MeshId,
    MeshOptions, PaintTarget, Painter, PixelRect, Projection, Quality, RenderError, Renderer, ResourceRef,
    ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_stage::frame::face::{CharacterSource, FaceRig, InlineExecutor, Job, SETTLE_STEPS, SwitchExecutor};
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::{StageError, UploadOptions};

use double::Recorder;

/// Counts the allocations made on a thread that has switched counting on, so parallel tests don't see each other's.
struct Counting;

thread_local! {
    static ACTIVE: Cell<bool> = const { Cell::new(false) };
    static COUNT: Cell<u64> = const { Cell::new(0) };
}

fn note() {
    // `try_with`: the allocator also runs while a thread is being torn down.
    let _ = ACTIVE.try_with(|active| {
        if active.get() {
            let _ = COUNT.try_with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: every call forwards unchanged to the system allocator; counting touches only const-initialised cells.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note();
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note();
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// Allocations made by `f` on this thread.
fn allocations_in<R>(f: impl FnOnce() -> R) -> (R, u64) {
    COUNT.with(|count| count.set(0));
    ACTIVE.with(|active| active.set(true));
    let result = f();
    ACTIVE.with(|active| active.set(false));
    (result, COUNT.with(Cell::get))
}

/// Runs `f` with counting paused on this thread: the renderer's and the painter's own work is theirs, not the stage's.
fn uncounted<R>(f: impl FnOnce() -> R) -> R {
    let was = ACTIVE.with(Cell::get);
    ACTIVE.with(|active| active.set(false));
    let result = f();
    ACTIVE.with(|active| active.set(was));
    result
}

/// [`uncounted`] if `skip`: lets one test count the stage's allocations apart from the renderer's.
fn quiet<R>(skip: bool, f: impl FnOnce() -> R) -> R {
    if skip { uncounted(f) } else { f() }
}

/// The recording renderer with the counters this task's acceptance reads, and copies of what textures and materials
/// hold (the mock renderer can't be read back).
struct Spy {
    inner: Recorder,
    mesh_creates: u32,
    material_updates: u32,
    /// Every `paint_texture` call, by texture.
    paints: Vec<TextureId>,
    /// Each texture's bytes as the renderer holds them: written by a paint only when the painter succeeds, as the
    /// mock commits them.
    textures: HashMap<TextureId, Vec<u8>>,
    /// Each material as last created or updated.
    materials: HashMap<MaterialId, Material>,
    /// Every `update_vertices` call so far.
    update_calls: u32,
    /// The `update_vertices` call (1-based, counted from `update_calls`) that fails once, then the switch resets.
    fail_update_call: Option<u32>,
    /// Makes the next `release` call fail (the resource stays), then resets.
    fail_next_release: bool,
    /// Leaves the renderer's own allocations out of [`allocations_in`] (the painter's always are).
    quiet: bool,
}

impl Spy {
    fn new(draw: bool) -> Self {
        Self {
            inner: Recorder::new(draw),
            mesh_creates: 0,
            material_updates: 0,
            paints: Vec::with_capacity(1 << 12),
            textures: HashMap::new(),
            materials: HashMap::new(),
            update_calls: 0,
            fail_update_call: None,
            fail_next_release: false,
            quiet: false,
        }
    }
}

impl Renderer for Spy {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.mesh_creates += 1;
        quiet(self.quiet, || self.inner.create_mesh(mesh))
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.mesh_creates += 1;
        quiet(self.quiet, || self.inner.create_mesh_with(mesh, options))
    }
    fn update_vertices(&mut self, mesh: MeshId, p: &[Vec3], n: &[Vec3]) -> Result<(), RenderError> {
        self.update_calls += 1;
        if self.fail_update_call == Some(self.update_calls) {
            self.fail_update_call = None;
            return Err(RenderError::invalid("update_vertices", "the upload was told to fail once"));
        }
        quiet(self.quiet, || self.inner.update_vertices(mesh, p, n))
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        quiet(self.quiet, || {
            let id = self.inner.create_texture(desc, rgba)?;
            let bytes = (desc.size.x * desc.size.y * 4) as usize;
            self.textures.insert(id, rgba.map_or_else(|| vec![0; bytes], <[u8]>::to_vec));
            Ok(id)
        })
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        self.paints.push(texture);
        // The painter's own work is the painter's cost (the real one is pre-sized, the mock is not): not counted here.
        uncounted(|| {
            let mut copying = Copying { painter, rgba: self.textures.get(&texture).cloned().unwrap_or_default() };
            self.inner.paint_texture(texture, &mut copying)?;
            self.textures.insert(texture, copying.rgba);
            Ok(())
        })
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        quiet(self.quiet, || {
            let id = self.inner.create_material(material)?;
            self.materials.insert(id, material.clone());
            Ok(id)
        })
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.material_updates += 1;
        quiet(self.quiet, || {
            self.inner.update_material(id, material)?;
            self.materials.insert(id, material.clone());
            Ok(())
        })
    }
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        quiet(self.quiet, || self.inner.create_target(desc))
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        quiet(self.quiet, || self.inner.resize_target(target, size))
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        if std::mem::take(&mut self.fail_next_release) {
            return Err(RenderError::invalid("release", "the release was told to fail once"));
        }
        quiet(self.quiet, || self.inner.release(resource))
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        quiet(self.quiet, || self.inner.render(target, scene, quality))
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        quiet(self.quiet, || self.inner.read_target(target))
    }
}

/// Runs a painter through a target that keeps a copy of the texture's bytes with what the painter writes.
struct Copying<'a> {
    painter: &'a mut dyn Painter,
    rgba: Vec<u8>,
}

impl Painter for Copying<'_> {
    fn paint(&mut self, target: &mut dyn PaintTarget) -> Result<(), RenderError> {
        self.painter.paint(&mut CopyingTarget { inner: target, rgba: &mut self.rgba })
    }
}

struct CopyingTarget<'a> {
    inner: &'a mut dyn PaintTarget,
    rgba: &'a mut Vec<u8>,
}

impl PaintTarget for CopyingTarget<'_> {
    fn desc(&self) -> &TextureDesc {
        self.inner.desc()
    }
    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        self.inner.write_rgba(rect, rgba)?;
        let (width, row) = (self.inner.desc().size.x as usize, rect.width as usize * 4);
        for (y, src) in rgba.chunks_exact(row).enumerate() {
            let at = ((rect.y as usize + y) * width + rect.x as usize) * 4;
            self.rgba[at..at + row].copy_from_slice(src);
        }
        Ok(())
    }
    fn native(&mut self) -> &mut dyn Any {
        self.inner.native()
    }
}

const FPS: f32 = 60.0;
const FACE_PX_PER_M: f32 = 2000.0;
/// The head fills this much of the frame at the start of a stage.
const START_COVERAGE: f64 = 0.15;

fn id(s: &str) -> VariantId {
    VariantId::new(s).unwrap()
}

/// The mock character built the way a live app builds it: through the caching builder, in the `game` variant.
struct Fixture {
    source: CharacterSource,
    built: BuiltCharacter,
}

fn fixture() -> Fixture {
    fixture_from(mock_character().unwrap(), mock_variants().unwrap())
}

fn fixture_from(base: CharacterDef, variants: VariantSet) -> Fixture {
    let builder = Arc::new(CachingBuilder::new());
    let options = BuildOptions { variant: Some(id("game")), ..BuildOptions::default() };
    let def = resolve_variant(&base, &variants, &id("game")).unwrap();
    let services = BuildServices::default();
    let built = builder.build(&CharId::new("mock").unwrap(), &def, &options, &services).unwrap();
    Fixture { source: CharacterSource::new(builder, base, variants, options, services), built }
}

fn scene() -> Scene {
    let mut scene = Scene::new(Camera::default());
    scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 1.0 }];
    scene
}

/// A stage with a face rig on `renderer`. `with_source` lets it switch variants and apply palettes.
fn stage_on(renderer: &mut Spy, size: u32, with_source: bool) -> Stage {
    stage_with_rig(renderer, size, with_source, mock_rig())
}

/// The mock painter's rig: the real painter's sections are still stubs, so only the mock reacts to the bus. It draws at
/// a fixed density, so `resize_face` tests use [`FaceRig::standard`].
fn mock_rig() -> FaceRig {
    mock_rig_on(Box::new(InlineExecutor))
}

/// The mock rig with its variant builds run by `executor`; the default one would use a thread and make tests racy.
fn mock_rig_on(executor: Box<dyn SwitchExecutor>) -> FaceRig {
    FaceRig::new(Box::new(|projection, _| {
        FaceSystem::new(Box::new(MockPainter::new(*projection)), Box::new(MockRasterizer))
    }))
    .with_executor(executor)
}

/// An executor that parks its jobs until the test runs them: the build is "in flight" for as long as the test wants.
#[derive(Clone, Default)]
struct Deferred(Arc<Mutex<Vec<Job>>>);

impl Deferred {
    fn pending(&self) -> usize {
        self.0.lock().unwrap().len()
    }

    fn run_all(&self) {
        let jobs: Vec<Job> = self.0.lock().unwrap().drain(..).collect();
        for job in jobs {
            job();
        }
    }
}

impl SwitchExecutor for Deferred {
    fn run(&self, job: Job) {
        self.0.lock().unwrap().push(job);
    }
}

/// One frame the way a live app runs it: the variant service, then the tick.
fn run_frame(stage: &mut Stage, renderer: &mut Spy, input: &FrameInput) -> Result<(), StageError> {
    stage.service_variants(renderer, input)?;
    stage.tick(renderer, input).map(|_| ())
}

fn stage_with_rig(renderer: &mut Spy, size: u32, with_source: bool, rig: FaceRig) -> Stage {
    stage_from(renderer, size, with_source, rig, fixture())
}

fn stage_from(renderer: &mut Spy, size: u32, with_source: bool, rig: FaceRig, fixture: Fixture) -> Stage {
    let Fixture { source, built } = fixture;
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::splat(size), alpha: AlphaEncoding::Straight })
        .expect("a target");
    let mut stage = Stage::new(
        renderer,
        built,
        UploadOptions { face_px_per_m: FACE_PX_PER_M },
        scene(),
        target,
        Quality::offline(),
        StageClock::fixed(FPS),
    )
    .expect("the stage starts");
    stage.set_face(renderer, if with_source { rig.with_source(source) } else { rig }).expect("the face binds");
    stage.scene_mut().camera = camera_for(&stage, START_COVERAGE);
    stage
}

/// A camera looking at the head from the front that sees the head at `coverage` of the frame's height.
fn camera_for(stage: &Stage, coverage: f64) -> Camera {
    let built = stage.built();
    let face = built.face.as_ref().expect("the mock has a face");
    let head_height = f64::from(face.projection.rect.y1 - face.projection.rect.y0);
    let bone = built.rig.bone(&JointRef::HEAD).expect("a head bone").index();
    let bind = built.rig.inverse_bind()[bone].inverse().w_axis.truncate();
    let head = stage.skin().get(bone).map_or(bind, |skin| skin.transform_point3(bind));
    let fov: f64 = 30.0;
    let distance = head_height / (coverage * 2.0 * (fov.to_radians() * 0.5).tan());
    Camera::look_at(
        head + Vec3::new(0.0, 0.0, distance as f32),
        head,
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.01, far: 50.0 },
    )
}

fn blink(on: bool) -> FrameInput {
    let mut input = FrameInput::default();
    let value = if on { 1.0 } else { 0.0 };
    input.channels.set(Channel::EYE_BLINK_LEFT, value);
    input.channels.set(Channel::EYE_BLINK_RIGHT, value);
    input.channels.set(Channel::JAW_OPEN, value * 0.6);
    input.channels.set(Channel::BROW_DOWN_LEFT, value);
    input
}

#[test]
fn unchanged_layers_are_not_repainted_over_a_300_frame_run_with_a_10_frame_blink() {
    let mut renderer = Spy::new(true);
    let mut stage = stage_on(&mut renderer, 48, false);
    let layers = stage.character().face_layers().count();
    assert!(layers > 0, "the mock head has face layers");
    let mut per_frame = Vec::new();
    for n in 0..300 {
        let before = renderer.paints.len();
        stage.tick(&mut renderer, &blink((100..110).contains(&n))).expect("frame");
        per_frame.push(renderer.paints.len() - before);
    }
    println!(
        "face layers {layers}; paints: first frame {}, blink on {}, blink off {}, total {}",
        per_frame[0],
        per_frame[100],
        per_frame[110],
        renderer.paints.len()
    );
    assert_eq!(per_frame[0], layers, "the first frame paints every layer once");
    for (n, &paints) in per_frame.iter().enumerate() {
        if !matches!(n, 0 | 100 | 110) {
            assert_eq!(paints, 0, "frame {n}: nothing changed, so nothing is painted");
        }
    }
    assert!(per_frame[100] > 0 && per_frame[110] > 0, "the blink repaints the layers it touches");
    assert!(per_frame[100] <= layers && per_frame[110] <= layers);
    assert_eq!(per_frame[100], per_frame[110], "closing and opening touch the same layers");
    stage.release(&mut renderer).expect("released");
}

/// Coverage of frame `step` in a sweep from 0.10 up to 1.00 (and back), jittered by at most 0.01.
fn sweep(rng: &mut Rng, step: u32) -> f64 {
    0.10 + 0.0010 * f64::from(step) + f64::from(rng.range_f32(-0.01, 0.01))
}

#[test]
fn a_zoom_sweep_switches_variants_at_the_two_crossings_and_uploads_only_what_changed() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_on(&mut renderer, 32, true);
    let mut rng = Rng::new(571);
    let steps: Vec<u32> = (0..=900).chain((0..=900).rev()).collect();
    let mut switches = Vec::new();
    for (frame, step) in steps.into_iter().enumerate() {
        stage.scene_mut().camera = camera_for(&stage, sweep(&mut rng, step));
        let (variant, before) = (stage.built().variant.clone(), renderer.mesh_creates);
        let (old, revision) = (stage.built().meshes.clone(), stage.revision());
        run_frame(&mut stage, &mut renderer, &FrameInput::default()).expect("frame");
        if stage.built().variant == variant {
            assert_eq!(renderer.mesh_creates, before, "frame {frame}: no switch, no new mesh");
            assert_eq!(stage.revision(), revision, "frame {frame}: no switch, same revision");
            continue;
        }
        assert_eq!(stage.revision(), revision + 1, "frame {frame}: a switch replaces the character once");
        let new = &stage.built().meshes;
        let changed = new
            .iter()
            .filter(|m| !old.iter().any(|o| o.part == m.part && o.local == m.local && o.mesh == m.mesh))
            .count();
        let created = (renderer.mesh_creates - before) as usize;
        println!(
            "frame {frame}: {variant:?} -> {:?}: {created} of {} meshes re-uploaded",
            stage.built().variant,
            new.len()
        );
        assert_eq!(created, changed, "only the meshes whose geometry changed are created");
        assert!(created < new.len(), "the unchanged parts keep their upload");
        switches.push((frame, stage.built().variant.clone()));
    }
    let names: Vec<_> = switches.iter().map(|(_, v)| v.as_ref().map(VariantId::as_str)).collect();
    assert_eq!(names, vec![Some("close-up"), Some("game")], "one switch up the zoom, one back: {switches:?}");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_switched_character_matches_a_fresh_build_of_the_variant() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_on(&mut renderer, 32, true);
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    run_frame(&mut stage, &mut renderer, &FrameInput::default()).expect("frame");
    let fresh = {
        let (base, variants) = (mock_character().unwrap(), mock_variants().unwrap());
        let def = resolve_variant(&base, &variants, &id("close-up")).unwrap();
        let options = BuildOptions { variant: Some(id("close-up")), ..BuildOptions::default() };
        omuzaki_char_api::RegistryBuilder
            .build(&CharId::new("mock").unwrap(), &def, &options, &BuildServices::default())
            .unwrap()
    };
    assert_eq!(stage.built().variant, Some(id("close-up")));
    assert_eq!(stage.built().meshes, fresh.meshes, "the same meshes as a build from scratch");
    assert_eq!(stage.character().meshes().len(), fresh.meshes.len(), "and each has an upload");
    stage.release(&mut renderer).expect("released");
}

fn palette_with_outfit(stage: &Stage, colour: Colour) -> Palette {
    let mut palette = stage.built().palette.clone();
    palette.0.insert(PaletteSlot::new("outfit").expect("a slot name"), colour);
    palette
}

#[test]
fn apply_palette_changes_the_next_frame_without_creating_a_mesh() {
    let mut renderer = Spy::new(true);
    let mut stage = stage_on(&mut renderer, 96, true);
    stage.scene_mut().camera = Camera::look_at(
        Vec3::new(0.0, 1.0, 2.5),
        Vec3::new(0.0, 0.9, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    );
    let shot = |stage: &mut Stage, renderer: &mut Spy| {
        stage.tick(renderer, &FrameInput::default()).expect("frame");
        renderer.read_target(stage.target()).expect("pixels").rgba8
    };
    let before = shot(&mut stage, &mut renderer);
    let (creates, updates) = (renderer.mesh_creates, renderer.material_updates);
    let palette = palette_with_outfit(&stage, Colour::hex(0x00d0_2060));
    let report = stage.apply_palette(&mut renderer, &palette).expect("the palette applies");
    println!("apply_palette: {report:?}");
    assert_eq!(renderer.mesh_creates, creates, "no create_mesh call");
    assert_eq!(report.meshes_created, 0);
    assert!(renderer.material_updates > updates, "materials were updated in place");
    assert_ne!(shot(&mut stage, &mut renderer), before, "the next frame shows the new colours");
    assert_eq!(stage.built().palette, palette);
    stage.release(&mut renderer).expect("released");
}

/// A GPU rasteriser that fails while `fail` is set and otherwise declines (so the CPU raster is written): the failure
/// comes from inside the face layer painter, after the renderer has handed it the texture.
struct FailingGpu(Arc<AtomicBool>);

impl GpuRasterizer for FailingGpu {
    fn rasterize_native(&mut self, _: &FaceLayerSpec, _: &DrawList, _: &mut dyn Any) -> Result<bool, FaceError> {
        if self.0.load(Ordering::Relaxed) {
            Err(FaceError::Raster("the rasteriser was told to fail".into()))
        } else {
            Ok(false)
        }
    }
}

/// The mock rig whose layer painters fail while `fail` is set.
fn failing_rig(fail: &Arc<AtomicBool>) -> FaceRig {
    let fail = Arc::clone(fail);
    FaceRig::new(Box::new(move |projection, _| {
        let mut system = FaceSystem::new(Box::new(MockPainter::new(*projection)), Box::new(MockRasterizer));
        system.set_gpu_rasterizer(Some(Box::new(FailingGpu(Arc::clone(&fail)))));
        system
    }))
    .with_executor(Box::new(InlineExecutor))
}

/// The bytes of every face layer texture of `stage`, by layer.
fn layer_bytes(stage: &Stage, renderer: &Spy) -> Vec<(FaceLayerId, Vec<u8>)> {
    stage.character().face_layers().map(|l| (l.spec.id, renderer.textures[&l.texture].clone())).collect()
}

#[test]
fn a_failing_painter_keeps_the_old_pixels_and_a_retry_paints_the_new_ones() {
    let fail = Arc::new(AtomicBool::new(false));
    let mut renderer = Spy::new(false);
    let mut stage = stage_with_rig(&mut renderer, 48, false, failing_rig(&fail));
    stage.tick(&mut renderer, &blink(false)).expect("frame");
    let layers = stage.character().face_layers().count();
    let open = layer_bytes(&stage, &renderer);

    // What the blink looks like, from a stage whose painter never fails.
    let mut control_renderer = Spy::new(false);
    let mut control = stage_with_rig(&mut control_renderer, 48, false, failing_rig(&Arc::new(AtomicBool::new(false))));
    control.tick(&mut control_renderer, &blink(false)).expect("frame");
    assert_eq!(layer_bytes(&control, &control_renderer), open, "the two stages start from the same pixels");
    control.tick(&mut control_renderer, &blink(true)).expect("frame");
    let closed = layer_bytes(&control, &control_renderer);
    assert_ne!(closed, open, "the blink changes some layer's pixels, or this test aims at nothing");

    // The painter fails on every layer the blink changed: the textures keep the open eyes, byte for byte.
    fail.store(true, Ordering::Relaxed);
    for attempt in 0..2 {
        let before = renderer.paints.len();
        let error = stage.tick(&mut renderer, &blink(true)).expect_err("the painter fails");
        assert!(matches!(error, StageError::Render { .. }), "{error}");
        assert!(error.to_string().contains("face layer"), "the painter's own error comes back: {error}");
        let tried = renderer.paints.len() - before;
        assert!(tried > 0 && tried <= layers, "attempt {attempt}: the changed layers were tried ({tried})");
        assert_eq!(layer_bytes(&stage, &renderer), open, "attempt {attempt}: the old pixels are kept");
    }

    // The retry paints exactly the picture the blink calls for, and then nothing is owed.
    fail.store(false, Ordering::Relaxed);
    stage.tick(&mut renderer, &blink(true)).expect("the retry paints");
    assert_eq!(layer_bytes(&stage, &renderer), closed, "the retried layers hold the new pixels");
    let before = renderer.paints.len();
    stage.tick(&mut renderer, &blink(true)).expect("frame");
    assert_eq!(renderer.paints.len(), before, "once painted, nothing is owed");
    stage.release(&mut renderer).expect("released");
    control.release(&mut control_renderer).expect("released");
}

#[test]
fn settle_moves_the_simulations_and_creates_nothing() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_on(&mut renderer, 32, false);
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    let hair = |stage: &Stage| -> Vec<Vec3> {
        stage
            .built()
            .runtimes
            .iter()
            .filter_map(|slot| slot.runtime.dynamic_mesh(0).map(|view| view.positions.to_vec()))
            .flatten()
            .collect()
    };
    let (before, creates) = (hair(&stage), renderer.mesh_creates);
    assert!(!before.is_empty(), "the mock character has simulated hair");
    stage.settle(SETTLE_STEPS);
    assert_ne!(hair(&stage), before, "{SETTLE_STEPS} steps at 1/90 moved the hair");
    let out = stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    assert!(out.uploads > 0, "the next frame uploads the settled hair");
    assert_eq!(renderer.mesh_creates, creates);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn resizing_the_face_repaints_every_layer_at_the_new_density() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_with_rig(&mut renderer, 32, false, FaceRig::standard());
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    let layers = stage.character().face_layers().count();
    stage.resize_face(&mut renderer, FACE_PX_PER_M / 2.0).expect("resized");
    let before = renderer.paints.len();
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    assert_eq!(renderer.paints.len() - before, layers, "every layer, once, on the new textures");
    let before = renderer.paints.len();
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    assert_eq!(renderer.paints.len(), before);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn tick_allocates_nothing_after_a_face_resize() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_with_rig(&mut renderer, 32, false, FaceRig::standard());
    let input = blink(false);
    for _ in 0..5 {
        stage.tick(&mut renderer, &input).expect("warm-up frame");
    }
    let layers = stage.character().face_layers().count();
    // The frame-budget scheduler's knob, turned between frames: the new face system is made and warmed here.
    let started = Instant::now();
    stage.resize_face(&mut renderer, FACE_PX_PER_M / 2.0).expect("resized");
    let resize = started.elapsed();

    // The first tick paints every layer on the new textures; it and the two after it must not allocate.
    renderer.quiet = true;
    let mut per_tick = Vec::new();
    let before = renderer.paints.len();
    for _ in 0..3 {
        let started = Instant::now();
        let (result, allocations) = allocations_in(|| stage.tick(&mut renderer, &input));
        result.expect("frame");
        per_tick.push((allocations, started.elapsed()));
    }
    println!("resize_face {resize:?}, then per tick (allocations, time): {per_tick:?}");
    let per_tick: Vec<u64> = per_tick.into_iter().map(|(allocations, _)| allocations).collect();
    assert_eq!(renderer.paints.len() - before, layers, "every layer, once, on the new textures");
    let allocations: u64 = per_tick.iter().sum();
    assert_eq!(allocations, 0, "the three ticks after a resize allocated {allocations} times: {per_tick:?}");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn the_face_path_allocates_nothing_in_steady_state() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_on(&mut renderer, 32, true);
    let input = blink(false);
    for _ in 0..20 {
        stage.tick(&mut renderer, &input).expect("warm-up frame");
    }
    // The variant check runs every frame here (a source is attached), as does the face mapping and fingerprinting.
    let (result, allocations) = allocations_in(|| {
        for _ in 0..100 {
            stage.tick(&mut renderer, &input)?;
        }
        Ok::<(), StageError>(())
    });
    result.expect("frames");
    assert_eq!(allocations, 0, "100 steady-state frames with a face allocated {allocations} times");
    stage.release(&mut renderer).expect("released");
}

/// Ticks once at `coverage` and returns the result.
fn tick_at(stage: &mut Stage, renderer: &mut Spy, coverage: f64) -> Result<(), StageError> {
    stage.scene_mut().camera = camera_for(stage, coverage);
    run_frame(stage, renderer, &FrameInput::default())
}

#[test]
fn a_failed_dynamic_upload_during_a_variant_switch_leaves_the_stage_sound_and_a_retry_succeeds() {
    let mut renderer = Spy::new(true);
    let mut stage = stage_on(&mut renderer, 32, true);
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    // How many `update_vertices` calls an ordinary frame makes, so the failure can be aimed at the install's refresh.
    let before = renderer.update_calls;
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    let per_frame = renderer.update_calls - before;
    assert!(per_frame > 0, "the mock hair moves every frame, or this test aims at nothing");

    let (stats, variant, meshes) = (renderer.stats(), stage.built().variant.clone(), stage.built().meshes.clone());
    // The swap's refresh is the tick's first vertex update: it runs before the frame's own upload.
    renderer.fail_update_call = Some(renderer.update_calls + 1);
    let error = tick_at(&mut stage, &mut renderer, 0.8).expect_err("the refresh after the rebuild is refused");
    assert!(matches!(error, StageError::Render { .. }), "{error}");
    assert_eq!(stage.built().variant, variant, "the previous character is still the stage's");
    assert_eq!(stage.built().meshes, meshes);
    assert_eq!(renderer.stats(), stats, "what the abandoned install created was given back");

    // The old character is coherent: a frame at the old zoom draws (a dead handle would be a render error) ...
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("the old character still runs");
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("and again");
    // ... and the retry at the new zoom switches.
    tick_at(&mut stage, &mut renderer, 0.8).expect("the retry succeeds");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    tick_at(&mut stage, &mut renderer, 0.8).expect("the new character runs");
    stage.release(&mut renderer).expect("released");
}

/// A stage with a source and `rig`, warmed up at the start zoom.
fn stage_about_to_zoom(renderer: &mut Spy, rig: FaceRig) -> Stage {
    let mut stage = stage_with_rig(renderer, 32, true, rig);
    for _ in 0..5 {
        tick_at(&mut stage, renderer, START_COVERAGE).expect("warm-up frame");
    }
    stage
}

/// Two variants where the close-up lengthens the torso (a new rig) and adds a coat (more instances and joint entries).
fn growing_variants() -> VariantSet {
    serde_json::from_str(
        r#"{
            "game": { "shot": { "min": 0.0, "max": 0.35 } },
            "close-up": { "extends": "game", "shot": { "min": 0.35, "max": 5.0 },
                          "overrides": {
                              "parts/body/params/proportions/torso_length": 1.25,
                              "parts/coat": { "kind": "mock-garment", "layer": 40, "params": { "ease": 0.01 } }
                          } }
        }"#,
    )
    .expect("variants")
}

/// [`stage_about_to_zoom`] on the mock with [`growing_variants`].
fn growing_stage_about_to_zoom(renderer: &mut Spy, rig: FaceRig) -> Stage {
    let fixture = fixture_from(mock_character().unwrap(), growing_variants());
    let mut stage = stage_from(renderer, 32, true, rig, fixture);
    for _ in 0..5 {
        tick_at(&mut stage, renderer, START_COVERAGE).expect("warm-up frame");
    }
    stage
}

#[test]
fn tick_allocates_nothing_across_a_variant_switch_that_outgrows_the_scene_buffers() {
    let mut renderer = Spy::new(false);
    let mut stage = growing_stage_about_to_zoom(&mut renderer, mock_rig());
    let input = FrameInput::default();
    // The scene's buffers hold exactly the wide variant's frame, so the close-up's bigger one cannot fit them.
    stage.scene_mut().instances.shrink_to_fit();
    stage.scene_mut().joints.shrink_to_fit();
    let capacity = (stage.scene().instances.capacity(), stage.scene().joints.capacity());
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    // Everything that builds and allocates happens here, off the tick.
    stage.service_variants(&mut renderer, &input).expect("the switch is built and staged");
    assert_eq!(stage.built().variant, Some(id("game")), "staged, not yet swapped");

    // The swapping tick, then two more: the new character's first frames must not allocate either.
    renderer.quiet = true;
    let mut per_tick = Vec::new();
    for _ in 0..3 {
        let (result, allocations) = allocations_in(|| stage.tick(&mut renderer, &input));
        result.expect("frame");
        per_tick.push(allocations);
    }
    let allocations: u64 = per_tick.iter().sum();
    assert_eq!(stage.built().variant, Some(id("close-up")), "the first of these ticks swapped the variant in");
    let size = (stage.scene().instances.len(), stage.scene().joints.len());
    println!("scene buffers: capacity {capacity:?} before the switch, the close-up fills {size:?}");
    assert!(size.0 > capacity.0 && size.1 > capacity.1, "the switch outgrows both buffers: {size:?} vs {capacity:?}");
    assert_eq!(allocations, 0, "the swapping tick and two after it allocated {allocations} times: {per_tick:?}");
    stage.service_variants(&mut renderer, &input).expect("the retired character is released");
    stage.release(&mut renderer).expect("released");
}

/// The model-space position of `bone` in `stage`'s last solve.
fn bone_position(stage: &Stage, bone: HumanoidBone) -> Vec3 {
    let rig = &stage.built().rig;
    let index = rig.bone(&JointRef::Humanoid(bone)).expect("a humanoid bone").index();
    let bind = rig.inverse_bind()[index].inverse().w_axis.truncate();
    stage.skin()[index].transform_point3(bind)
}

/// A frame input that moves the hips by `hips` and reaches the left hand for `hand`.
fn reaching(hips: Vec3, hand: Vec3) -> FrameInput {
    let mut input = FrameInput::default();
    input.pose.hips_offset = hips;
    input.goals = IkGoals {
        left_hand: Some(Limb { target: hand, pole: hand + Vec3::new(0.0, 0.0, -0.5), weight: 1.0 }),
        ..IkGoals::NONE
    };
    input
}

#[test]
fn the_swapping_frame_shows_this_frames_pose_on_a_new_rig() {
    let mut renderer = Spy::new(false);
    let mut stage = growing_stage_about_to_zoom(&mut renderer, mock_rig());
    let old_rig = stage.built().rig.clone();
    let shoulder = bone_position(&stage, HumanoidBone::LeftUpperArm);
    let (before, now) = (
        reaching(Vec3::ZERO, shoulder + Vec3::new(0.12, -0.1, 0.1)),
        reaching(Vec3::new(0.0, -0.05, 0.08), shoulder + Vec3::new(0.1, 0.08, 0.12)),
    );
    stage.tick(&mut renderer, &before).expect("frame");

    // The switch is staged with the earlier pose and goals, then the swapping frame brings new ones.
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    stage.service_variants(&mut renderer, &before).expect("staged");
    let staged_at = (bone_position(&stage, HumanoidBone::Head), bone_position(&stage, HumanoidBone::LeftHand));
    stage.tick(&mut renderer, &now).expect("the swapping frame");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    assert_ne!(stage.built().rig, old_rig, "the close-up has its own rig, or this test aims at nothing");
    let swapped = (bone_position(&stage, HumanoidBone::Head), bone_position(&stage, HumanoidBone::LeftHand));
    let joints = stage.scene().joints.clone();

    // The next frame with the same input is the reference: the swapping frame must already look like it.
    stage.tick(&mut renderer, &now).expect("frame");
    let settled = (bone_position(&stage, HumanoidBone::Head), bone_position(&stage, HumanoidBone::LeftHand));
    println!(
        "head {:?} -> {:?} (next {:?}); hand {:?} -> {:?} (next {:?})",
        staged_at.0, swapped.0, settled.0, staged_at.1, swapped.1, settled.1
    );
    assert!(staged_at.0.distance(settled.0) > 0.05, "the pose moved the head, or this test aims at nothing");
    assert!(staged_at.1.distance(settled.1) > 0.05, "the goals moved the hand, or this test aims at nothing");
    assert!(swapped.0.distance(settled.0) < 1e-4, "the swapping frame's head follows this frame's pose");
    assert!(swapped.1.distance(settled.1) < 1e-4, "the swapping frame's hand follows this frame's goal");
    let drift =
        joints.iter().zip(&stage.scene().joints).map(|(a, b)| (a.w_axis - b.w_axis).length()).fold(0.0, f32::max);
    assert_eq!(joints.len(), stage.scene().joints.len());
    // Spring bones (hair) still swing between the two frames; the skinning joints of the body don't.
    assert!(drift < 0.05, "the swapping frame's joint palette is this frame's: drift {drift}");
    stage.release(&mut renderer).expect("released");
}

/// The meshes, textures and materials a renderer holds: a released stage leaves none (only its target).
fn held(stats: ResourceStats) -> (u32, u32, u32) {
    (stats.meshes, stats.textures, stats.materials)
}

/// Live materials as `renderer` last saw them, for those of `before`'s ids.
fn materials_of(renderer: &Spy, before: &HashMap<MaterialId, Material>) -> HashMap<MaterialId, Material> {
    before.keys().filter_map(|id| renderer.materials.get(id).map(|m| (*id, m.clone()))).collect()
}

#[test]
fn replacing_a_rig_with_a_staged_switch_gives_back_what_it_staged() {
    let mut renderer = Spy::new(false);
    let fixture = fixture_from(mock_character().unwrap(), brows_variants());
    let mut stage = stage_from(&mut renderer, 32, true, mock_rig(), fixture);
    for _ in 0..3 {
        tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    }
    let (stats, materials) = (renderer.stats(), renderer.materials.clone());
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("staged");
    assert_ne!(renderer.stats(), stats, "staging the brows layer made resources, or this test aims at nothing");
    assert_ne!(materials_of(&renderer, &materials), materials, "and repointed a live material");

    stage.set_face(&mut renderer, mock_rig()).expect("the new rig binds");
    assert_eq!(renderer.stats(), stats, "what the old rig staged was given back");
    assert_eq!(materials_of(&renderer, &materials), materials, "and the live materials are as they were");
    tick_at(&mut stage, &mut renderer, 0.8).expect("the stage runs on its old variant");
    assert_eq!(stage.built().variant, Some(id("game")), "the new rig has no source, so nothing switches");
    stage.release(&mut renderer).expect("released");
    assert_eq!(held(renderer.stats()), (0, 0, 0), "nothing leaked");
}

#[test]
fn replacing_a_rig_after_a_swap_releases_what_the_swap_retired() {
    let run = |replace: bool| {
        let mut renderer = Spy::new(false);
        let mut stage = stage_about_to_zoom(&mut renderer, mock_rig());
        tick_at(&mut stage, &mut renderer, 0.8).expect("the switching frame");
        assert_eq!(stage.built().variant, Some(id("close-up")));
        let before = renderer.stats();
        if replace {
            stage.set_face(&mut renderer, mock_rig()).expect("the new rig binds");
        } else {
            stage.service_variants(&mut renderer, &FrameInput::default()).expect("released");
        }
        let after = renderer.stats();
        stage.tick(&mut renderer, &FrameInput::default()).expect("the stage runs");
        stage.release(&mut renderer).expect("released");
        let left = renderer.stats();
        (before, after, left)
    };
    let (before, replaced, left) = run(true);
    let (_, serviced, _) = run(false);
    assert_ne!(before, serviced, "the swap retired something, or this test aims at nothing");
    assert_eq!(replaced, serviced, "replacing the rig released what the swap retired, as a service would");
    assert_eq!(held(left), (0, 0, 0), "nothing leaked");
}

#[test]
fn the_old_variant_keeps_drawing_until_the_new_one_is_ready() {
    let (mut renderer, deferred) = (Spy::new(true), Deferred::default());
    let mut stage = stage_about_to_zoom(&mut renderer, mock_rig_on(Box::new(deferred.clone())));
    let (variant, meshes, creates) =
        (stage.built().variant.clone(), stage.built().meshes.clone(), renderer.mesh_creates);

    // The head now fills the frame: the build is requested, and parked.
    for n in 0..10 {
        tick_at(&mut stage, &mut renderer, 0.8).expect("a frame while the build is in flight");
        assert_eq!(deferred.pending(), 1, "frame {n}: one build requested, not one per frame");
        assert_eq!(stage.built().variant, variant, "frame {n}: still the old variant");
    }
    assert_eq!(stage.built().meshes, meshes);
    assert_eq!(renderer.mesh_creates, creates, "nothing is created before the build is done");

    // The build finishes: the next service stages it, and only the tick after that swaps it in.
    deferred.run_all();
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("staged");
    assert_eq!(stage.built().variant, variant, "staged, but the old variant is still the live one");
    stage.tick(&mut renderer, &FrameInput::default()).expect("the swapping frame");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    assert_eq!(deferred.pending(), 0, "no second build: the head is where the new variant wants it");
    tick_at(&mut stage, &mut renderer, 0.8).expect("and the new variant draws");
    assert_eq!(deferred.pending(), 0);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_lost_build_is_reported_and_asked_for_again() {
    let (mut renderer, deferred) = (Spy::new(false), Deferred::default());
    let mut stage = stage_about_to_zoom(&mut renderer, mock_rig_on(Box::new(deferred.clone())));
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("requested");
    // The executor drops the job without running it.
    drop(deferred.0.lock().unwrap().drain(..).collect::<Vec<_>>());
    let error = stage.service_variants(&mut renderer, &FrameInput::default()).expect_err("the build was lost");
    assert!(matches!(error, StageError::Rebuild { .. }), "{error}");
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("asks again");
    assert_eq!(deferred.pending(), 1);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn the_swap_replaces_the_character_in_one_tick_and_the_next_service_releases_the_old_one() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_about_to_zoom(&mut renderer, mock_rig());
    let input = FrameInput::default();
    stage.scene_mut().camera = camera_for(&stage, 0.8);

    let started = Instant::now();
    stage.service_variants(&mut renderer, &input).expect("staged");
    let staged_in = started.elapsed();
    let before_swap = renderer.stats();
    stage.tick(&mut renderer, &input).expect("swap");
    let latency = started.elapsed();
    println!(
        "variant switch, inline executor ({SETTLE_STEPS} warm-up steps): staged after {staged_in:?}, swapped after {latency:?} \
         (1 service call + 1 tick)"
    );
    // A bound, not a benchmark: the switch is one service call and one tick, and finishes in seconds on any CI machine.
    assert!(latency.as_secs() < 30, "the switch took {latency:?}");

    assert_eq!(stage.built().variant, Some(id("close-up")));
    assert_eq!(renderer.stats(), before_swap, "the swap itself creates and releases nothing on the renderer");
    // The old resources go back with the next service, not in the swap.
    stage.service_variants(&mut renderer, &input).expect("released");
    assert!(renderer.stats().meshes <= before_swap.meshes, "the old meshes are gone: {:?}", renderer.stats());
    stage.release(&mut renderer).expect("released");
}

/// Two variants that differ only in whether the hair's brows show through, so no projection or profile changes.
fn brows_variants() -> VariantSet {
    serde_json::from_str(
        r#"{
            "game": { "shot": { "min": 0.0, "max": 0.35 },
                      "overrides": { "parts/hair/params/shading/brows_through": false } },
            "close-up": { "extends": "game", "shot": { "min": 0.35, "max": 5.0 },
                          "overrides": { "parts/hair/params/shading/brows_through": true } }
        }"#,
    )
    .expect("variants")
}

#[test]
fn a_variant_that_turns_brows_through_on_paints_the_new_brows_layer_on_the_same_tick() {
    let mut renderer = Spy::new(true);
    let fixture = fixture_from(mock_character().unwrap(), brows_variants());
    let mut stage = stage_from(&mut renderer, 32, true, mock_rig(), fixture);
    for _ in 0..3 {
        tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    }
    assert!(stage.character().face_layer(FaceLayerId::Brows).is_none(), "the wide variant has no brows layer");

    let paints = renderer.paints.len();
    tick_at(&mut stage, &mut renderer, 0.8).expect("the switching frame");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    let brows = stage.character().face_layer(FaceLayerId::Brows).expect("the close-up has a brows layer").texture;
    assert!(renderer.paints[paints..].contains(&brows), "the new layer texture got its paint on the very next tick");
    stage.release(&mut renderer).expect("released");
}

/// The mock character with streaked hair, so a palette slot is baked into hair vertex colours.
fn streaked_base() -> CharacterDef {
    let mut base = mock_character().unwrap();
    base.palette.0.insert(PaletteSlot::new("streak").unwrap(), Colour::rgb(200, 30, 30));
    base.parts.get_mut(&PartId::new("hair").unwrap()).unwrap().params = serde_json::json!({
        "groups": {
            "a-cap": { "generator": { "type": "cap" } },
            "b-bangs": { "generator": { "type": "bangs" } },
            "c-curtain": { "generator": { "type": "curtain" } },
        },
        "shading": {
            "streaks": [{ "fraction": 0.5, "colour": "@streak", "strength": 0.8, "seed": 3, "blend": "replace" }],
        },
    });
    base
}

#[test]
fn a_palette_change_creates_exactly_the_meshes_whose_baked_tint_moved() {
    let mut renderer = Spy::new(false);
    let fixture = fixture_from(streaked_base(), mock_variants().unwrap());
    let mut stage = stage_from(&mut renderer, 32, true, mock_rig(), fixture);

    // A slot no vertex colour reads: materials only, no mesh.
    let palette = palette_with_outfit(&stage, Colour::hex(0x0020_a060));
    let creates = renderer.mesh_creates;
    let report = stage.apply_palette(&mut renderer, &palette).expect("applies");
    assert_eq!(renderer.mesh_creates, creates, "a palette change that touches no baked tint creates no mesh");
    assert_eq!(report.meshes_created, 0);

    // The streak slot is baked into the hair: those meshes are recreated (will update their vertices instead).
    let old = stage.built().meshes.clone();
    let mut palette = stage.built().palette.clone();
    palette.0.insert(PaletteSlot::new("streak").unwrap(), Colour::rgb(20, 220, 90));
    let creates = renderer.mesh_creates;
    let report = stage.apply_palette(&mut renderer, &palette).expect("applies");
    let affected = stage
        .built()
        .meshes
        .iter()
        .filter(|m| !old.iter().any(|o| o.part == m.part && o.local == m.local && o.mesh == m.mesh))
        .count();
    assert!(affected > 0, "the streaks reach some hair mesh");
    assert!(affected < stage.built().meshes.len(), "and not every mesh");
    assert_eq!((renderer.mesh_creates - creates) as usize, affected, "exactly the affected meshes are created");
    assert_eq!(report.meshes_created as usize, affected);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_rejected_palette_does_not_come_back_with_the_next_variant_switch() {
    let mut renderer = Spy::new(true);
    let mut stage = stage_on(&mut renderer, 32, true);
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    let before = renderer.update_calls;
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    let per_frame = renderer.update_calls - before;

    let original = stage.built().palette.clone();
    let rejected = palette_with_outfit(&stage, Colour::hex(0x0020_a060));
    assert_ne!(rejected, original);
    // The install's dynamic refresh is refused, so the palette change is abandoned.
    renderer.fail_update_call = Some(renderer.update_calls + 1);
    stage.apply_palette(&mut renderer, &rejected).expect_err("the install fails");
    assert_eq!(stage.built().palette, original, "the old palette stays in force");

    // Zooming in switches variants, which rebuilds from the source: it must carry the palette in force, not the rejected one.
    tick_at(&mut stage, &mut renderer, 0.8).expect("the switch");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    assert_eq!(stage.built().palette, original, "the rejected palette did not leak into the rebuild");
    assert!(per_frame > 0);
    stage.release(&mut renderer).expect("released");
}

#[test]
fn an_aborted_install_makes_the_kept_character_resend_every_dynamic_mesh() {
    let mut renderer = Spy::new(true);
    let mut stage = stage_on(&mut renderer, 32, true);
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    let before = renderer.update_calls;
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    let per_frame = renderer.update_calls - before;
    assert!(per_frame > 0, "the mock hair moves every frame");

    // Fail the swap's refresh (it runs before the frame's own upload): the switch is abandoned, and the meshes it shares
    // with the kept character may already show the abandoned simulation.
    renderer.fail_update_call = Some(renderer.update_calls + 1);
    tick_at(&mut stage, &mut renderer, 0.8).expect_err("the refresh is refused");

    // The kept character's next frame resends all of its meshes, so none keeps showing the abandoned shape. A revision
    // that merely differs from the last seen one is not enough: the steps of that frame move it back to equality.
    let before = renderer.update_calls;
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("the kept character runs");
    assert_eq!(renderer.update_calls - before, per_frame, "every dynamic mesh was uploaded again");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_failed_release_after_a_resize_still_rebinds_the_face_system() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_with_rig(&mut renderer, 32, false, FaceRig::standard());
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    let layers = stage.character().face_layers().count();

    renderer.fail_next_release = true;
    let error = stage.resize_face(&mut renderer, FACE_PX_PER_M / 2.0).expect_err("the old textures stay");
    assert!(matches!(error, StageError::Render { .. }), "{error}");
    assert_eq!(
        stage.character().face_px_per_m().to_bits(),
        (FACE_PX_PER_M / 2.0).to_bits(),
        "the new density is in force"
    );

    let before = renderer.paints.len();
    stage.tick(&mut renderer, &FrameInput::default()).expect("painting at the new density validates");
    assert_eq!(renderer.paints.len() - before, layers, "every layer repainted on the new textures");
    // The refused release left the old textures behind; that is the renderer's to clean up, not this test's.
    stage.release(&mut renderer).expect("released");
}

// ---- #1485: replacing a rig mid-switch, and a painter failing during one ----

#[test]
fn replacing_a_rig_while_its_build_is_in_flight_never_swaps_in_the_old_rigs_variant() {
    let (mut renderer, deferred) = (Spy::new(false), Deferred::default());
    let mut stage = stage_about_to_zoom(&mut renderer, mock_rig_on(Box::new(deferred.clone())));
    let stats = renderer.stats();
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("the build is requested");
    assert_eq!(deferred.pending(), 1, "a build is in flight, or this test aims at nothing");

    // The new rig has no source: nothing it does may come from the old rig's build.
    stage.set_face(&mut renderer, mock_rig()).expect("the new rig binds");
    deferred.run_all();
    for _ in 0..3 {
        tick_at(&mut stage, &mut renderer, 0.8).expect("the stage runs");
    }
    assert_eq!(stage.built().variant, Some(id("game")), "the old rig's finished build was never staged or swapped in");
    assert_eq!(renderer.stats(), stats, "and nothing it built reached the renderer");
    stage.release(&mut renderer).expect("released");
    assert_eq!(held(renderer.stats()), (0, 0, 0), "nothing leaked");
}

/// The mock painter, failing while `fail` is set.
struct FailingPainter {
    inner: MockPainter,
    fail: Arc<AtomicBool>,
}

impl omuzaki_face_api::FacePainter for FailingPainter {
    fn layers(&self) -> &[FaceLayerSpec] {
        omuzaki_face_api::FacePainter::layers(&self.inner)
    }

    fn paint(
        &mut self,
        input: &omuzaki_face_api::FaceInput<'_>,
        out: &mut omuzaki_face_api::FaceDrawing,
    ) -> Result<(), FaceError> {
        if self.fail.load(Ordering::Relaxed) {
            return Err(FaceError::Invalid("the painter was told to fail".into()));
        }
        omuzaki_face_api::FacePainter::paint(&mut self.inner, input, out)
    }
}

#[test]
fn a_painter_failing_while_a_switch_is_staged_is_reported_and_the_current_variant_keeps_drawing() {
    let fail = Arc::new(AtomicBool::new(false));
    let painter_fail = Arc::clone(&fail);
    let rig = FaceRig::new(Box::new(move |projection, _| {
        let painter = FailingPainter { inner: MockPainter::new(*projection), fail: Arc::clone(&painter_fail) };
        FaceSystem::new(Box::new(painter), Box::new(MockRasterizer))
    }))
    .with_executor(Box::new(InlineExecutor));
    let mut renderer = Spy::new(false);
    let mut stage = stage_about_to_zoom(&mut renderer, rig);
    let stats = renderer.stats();

    // The close-up has its own face window, so staging it warms a new face system, whose painter fails.
    stage.scene_mut().camera = camera_for(&stage, 0.8);
    fail.store(true, Ordering::Relaxed);
    let error = stage.service_variants(&mut renderer, &FrameInput::default()).expect_err("the painter fails");
    assert!(matches!(error, StageError::Face { .. }), "the painter's failure is reported as the face's: {error}");
    assert!(error.to_string().contains("told to fail"), "{error}");
    assert_eq!(renderer.stats(), stats, "nothing was left staged on the renderer");

    // The current variant keeps drawing; once the painter recovers, the switch goes through.
    fail.store(false, Ordering::Relaxed);
    stage.tick(&mut renderer, &FrameInput::default()).expect("the current variant draws");
    assert_eq!(stage.built().variant, Some(id("game")));
    tick_at(&mut stage, &mut renderer, 0.8).expect("the switch is staged again");
    tick_at(&mut stage, &mut renderer, 0.8).expect("and swapped in");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    stage.release(&mut renderer).expect("released");
    assert_eq!(held(renderer.stats()), (0, 0, 0), "nothing leaked");
}

#[test]
fn the_face_expressions_are_shared_and_a_variant_swap_replaces_them_with_a_pointer_store() {
    let mut renderer = Spy::new(false);
    let mut stage = stage_about_to_zoom(&mut renderer, mock_rig());
    let profile_expressions = |stage: &Stage| stage.built().face.as_ref().expect("a face").profile.expressions.clone();
    let before = Arc::clone(stage.face_expressions().expect("the mock character has a face"));
    assert_eq!(*before, profile_expressions(&stage), "the table is the face profile's");
    let (held, allocations) = allocations_in(|| stage.face_expressions().map(Arc::clone));
    assert_eq!(allocations, 0, "holding the table is a pointer copy: no clone, no allocation");
    drop(held);

    tick_at(&mut stage, &mut renderer, 0.8).expect("the switching frame");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    let after = stage.face_expressions().expect("the close-up has a face");
    assert!(!Arc::ptr_eq(&before, after), "the swap stored the table the build job made for the new variant");
    assert_eq!(**after, profile_expressions(&stage), "and it is the new character's");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_live_stage_installs_the_expressions_of_the_character_a_variant_swap_brings() {
    use omuzaki_stage::perform::host::PerformHost as _;
    use omuzaki_stage::perform::input::SourceSlot;
    use omuzaki_stage::perform::live::LiveStage;

    let mut renderer = Spy::new(false);
    let stage = stage_on(&mut renderer, 32, true);
    let performer = {
        let parts = omuzaki_stage::perform::PerformerParts {
            source: Box::new(omuzaki_stage::perform::input::NoSource),
            retarget: Box::new(omuzaki_anim::retarget::ArkitRetarget::new(omuzaki_anim_api::DeviceProfile::Webcam)),
            life: Box::new(omuzaki_anim::life::Life::new(omuzaki_anim_api::LifeConfig::new(7))),
            animator: Box::new(omuzaki_anim_mock::animator::MockAnimator),
        };
        omuzaki_stage::perform::Performer::new(
            parts,
            omuzaki_anim_api::ChannelBuf::NEUTRAL,
            omuzaki_anim_api::HumanoidPose::REST,
        )
    };
    let mut live = LiveStage::new(
        performer,
        stage,
        Arc::new(SourceSlot::new()),
        Quality::live(),
        omuzaki_render_api::budget::LIVE_GPU,
    )
    .expect("the loop starts");
    let wanted = live.stage().built().face.as_ref().expect("the mock has a face").profile.expressions.clone();
    assert_eq!(live.handle().expressions(), wanted, "installed at the start");
    // Another character's set is installed by the app: the swap must put the face's own back.
    live.handle()
        .set_expressions(omuzaki_face_api::ExpressionSet::starter(), omuzaki_anim_api::ChannelBuf::NEUTRAL)
        .unwrap();
    assert_ne!(live.handle().expressions(), wanted);
    let mut rng = Rng::new(571);
    let revision = live.stage().revision();
    let mut swapped_at = None;
    for (frame, step) in (0..=900).enumerate() {
        live.stage_mut().scene_mut().camera = camera_for(live.stage(), sweep(&mut rng, step));
        // A frame per performer step: `now` is the middle of step `frame`.
        let now = omuzaki_core::time::Seconds((frame as f64 + 0.5) / f64::from(omuzaki_stage::perform::STEP_HZ));
        live.frame(&mut renderer, now).expect("frame");
        if live.stage().revision() != revision {
            assert_eq!(
                live.handle().expressions(),
                omuzaki_face_api::ExpressionSet::starter(),
                "frame() installs nothing"
            );
            assert!(live.expressions_stale());
            swapped_at = Some(frame);
            // The loop's cold step, which it runs after every frame, installs the face's expressions again.
            assert!(live.service(&mut renderer).expect("the service step runs"), "the expressions were installed");
            assert!(!live.expressions_stale());
            break;
        }
        live.service(&mut renderer).expect("the service step runs");
    }
    assert!(swapped_at.is_some(), "the zoom sweep switched variants");
    assert_eq!(live.handle().expressions(), wanted, "the swap installed the character's expressions again");
    live.into_stage().release(&mut renderer).expect("released");
}

// ---- the rig's GPU rasteriser goes from each face system to the next ----

/// A GPU rasteriser that counts the paints it is offered and declines them (the mock renderer's targets are no wgpu
/// tuple), so the CPU raster draws: a test sees whether the rig still hands paints to it.
struct ProbeGpu(Arc<AtomicUsize>);

impl GpuRasterizer for ProbeGpu {
    fn rasterize_native(&mut self, _: &FaceLayerSpec, _: &DrawList, _: &mut dyn Any) -> Result<bool, FaceError> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(false)
    }
}

/// The mock rig with a [`ProbeGpu`] counting into `offered`.
fn probed_rig(offered: &Arc<AtomicUsize>) -> FaceRig {
    mock_rig().with_gpu_rasterizer(Box::new(ProbeGpu(Arc::clone(offered))))
}

/// Paints offered to the GPU rasteriser by a frame whose face changes (a blink), then by one that changes it back.
fn offered_by_a_blink(stage: &mut Stage, renderer: &mut Spy, offered: &AtomicUsize) -> usize {
    let before = offered.load(Ordering::Relaxed);
    stage.tick(renderer, &blink(true)).expect("frame");
    stage.tick(renderer, &blink(false)).expect("frame");
    offered.load(Ordering::Relaxed) - before
}

#[test]
fn the_gpu_rasteriser_follows_the_face_into_a_new_variants_face_system() {
    let offered = Arc::new(AtomicUsize::new(0));
    let mut renderer = Spy::new(false);
    let mut stage = stage_about_to_zoom(&mut renderer, probed_rig(&offered));
    assert!(offered_by_a_blink(&mut stage, &mut renderer, &offered) > 0, "the first face system has the rasteriser");
    // The close-up has its own face window, so it gets a face system of its own.
    tick_at(&mut stage, &mut renderer, 0.8).expect("the switching frame");
    assert_eq!(stage.built().variant, Some(id("close-up")));
    stage.service_variants(&mut renderer, &FrameInput::default()).expect("the old face system is released");
    assert!(offered_by_a_blink(&mut stage, &mut renderer, &offered) > 0, "the new face system got the rasteriser");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn the_gpu_rasteriser_stays_with_a_face_system_a_variant_keeps() {
    let offered = Arc::new(AtomicUsize::new(0));
    let mut renderer = Spy::new(false);
    let fixture = fixture_from(mock_character().unwrap(), brows_variants());
    let mut stage = stage_from(&mut renderer, 32, true, probed_rig(&offered), fixture);
    tick_at(&mut stage, &mut renderer, START_COVERAGE).expect("frame");
    tick_at(&mut stage, &mut renderer, 0.8).expect("the switching frame");
    assert_eq!(stage.built().variant, Some(id("close-up")), "the brows variant swapped in, keeping the face system");
    assert!(offered_by_a_blink(&mut stage, &mut renderer, &offered) > 0, "the kept face system still has it");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn the_gpu_rasteriser_follows_the_face_into_the_face_system_a_resize_makes() {
    let offered = Arc::new(AtomicUsize::new(0));
    let mut renderer = Spy::new(false);
    // The real painter, whose layers follow the density (the mock painter's are fixed).
    let rig = FaceRig::standard().with_gpu_rasterizer(Box::new(ProbeGpu(Arc::clone(&offered))));
    let mut stage = stage_with_rig(&mut renderer, 32, false, rig);
    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    stage.resize_face(&mut renderer, FACE_PX_PER_M / 2.0).expect("resized");
    let before = offered.load(Ordering::Relaxed);
    stage.tick(&mut renderer, &FrameInput::default()).expect("the frame after the resize repaints every layer");
    assert!(offered.load(Ordering::Relaxed) > before, "the new face system was offered its repaint");
    assert!(offered_by_a_blink(&mut stage, &mut renderer, &offered) > 0, "and keeps the rasteriser");
    stage.release(&mut renderer).expect("released");
}
