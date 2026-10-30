//! The Render area's exists-when test (CLAUDE.md, spine map §5): the renderer draws the kit character and an imported
//! VRM (bring your own model) in one scene, with toon and `MToon` shading, outlines, GPU morphs and expressions, a light
//! rig and framing; the stage runs the perform loop at a fixed 60 Hz from tracking frames to posed, rendered frames;
//! and the frame is there as a GPU texture for the viewport and the outputs.
//!
//! The project is made the way a person or an agent makes it: `character.create` for the kit's default character, a
//! VRM file of the kit's mock character dropped in as the second (no third-party model), `stage.create_scene` and
//! `stage.place_character`, and staged whole by [`SceneStage`], as the studio's live view stages it.
//!
//! The CPU side runs on the render mock in every test run: 0 allocations a frame in steady state, and the stage's
//! time per frame. With `--features wgpu` the GPU side runs on the software adapter (lavapipe): the pictures change as
//! the channels, the light rig and the camera change, and the target's native texture is the frame's size. With
//! `OMUZAKI_EVIDENCE_DIR` set the pictures are written there. GPU timings on a real GPU are the owner's hand check.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, reason = "frame counts and pixel counts, far below f32's range")]

#[path = "avatar/kit.rs"]
mod kit;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::time::{Duration, Instant};

use omuzaki_anim_api::Channel;
use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildServices, Quality as BuildQuality};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, find};
use omuzaki_render_api::{
    AlphaEncoding, Capabilities, EnvironmentDesc, EnvironmentId, FrameReport, Image, Material, MaterialId, MeshData,
    MeshId, MeshOptions, PaintTarget, Painter, PixelRect, Quality, RenderError, Renderer, ResourceRef, ResourceStats,
    Scene, TargetDesc, TargetId, TextureDesc, TextureId,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::body::BodyServices;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::scene::stage::{Bodies, SceneStage, SceneStageOptions};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::{MemberId, SceneId};
use omuzaki_store_api::Store;
use omuzaki_store_mock::MemStore;
use serde_json::{Value, json};

// ------------------------------------------------------------------------------------------------ counting

/// Counts the allocations made on a thread that has switched counting on.
struct Counting;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn note() {
    let _ = WATCHING.try_with(|w| {
        if w.get() {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
    });
}

// SAFETY: forwards to the system allocator; the counter is thread-local.
#[allow(unsafe_code)]
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

fn counted<T>(f: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    let result = f();
    WATCHING.with(|w| w.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

/// The mock renderer, which once `report` holds a report answers `render` with it without drawing, doesn't copy vertex
/// updates and runs painters into a target that keeps nothing: a frame then measures the stage's own work (as the frame
/// loop's `Recorder` does), the face painter's included.
struct Canned {
    inner: MockRenderer,
    report: Option<FrameReport>,
    /// Every texture made, for the painters' targets.
    textures: Vec<(TextureId, TextureDesc)>,
}

/// A paint target that checks what is written and keeps none of it.
struct Sink<'a>(&'a TextureDesc, ());

impl PaintTarget for Sink<'_> {
    fn desc(&self) -> &TextureDesc {
        self.0
    }
    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        if rgba.len() == rect.byte_len() { Ok(()) } else { Err(RenderError::invalid("rgba", "the wrong length")) }
    }
    fn native(&mut self) -> &mut dyn std::any::Any {
        &mut self.1
    }
}

impl Renderer for Canned {
    fn capabilities(&self) -> &Capabilities {
        self.inner.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.inner.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        self.inner.create_mesh(mesh)
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        self.inner.create_mesh_with(mesh, options)
    }
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        if self.report.is_some() { Ok(()) } else { self.inner.update_vertices(mesh, positions, normals) }
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        let id = self.inner.create_texture(desc, rgba)?;
        self.textures.push((id, *desc));
        Ok(id)
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn Painter) -> Result<(), RenderError> {
        match self.textures.iter().find(|(id, _)| *id == texture) {
            Some((_, desc)) if self.report.is_some() => painter.paint(&mut Sink(desc, ())),
            _ => self.inner.paint_texture(texture, painter),
        }
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        self.inner.create_material(material)
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        self.inner.update_material(id, material)
    }
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        self.inner.create_target(desc)
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        self.inner.resize_target(target, size)
    }
    fn create_environment(&mut self, desc: &EnvironmentDesc) -> Result<EnvironmentId, RenderError> {
        self.inner.create_environment(desc)
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        self.inner.release(resource)
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        match self.report {
            Some(report) => Ok(report),
            None => self.inner.render(target, scene, quality),
        }
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        self.inner.read_target(target)
    }
}

// ------------------------------------------------------------------------------------------------ the project

/// Runs op `name` with `args` on `doc` as the executor would, and applies its plan.
fn step(doc: &mut MemDoc, name: &str, args: Value) {
    omuzaki_stage::link();
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let op = (find(name).unwrap_or_else(|| panic!("{name} is linked")).decode)(args).unwrap();
    let plan = op.plan(&OpCtx::new(doc, &actor, &clock)).unwrap_or_else(|e| panic!("{name}: {e:?}"));
    doc.apply(&plan.patch).expect("applies");
}

/// The kit's default character `kit` and an imported VRM `guest`, cast together in scene `main`: the guest stands
/// beside the subject.
fn project(store: &MemStore) -> MemDoc {
    let mut doc = MemDoc::new();
    step(&mut doc, "character.create", json!({ "id": "kit", "name": "Kit", "template": "standard", "seed": 7 }));
    let hash = store.put(&kit::vrm_bytes(&kit::kit()), kit::provenance()).expect("stored");
    let def = AvatarDef::new(AvatarSource::new(hash, SourceFormat::Vrm1, "guest.vrm"));
    let node = json!({ "name": "Guest", "avatar": serde_json::to_value(&def).unwrap() });
    let path = omuzaki_doc_api::DocPath::parse("characters/guest").unwrap();
    doc.apply(&omuzaki_doc_api::Patch::new().with(path, None, Some(node))).expect("the import's node");
    step(&mut doc, "stage.create_scene", json!({ "id": "main", "cast": ["kit"] }));
    step(&mut doc, "stage.place_character", json!({ "character": "guest", "id": "guest", "position": [0.55, 0.0] }));
    doc
}

fn main_scene() -> SceneId {
    SceneId::new("main").unwrap()
}

/// The scene staged on `renderer` into a new target of `size`, as the live view stages it.
fn open(renderer: &mut dyn Renderer, doc: &MemDoc, store: &MemStore, size: UVec2, quality: Quality) -> SceneStage {
    let mtoon = renderer.capabilities().mtoon;
    let (builder, build, clock) = (CachingBuilder::new(), BuildServices::default(), FakeClock::default());
    let mut bodies = Bodies::new(BodyServices::new(&builder, &build, store, mtoon, &clock), BuildQuality::Live);
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Premultiplied }).unwrap();
    let aspect = size.x as f32 / size.y as f32;
    let options = SceneStageOptions::new(quality, StageClock::fixed(60.0));
    SceneStage::open(doc, Some(&main_scene()), &mut bodies, renderer, target, aspect, options)
        .expect("the scene stages both characters")
}

/// A tracking frame `i` steps in: the head turning and nodding, the jaw opening and a smile, both eyes blinking.
fn tracked(i: usize) -> FrameInput {
    let t = i as f32 / 60.0;
    let mut input = FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() };
    let wave = |hz: f32| 0.5 + 0.5 * (t * hz * std::f32::consts::TAU).sin();
    input.channels.set(Channel::HEAD_YAW, 0.4 * (wave(0.3) - 0.5));
    input.channels.set(Channel::HEAD_PITCH, 0.2 * (wave(0.5) - 0.5));
    input.channels.set(Channel::JAW_OPEN, wave(2.0));
    input.channels.set(Channel::MOUTH_SMILE_LEFT, wave(0.7));
    input.channels.set(Channel::MOUTH_SMILE_RIGHT, wave(0.7));
    input.channels.set(Channel::EYE_BLINK_LEFT, if i % 90 < 6 { 1.0 } else { 0.0 });
    input.channels.set(Channel::EYE_BLINK_RIGHT, if i % 90 < 6 { 1.0 } else { 0.0 });
    input
}

/// [`tracked`] with the face held at frame 0's expression: the head and the bodies still move, and the face is not
/// repainted. On the live path a hardware GPU paints the face (the stage's `VelloRasterizer`, Face's 0.5 ms budget);
/// here the mock has no GPU, so the face falls back to the CPU rasteriser, which is timed separately.
fn held_face(i: usize) -> FrameInput {
    let mut input = tracked(i);
    let rest = tracked(0);
    for channel in [
        Channel::JAW_OPEN,
        Channel::MOUTH_SMILE_LEFT,
        Channel::MOUTH_SMILE_RIGHT,
        Channel::EYE_BLINK_LEFT,
        Channel::EYE_BLINK_RIGHT,
    ] {
        input.channels.set(channel, rest.channels[channel]);
    }
    input
}

/// Gives `input` to both characters (the lead's comes with the frame) and runs one frame.
fn frame(stage: &mut SceneStage, renderer: &mut dyn Renderer, guest: &MemberId, input: &FrameInput) {
    stage.cast_mut().set_member_input(guest, input);
    stage.tick(renderer, input).expect("a frame");
}

fn guest() -> MemberId {
    MemberId::new("guest").unwrap()
}

fn release(stage: SceneStage, renderer: &mut dyn Renderer) {
    let target = stage.target();
    stage.release(renderer).unwrap();
    renderer.release(target.into()).unwrap();
}

// ------------------------------------------------------------------------------------------------ the CPU side

/// The frames counted: ten seconds of [`tracked`].
const FRAMES: usize = 600;

#[test]
fn both_characters_perform_at_60_hz_from_tracking_frames_and_a_frame_allocates_nothing() {
    let store = MemStore::new();
    let doc = project(&store);
    let mut renderer = Canned { inner: MockRenderer::new(), report: None, textures: Vec::new() };
    let mut stage = open(&mut renderer, &doc, &store, UVec2::new(640, 360), Quality::live());
    let guest = guest();
    assert_eq!(stage.members().map(MemberId::as_str).collect::<Vec<_>>(), ["guest", "kit"]);
    assert!(stage.skipped().is_empty(), "both load: {:?}", stage.skipped());

    // Warm up on the same 600 frames the count runs: the springs settle, and the face painter's scratch rows grow to the
    // widest shape the trace makes (they grow to a new extreme, never per frame). Then the draw is answered with a real
    // report, so what is counted is the stage's own work.
    for i in 0..FRAMES {
        frame(&mut stage, &mut renderer, &guest, &tracked(i));
    }
    renderer.report =
        Some(renderer.inner.render(stage.target(), stage.scene(), &Quality::live()).expect("a mock frame"));
    let mut tracking = Vec::with_capacity(FRAMES);
    let ((), allocations) = counted(|| {
        for i in 0..FRAMES {
            let input = tracked(i);
            let start = Instant::now();
            frame(&mut stage, &mut renderer, &guest, &input);
            tracking.push(start.elapsed());
        }
    });
    assert_eq!(allocations, 0, "600 frames of two performing characters allocate nothing");
    let mut held = Vec::with_capacity(FRAMES);
    for i in 0..FRAMES {
        let input = held_face(i);
        let start = Instant::now();
        frame(&mut stage, &mut renderer, &guest, &input);
        held.push(start.elapsed());
    }
    let [tracking, held] = [tracking, held].map(|mut times| {
        times.sort_unstable();
        [50, 95, 100].map(|percent| times[(times.len() - 1) * percent / 100])
    });
    eprintln!(
        "stage CPU per frame, kit + imported VRM, this build and machine: p50 {:.2} ms, p95 {:.2} ms, max {:.2} ms; \
         with the face painted on the CPU (the fallback without a hardware GPU) p50 {:.2} ms, p95 {:.2} ms",
        ms(held[0]),
        ms(held[1]),
        ms(held[2]),
        ms(tracking[0]),
        ms(tracking[1]),
    );
    // A ceiling, not the budget, and only for an optimised build: the workspace's own crates build unoptimised in the
    // dev profile, where two characters read several times slower than they run.
    if !cfg!(debug_assertions) {
        assert!(held[0] < Duration::from_micros(16_667), "the median frame is {:.2} ms", ms(held[0]));
    }
    release(stage, &mut renderer);
}

fn ms(d: Duration) -> f64 {
    d.as_secs_f64() * 1e3
}

// ------------------------------------------------------------------------------------------------ the GPU side

#[cfg(feature = "wgpu")]
mod gpu {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, NativeTarget, WgpuRenderer};
    use omuzaki_render_api::MtoonSupport;

    use super::*;

    /// How many pixels differ between two pictures by more than a few levels.
    fn changed(a: &Image, b: &Image) -> usize {
        let (left, _) = a.rgba8.as_chunks::<4>();
        let (right, _) = b.rgba8.as_chunks::<4>();
        left.iter().zip(right).filter(|(x, y)| x.iter().zip(*y).take(3).any(|(p, q)| p.abs_diff(*q) > 8)).count()
    }

    #[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
    fn keep(name: &str, image: &Image) {
        let Some(dir) = std::env::var_os("OMUZAKI_EVIDENCE_DIR") else { return };
        let png = omuzaki_stage::ops::render::still::encode_png(image).unwrap();
        std::fs::write(std::path::Path::new(&dir).join(format!("{name}.png")), png).unwrap();
    }

    /// Runs `frames` frames from the tracking frame `at` and reads the picture.
    fn shot(stage: &mut SceneStage, renderer: &mut WgpuRenderer, at: &FrameInput, frames: usize) -> Image {
        let guest = guest();
        for _ in 0..frames {
            frame(stage, renderer, &guest, at);
        }
        renderer.read_target(stage.target()).expect("the picture reads back")
    }

    /// Edits the document with op `name` and has the stage follow it.
    fn edit(
        stage: &mut SceneStage,
        renderer: &mut WgpuRenderer,
        doc: &mut MemDoc,
        store: &MemStore,
        name: &str,
        args: Value,
    ) {
        step(doc, name, args);
        let mtoon = renderer.capabilities().mtoon;
        let (builder, build, clock) = (CachingBuilder::new(), BuildServices::default(), FakeClock::default());
        let mut bodies = Bodies::new(BodyServices::new(&builder, &build, store, mtoon, &clock), BuildQuality::Live);
        stage.sync(doc, &mut bodies, renderer).expect("the stage follows the edit");
    }

    #[test]
    fn on_the_gpu_both_characters_are_drawn_and_move_with_the_channels_the_lights_and_the_camera() {
        let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
            .expect("a software adapter (install mesa-vulkan-drivers)");
        assert_eq!(renderer.capabilities().mtoon, MtoonSupport::ALL, "the renderer draws every MToon group");
        let store = MemStore::new();
        let mut doc = project(&store);
        let size = UVec2::new(480, 270);
        let mut stage = open(&mut renderer, &doc, &store, size, Quality::offline());

        // The scene's full shot, at rest and then with the face moving: GPU morphs on the VRM, the painted face on
        // the kit.
        edit(&mut stage, &mut renderer, &mut doc, &store, "stage.set_active_camera", json!({ "camera": "full" }));
        let rest = FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() };
        let full = shot(&mut stage, &mut renderer, &rest, 30);
        keep("exists-1-full", &full);
        let instances = stage.scene().instances.len();
        assert!(instances > 4, "both bodies, their parts and the ground are drawn: {instances} instances");
        edit(&mut stage, &mut renderer, &mut doc, &store, "stage.set_active_camera", json!({ "camera": "bust" }));
        let bust = shot(&mut stage, &mut renderer, &rest, 30);
        keep("exists-2-bust", &bust);
        let mut smile = rest.clone();
        for (channel, value) in [
            (Channel::JAW_OPEN, 0.8),
            (Channel::MOUTH_SMILE_LEFT, 1.0),
            (Channel::MOUTH_SMILE_RIGHT, 1.0),
            (Channel::EYE_BLINK_LEFT, 1.0),
            (Channel::EYE_BLINK_RIGHT, 1.0),
        ] {
            smile.channels.set(channel, value);
        }
        let expression = shot(&mut stage, &mut renderer, &smile, 30);
        keep("exists-3-expression", &expression);
        // Another light rig, through its op: the scene's lights change on the next frame.
        edit(&mut stage, &mut renderer, &mut doc, &store, "stage.set_light_rig", json!({ "rig": "viewer-moonlit" }));
        let moonlit = shot(&mut stage, &mut renderer, &smile, 2);
        keep("exists-4-moonlit", &moonlit);
        edit(&mut stage, &mut renderer, &mut doc, &store, "stage.set_active_camera", json!({ "camera": "face" }));
        let face = shot(&mut stage, &mut renderer, &smile, 2);
        keep("exists-5-face", &face);

        let pixels = (size.x * size.y) as usize;
        for (what, a, b) in [
            ("the bust framing", &full, &bust),
            ("the expression", &bust, &expression),
            ("the light rig", &expression, &moonlit),
            ("the face framing", &moonlit, &face),
        ] {
            let moved = changed(a, b);
            eprintln!("{what}: {moved} of {pixels} pixels changed");
            assert!(moved > pixels / 200, "{what} changed only {moved} pixels");
        }

        // The frame as a GPU texture: the device, the queue and the output texture, at the frame's size.
        let native = renderer.target_native(stage.target()).expect("a live target").expect("a native target");
        let (_, _, texture) = native.downcast_ref::<NativeTarget>().expect("the wgpu renderer's native target");
        assert_eq!((texture.width(), texture.height()), (size.x, size.y));
        release(stage, &mut renderer);
    }
}
