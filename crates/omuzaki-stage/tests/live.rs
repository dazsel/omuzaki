//! The kit character performing live (#2072): a scripted camera, through `LiveInput`, the performer and the stage, to
//! the scene the renderer draws. The mock character with its face, the real retarget, life layer and animator, the
//! render mock. Every run is in simulation time, so the same trace gives the same frames.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "frame/double.rs"]
mod double;
#[path = "live/support.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim::BusAnimatorParams;
use omuzaki_anim_api::{Channel, DeviceProfile, HeadPose, HumanoidBone, HumanoidPose, TrackFrame};
use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, RegistryBuilder};
use omuzaki_char_api::{BuiltCharacter, JointRef};
use omuzaki_core::glam::EulerRot;
use omuzaki_core::glam::{Mat4, Quat, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face::FaceSystem;
use omuzaki_face_api::ExpressionSet;
use omuzaki_face_mock::{MockPainter, MockRasterizer};
use omuzaki_ops_api::Mode;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Capabilities, FrameReport, Image, Material, MaterialId, MeshData, MeshId, MeshOptions,
    PaintTarget, Painter as RenderPainter, Pass, Projection, Quality, RenderError, Renderer, ResourceRef,
    ResourceStats, Scene, TargetDesc, TargetId, TextureDesc, TextureId, budget,
};
use omuzaki_render_api::{Colour, Light};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{InputError, InputHost, LiveInput, NoPrompt, PreviewState, SourceKind, SourceSlot};
use omuzaki_stage::perform::live::{LiveFrame, LiveStage};
use omuzaki_stage::perform::triggers::TriggerBinding;
use omuzaki_stage::perform::{STEP_HZ, Tracking};
use omuzaki_stage::scene::resolve::{CheckedScene, Figure, ResolvedScene};
use omuzaki_stage::{StageError, UploadOptions};
use omuzaki_stage_api::{
    Anchor, CameraDef, CameraId as SceneCameraId, CastMember, Framing, LightDef, LightId, LightShape, MemberId,
    SceneDef, SceneSettings,
};
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::TrackerConfig;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

use double::{Recorder, built};
use support::{CameraLog, Cameras, face_for, mid_step, performer};

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

/// Runs `f` without counting its allocations: the renderer's are the renderer's cost, not the loop's.
fn uncounted<R>(f: impl FnOnce() -> R) -> R {
    let was = ACTIVE.with(Cell::get);
    ACTIVE.with(|active| active.set(false));
    let result = f();
    ACTIVE.with(|active| active.set(was));
    result
}

/// The recording renderer with its own allocations left out of [`allocations_in`]: the mock copies every painted
/// texture, which a GPU renderer does not.
struct Quiet(Recorder);

impl Renderer for Quiet {
    fn capabilities(&self) -> &Capabilities {
        self.0.capabilities()
    }
    fn stats(&self) -> ResourceStats {
        self.0.stats()
    }
    fn create_mesh(&mut self, mesh: &MeshData) -> Result<MeshId, RenderError> {
        uncounted(|| self.0.create_mesh(mesh))
    }
    fn create_mesh_with(&mut self, mesh: &MeshData, options: &MeshOptions) -> Result<MeshId, RenderError> {
        uncounted(|| self.0.create_mesh_with(mesh, options))
    }
    fn update_vertices(&mut self, mesh: MeshId, positions: &[Vec3], normals: &[Vec3]) -> Result<(), RenderError> {
        uncounted(|| self.0.update_vertices(mesh, positions, normals))
    }
    fn create_texture(&mut self, desc: &TextureDesc, rgba: Option<&[u8]>) -> Result<TextureId, RenderError> {
        uncounted(|| self.0.create_texture(desc, rgba))
    }
    fn paint_texture(&mut self, texture: TextureId, painter: &mut dyn RenderPainter) -> Result<(), RenderError> {
        // The face painter runs inside the call and is the loop's, so it is counted; the mock's texture copy is not.
        let outer = ACTIVE.with(Cell::get);
        let mut counted = |target: &mut dyn PaintTarget| {
            ACTIVE.with(|active| active.set(outer));
            let result = painter.paint(target);
            ACTIVE.with(|active| active.set(false));
            result
        };
        uncounted(|| self.0.paint_texture(texture, &mut counted))
    }
    fn create_material(&mut self, material: &Material) -> Result<MaterialId, RenderError> {
        uncounted(|| self.0.create_material(material))
    }
    fn update_material(&mut self, id: MaterialId, material: &Material) -> Result<(), RenderError> {
        uncounted(|| self.0.update_material(id, material))
    }
    fn create_target(&mut self, desc: &TargetDesc) -> Result<TargetId, RenderError> {
        uncounted(|| self.0.create_target(desc))
    }
    fn resize_target(&mut self, target: TargetId, size: UVec2) -> Result<(), RenderError> {
        uncounted(|| self.0.resize_target(target, size))
    }
    fn release(&mut self, resource: ResourceRef) -> Result<(), RenderError> {
        uncounted(|| self.0.release(resource))
    }
    fn render(&mut self, target: TargetId, scene: &Scene, quality: &Quality) -> Result<FrameReport, RenderError> {
        uncounted(|| self.0.render(target, scene, quality))
    }
    fn read_target(&mut self, target: TargetId) -> Result<Image, RenderError> {
        uncounted(|| self.0.read_target(target))
    }
}

/// A camera that turns the head through `yaw(t)` radians, 30 frames a second for `seconds`.
fn turning(seconds: f32, yaw: impl Fn(f32) -> f32) -> Vec<(Duration, TrackFrame)> {
    (0..(seconds * 30.0) as u64)
        .map(|i| {
            let t = i as f32 / 30.0;
            let mut frame = TrackFrame::empty(i, Seconds(f64::from(t)), DeviceProfile::Webcam);
            frame.head = Some(HeadPose { yaw: yaw(t), pitch: 0.0, roll: 0.0, position: None });
            (Duration::from_nanos((f64::from(t) * 1e9) as u64), frame)
        })
        .collect()
}

/// A sweep of ±0.4 rad with a 4 s period.
fn sweep(t: f32) -> f32 {
    0.4 * (t * std::f32::consts::TAU / 4.0).sin()
}

fn scene() -> Scene {
    Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ))
}

/// Which face painter the stage runs.
#[derive(Copy, Clone, PartialEq, Eq)]
enum Painter {
    /// The face mock's painter and rasteriser: the face path runs and reacts to the bus at no painting cost, so a
    /// measurement is the stage's and the performer's.
    Mock,
    /// The real painter on the CPU rasteriser.
    Standard,
    /// No face rig: what the stage and the performer cost on their own.
    None,
}

/// The live world: the loop, the input the person drives, and the camera log. Dropping it releases the stage.
struct Live<R: Renderer> {
    renderer: R,
    inner: Option<LiveStage>,
    input: LiveInput,
    log: CameraLog,
    /// The step the next frame runs.
    step: u64,
}

impl<R: Renderer> Live<R> {
    /// The mock character with its face on `renderer`, performing from `cameras` (none bound yet).
    fn new(renderer: R, cameras: Vec<(&str, Vec<(Duration, TrackFrame)>)>, quality: Quality, painter: Painter) -> Self {
        Self::sized(renderer, cameras, quality, painter, 64)
    }

    /// [`new`](Self::new) with a `size` px square target.
    fn sized(
        renderer: R,
        cameras: Vec<(&str, Vec<(Duration, TrackFrame)>)>,
        quality: Quality,
        painter: Painter,
        size: u32,
    ) -> Self {
        Self::build(renderer, cameras, quality, quality, painter, size)
    }

    /// A loop whose scheduler asks for `quality` over a stage made with `stage_quality`.
    fn build(
        mut renderer: R,
        cameras: Vec<(&str, Vec<(Duration, TrackFrame)>)>,
        quality: Quality,
        stage_quality: Quality,
        painter: Painter,
        size: u32,
    ) -> Self {
        let mut character: BuiltCharacter = built();
        if let Some(face) = character.face.as_mut() {
            face.profile.expressions = ExpressionSet::starter();
        }
        let mut base = HumanoidPose::REST;
        seed_base_pose(&character.rig, &mut base);
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::splat(size), alpha: AlphaEncoding::Straight }).unwrap();
        // The mock painter draws at this density; the real one at any.
        let options = UploadOptions { face_px_per_m: 2000.0 };
        let mut stage =
            Stage::new(&mut renderer, character, options, scene(), target, stage_quality, StageClock::live()).unwrap();
        let rig = match painter {
            Painter::Mock => Some(FaceRig::new(Box::new(|projection, _| {
                FaceSystem::new(Box::new(MockPainter::new(*projection)), Box::new(MockRasterizer))
            }))),
            Painter::Standard => Some(FaceRig::standard()),
            Painter::None => None,
        };
        if let Some(rig) = rig {
            stage.set_face(&mut renderer, rig.with_executor(Box::new(InlineExecutor))).unwrap();
        }
        let sources = Arc::new(SourceSlot::new());
        let live = LiveStage::new(performer(&base), stage, Arc::clone(&sources), quality, budget::LIVE_GPU).unwrap();
        let perform: Arc<dyn PerformHost> = Arc::new(live.handle().clone());
        let (tracker, log) = Cameras::new(cameras);
        let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), sources, perform);
        Self { renderer, inner: Some(live), input, log, step: 0 }
    }

    fn live(&self) -> &LiveStage {
        self.inner.as_ref().expect("the loop runs until the drop")
    }

    /// One display frame at the next step's middle, as the loop returns it.
    fn try_frame(&mut self) -> Result<Option<LiveFrame>, StageError> {
        let now = mid_step(self.step);
        self.step += 1;
        let live = self.inner.as_mut().expect("the loop runs until the drop");
        live.frame(&mut self.renderer, now)
    }

    /// What the person does in the Live workspace: pick the webcam, bind `camera`, start the preview.
    fn go_live(&self, camera: &str) {
        self.input.select(SourceKind::Webcam, Mode::Apply).unwrap();
        self.input.bind_camera(Some(CameraId(camera.to_owned())), Mode::Apply).unwrap();
        self.input.preview(true, Mode::Apply).unwrap();
    }

    /// One display frame in the middle of the next step.
    fn frame(&mut self) -> LiveFrame {
        self.try_frame().unwrap().expect("one step is due per frame")
    }

    fn frames(&mut self, n: u64) {
        for _ in 0..n {
            self.frame();
        }
    }

    fn bus(&self, channel: Channel) -> f32 {
        self.live().input().channels[channel]
    }
}

impl<R: Renderer> Drop for Live<R> {
    fn drop(&mut self) {
        if let Some(live) = self.inner.take() {
            live.into_stage().release(&mut self.renderer).expect("the stage releases");
        }
    }
}

/// The joint palette entry of `bone` in the submitted scene: its skinning matrix, as the renderer reads it.
fn palette(stage: &Stage, bone: &JointRef) -> Mat4 {
    let index = stage.built().rig.bone(bone).expect("the bone exists").index();
    stage.skin()[index]
}

/// The rotation `bone` has turned through since its bind, in world space.
fn turned(stage: &Stage, bone: &JointRef) -> Quat {
    palette(stage, bone).to_scale_rotation_translation().1
}

/// The head's turn relative to the bone the neck hangs from: what the neck and head add on top of the body.
fn head_turn(stage: &Stage) -> Quat {
    let rig = &stage.built().rig;
    let neck = rig.bone(&JointRef::Humanoid(HumanoidBone::Neck)).expect("a neck");
    let parent = rig.skeleton.bones()[neck.index()].parent.expect("the neck has a parent");
    let parent_turn = stage.skin()[parent.index()].to_scale_rotation_translation().1;
    parent_turn.inverse() * turned(stage, &JointRef::HEAD)
}

/// The angle between two rotations, precise for small angles (`Quat::angle_between` goes through `acos`, which loses
/// everything under about 3e-4 rad in f32).
fn angle(a: Quat, b: Quat) -> f32 {
    let d = a.inverse() * b;
    2.0 * d.xyz().length().min(1.0).asin()
}

/// What the animator turns the neck and head through for the bus's head angles: each its share, neck first.
fn bus_turn(yaw: f32, pitch: f32, roll: f32) -> Quat {
    let neck = BusAnimatorParams::default().neck_share;
    let euler = |share: f32| Quat::from_euler(EulerRot::YXZ, yaw * share, pitch * share, roll * share);
    euler(neck) * euler(1.0 - neck)
}

/// The world position of a point 10 cm in front of the head's bind position, as the scene's palette poses it.
fn nose(stage: &Stage) -> Vec3 {
    let rig = &stage.built().rig;
    let head = rig.bone(&JointRef::HEAD).unwrap().index();
    let bind = rig.inverse_bind()[head].inverse();
    let front = bind.transform_point3(Vec3::new(0.0, 0.0, 0.1));
    palette(stage, &JointRef::HEAD).transform_point3(front)
}

/// The scene's instances and joints as bytes, for comparing runs.
fn digest(scene: &Scene) -> [u8; 32] {
    let mut hash = Sha256::new();
    for matrix in &scene.joints {
        for value in matrix.to_cols_array() {
            hash.update(value.to_le_bytes());
        }
    }
    for instance in &scene.instances {
        for value in instance.transform.to_cols_array() {
            hash.update(value.to_le_bytes());
        }
    }
    hash.update(scene.frame.time.0.to_le_bytes());
    hash.finalize().into()
}

#[test]
fn a_fake_trace_reaches_the_rendered_scene_and_the_head_follows_the_bus_yaw() {
    let mut live = Live::new(MockRenderer::new(), vec![("cam", turning(12.0, sweep))], Quality::live(), Painter::Mock);
    live.go_live("cam");
    live.frames(60);
    assert_eq!(live.input.state().tracking, Tracking::Tracking);
    let mut worst = 0.0_f32;
    for _ in 0..(4 * 60) {
        let frame = live.frame();
        let stage = live.live().stage();
        assert_eq!(stage.scene().frame.index, frame.output.report.frame, "the scene rendered is the one submitted");
        let yaw = live.bus(Channel::HEAD_YAW);
        let (pitch, roll) = (live.bus(Channel::HEAD_PITCH), live.bus(Channel::HEAD_ROLL));
        let want = bus_turn(yaw, pitch, roll);
        let head = palette(stage, &JointRef::HEAD);
        assert!(stage.scene().joints.contains(&head), "the head's palette entry is in the submitted scene");
        let got = head_turn(stage);
        worst = worst.max(angle(got, want));
    }
    assert!(worst <= 1e-4, "the head is {worst} rad off the bus's head angles");
}

#[test]
fn the_same_trace_gives_the_same_frames() {
    let run = || {
        let mut live =
            Live::new(MockRenderer::new(), vec![("cam", turning(6.0, sweep))], Quality::live(), Painter::Mock);
        live.go_live("cam");
        (0..300)
            .map(|_| {
                live.frame();
                digest(live.live().stage().scene())
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(run(), run());
}

#[test]
fn three_yaws_move_the_head_monotonically_across_the_frame() {
    let yaw = |t: f32| {
        if t < 2.0 {
            -0.3
        } else if t < 4.0 {
            0.0
        } else {
            0.3
        }
    };
    let mut live = Live::new(MockRenderer::new(), vec![("cam", turning(6.0, yaw))], Quality::live(), Painter::Mock);
    live.go_live("cam");
    let mut seen = Vec::new();
    for second in 1..=3 {
        live.frames(120 * second - live.step);
        let stage = live.live().stage();
        let camera = &stage.scene().camera;
        let clip = camera.projection_matrix(1.0) * camera.view() * nose(stage).extend(1.0);
        seen.push((live.bus(Channel::HEAD_YAW), clip.x / clip.w));
    }
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "the bus yaw rises: {seen:?}");
    let rising = seen.windows(2).all(|w| w[0].1 < w[1].1);
    let falling = seen.windows(2).all(|w| w[0].1 > w[1].1);
    assert!(rising || falling, "the head's projection moves one way: {seen:?}");
}

#[test]
fn a_source_lost_mid_session_hands_the_face_to_life_and_rebinding_brings_it_back() {
    let cameras = vec![("a", face_for(3.0, 1)), ("b", face_for(30.0, 2))];
    let mut live = Live::new(MockRenderer::new(), cameras, Quality::live(), Painter::Mock);
    live.go_live("a");
    live.frames(150);
    assert_eq!(live.input.state().tracking, Tracking::Tracking);
    // Camera "a" stops seeing anything at 3 s (unplugged, say): the performer reports the face lost, and the head keeps
    // moving on life without a jump.
    live.log.unplug("a");
    // The retarget holds the last tracked face for its grace period, then hands it to life: measured from 4 s, a second
    // after the frames stopped.
    live.frames(90);
    let mut last = head_turn(live.live().stage());
    let (mut still, mut jump) = (0, 0.0_f32);
    for _ in 0..180 {
        live.frame();
        let now = head_turn(live.live().stage());
        let moved = angle(now, last);
        if moved < 1e-7 {
            still += 1;
        }
        jump = jump.max(moved);
        last = now;
    }
    let state = live.input.state();
    assert_eq!(state.tracking, Tracking::Lost);
    assert!(!state.camera_present, "the state says the camera is gone");
    assert_eq!(state.preview, PreviewState::On { camera: CameraId("a".into()) });
    assert_eq!(still, 0, "the head froze on {still} of 180 frames while the face was lost");
    assert!(jump <= 0.15, "the head jumped {jump} rad in one frame");

    // The person binds the other camera: the running preview moves to it and tracking resumes.
    live.input.bind_camera(Some(CameraId("b".into())), Mode::Apply).unwrap();
    live.frames(30);
    let state = live.input.state();
    assert_eq!(
        (state.tracking, state.preview),
        (Tracking::Tracking, PreviewState::On { camera: CameraId("b".into()) })
    );
}

#[test]
fn with_no_camera_the_life_layer_keeps_the_character_moving() {
    let mut live = Live::new(MockRenderer::new(), vec![("a", face_for(3.0, 1))], Quality::live(), Painter::Mock);
    live.input.select(SourceKind::Webcam, Mode::Apply).unwrap();
    assert_eq!(live.input.preview(true, Mode::Apply), Err(InputError::NoCameraBound));
    assert_eq!(
        live.input.bind_camera(Some(CameraId("gone".into())), Mode::Apply),
        Err(InputError::no_camera(CameraId("gone".into())))
    );
    let first = digest(live.live().stage().scene());
    live.frames(120);
    assert_ne!(digest(live.live().stage().scene()), first, "life moves the character with no camera");
    assert_eq!(live.input.state().tracking, Tracking::NoFrames);
    assert!(!live.log.started());
}

#[test]
fn the_character_s_expressions_are_installed_for_the_triggers() {
    let live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::Mock);
    let handle = live.live().handle();
    let face = live.live().stage().built().face.as_ref().expect("the mock character has a face");
    assert_eq!(handle.expressions(), face.profile.expressions);
    let expression = face.profile.expressions.0.keys().next().expect("the face has expressions").clone();
    let binding = TriggerBinding { key: "F1".into(), expression, mode: omuzaki_anim::TriggerMode::Toggle };
    handle.bind_trigger(binding).unwrap();
    assert!(!handle.triggers()[0].missing, "the expression is the character's, so the binding shows it");
}

#[test]
fn the_scheduler_measures_every_submission_and_steps_quality_down_over_budget() {
    let mut renderer = MockRenderer::new();
    // Every frame costs twice the live budget on the GPU.
    renderer.set_pass_cost(Pass::Opaque, Seconds(0.0001), Seconds(2.0 * budget::LIVE_GPU.0));
    let mut live = Live::new(renderer, vec![], Quality::live(), Painter::Mock);
    let mut changes = Vec::new();
    for _ in 0..120 {
        if let Some(change) = live.frame().change {
            changes.push(change);
        }
    }
    assert!(live.live().scheduler().smoothed().is_some(), "the scheduler measured the frames");
    assert!(!changes.is_empty(), "two frames' worth of GPU time per frame steps quality down");
    assert!(live.live().scheduler().rungs() > 0);
    assert_eq!(
        *live.live().stage().quality(),
        live.live().scheduler().quality(),
        "the stage renders at the scheduler's quality"
    );
}

#[test]
fn a_steady_live_frame_allocates_nothing() {
    // The stage and the performer, which this change owns; the face painter's repaints are measured on their own below.
    let mut live =
        Live::new(Quiet(Recorder::new(false)), vec![("cam", turning(30.0, sweep))], Quality::live(), Painter::None);
    live.go_live("cam");
    live.frames(120);
    let (result, allocations) = allocations_in(|| {
        for _ in 0..600 {
            live.try_frame()?;
        }
        Ok::<(), StageError>(())
    });
    result.unwrap();
    assert_eq!(allocations, 0, "600 live frames allocated {allocations} times");
}

#[test]
fn live_frames_fit_the_60_fps_budget() {
    let mut live = Live::new(Recorder::new(false), vec![("cam", turning(30.0, sweep))], Quality::live(), Painter::None);
    live.go_live("cam");
    live.frames(120);
    let mut times: Vec<Duration> = (0..1200)
        .map(|_| {
            let start = Instant::now();
            live.try_frame().unwrap();
            start.elapsed()
        })
        .collect();
    times.sort_unstable();
    let (p50, p99) = (times[times.len() / 2], times[times.len() * 99 / 100]);
    eprintln!("live frame (stage and performer, no drawing): p50 {p50:?}, p99 {p99:?} over {} frames", times.len());
    // The frame budget is 1/60 s; the optimised build is what runs live, so only it is held to it.
    if !cfg!(debug_assertions) {
        assert!(p99 < Duration::from_secs_f32(1.0 / STEP_HZ), "p99 {p99:?} is over a 60 fps frame");
    }
}

#[test]
fn the_real_face_painter_runs_live_and_its_cost_is_reported() {
    // The CPU painter's own cost and allocations belong to the face path (#2047 moves it to the GPU); this run shows
    // the live loop drives it and reports what a frame costs with it.
    let mut live =
        Live::new(Quiet(Recorder::new(false)), vec![("cam", turning(10.0, sweep))], Quality::live(), Painter::Standard);
    live.go_live("cam");
    live.frames(5);
    let mut times: Vec<Duration> = (0..30)
        .map(|_| {
            let start = Instant::now();
            live.try_frame().unwrap();
            start.elapsed()
        })
        .collect();
    times.sort_unstable();
    let ((), allocations) = allocations_in(|| live.frames(30));
    eprintln!(
        "live frame with the CPU face painter: p50 {:?}, p99 {:?}; {allocations} allocations over 30 frames",
        times[times.len() / 2],
        times[times.len() * 99 / 100]
    );
}

/// The scene document the live tests post: the mock character, two cameras and three lights.
fn scene_def() -> SceneDef {
    let mut def = SceneDef::new(SceneSettings::named("Live"));
    def.cast.insert(MemberId::new("kiri").unwrap(), CastMember::new(CharId::new("mock").unwrap()));
    for (name, framing) in [("front", Framing::Full), ("close", Framing::Bust)] {
        def.cameras.insert(SceneCameraId::new(name).unwrap(), CameraDef::preset(framing));
    }
    for (name, azimuth) in [("key", 30.0), ("fill", -50.0), ("rim", 150.0)] {
        let light = LightDef {
            role: None,
            colour: Colour::WHITE,
            intensity: 1.0,
            shadow: false,
            anchor: Anchor::Subject,
            shape: LightShape::Directional { azimuth, elevation: 40.0 },
        };
        def.lighting.lights.insert(LightId::new(name).unwrap(), light);
    }
    def
}

/// Resolves `def` for a 16:9 frame, as the scene service does before posting.
fn resolved(def: &SceneDef) -> ResolvedScene {
    omuzaki_stage::link();
    let mock = omuzaki_char_mock::mock_character().expect("the fixture parses");
    let built = RegistryBuilder
        .build(&CharId::new("mock").unwrap(), &mock, &BuildOptions::default(), &BuildServices::default())
        .expect("the mock character builds");
    let landmarks = omuzaki_stage::scene::landmarks::Landmarks::measure(&built).unwrap();
    let figures = BTreeMap::from([(
        MemberId::new("kiri").unwrap(),
        Figure::new(&CastMember::new(CharId::new("mock").unwrap()), landmarks),
    )]);
    let mut out = ResolvedScene::new();
    out.resolve_into(CheckedScene::new(def).unwrap(), &figures, 16.0 / 9.0).unwrap();
    out
}

#[test]
fn live_submissions_are_numbered_through_a_pause_a_redraw_and_a_restart() {
    let mut live = Live::new(MockRenderer::new(), vec![("cam", turning(12.0, sweep))], Quality::live(), Painter::Mock);
    live.go_live("cam");
    let mut reported = Vec::new();
    let mut scene_indices = Vec::new();
    let mut run = |live: &mut Live<MockRenderer>, n: u64| {
        for _ in 0..n {
            let frame = live.frame();
            reported.push(frame.output.report.frame);
            scene_indices.push(live.live().stage().scene().frame.index);
        }
    };
    run(&mut live, 20);
    live.inner.as_mut().unwrap().set_paused(true);
    run(&mut live, 20); // the paused loop redraws the same timeline position
    live.inner.as_mut().unwrap().set_paused(false);
    live.inner.as_mut().unwrap().restart(); // the timeline goes back to tick 0
    run(&mut live, 20);
    assert_eq!(reported.len(), 60);
    assert!(
        reported.windows(2).all(|pair| pair[1] > pair[0]),
        "every submission is numbered above the last: {reported:?}"
    );
    assert_eq!(reported, scene_indices, "the report names the submission the scene carried");
    assert!(live.live().scheduler().smoothed().is_some(), "the scheduler measured throughout");
}

#[test]
fn a_posted_scene_shows_in_the_next_frame_and_edits_follow() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    live.frames(3);
    let mailbox = live.live().scenes();
    let mut def = scene_def();
    let first = resolved(&def);
    mailbox.post(first.clone());
    live.frame();
    let stage = live.live().stage();
    assert_eq!(stage.scene().lights, first.lights.as_slice(), "the lights reach the next frame");
    assert_eq!(stage.scene().camera, first.cameras[first.active.unwrap()].1);
    let (camera, lights) = (stage.scene().camera, stage.scene().lights.clone());
    // The renderer owes the ground of the first scene; the app does that and commits it.
    let owed = live.live().owed();
    assert!(!owed.is_empty(), "a first scene with a ground owes the renderer a disc");
    live.inner.as_mut().unwrap().stage_mut().commit_scene(&owed);

    // A light colour edit and a camera yaw edit.
    def.lighting.lights.get_mut(&LightId::new("key").unwrap()).unwrap().colour = Colour::rgb(255, 0, 0);
    for camera in def.cameras.values_mut() {
        camera.yaw = 25.0;
    }
    mailbox.post(resolved(&def));
    live.frame();
    let stage = live.live().stage();
    assert_ne!(stage.scene().camera, camera, "the camera yaw shows");
    assert_ne!(stage.scene().lights, lights, "the light colour shows");
    assert!(
        stage
            .scene()
            .lights
            .iter()
            .any(|l| matches!(l, Light::Directional { colour, .. } if *colour == Colour::rgb(255, 0, 0)))
    );
    assert!(live.live().owed().is_empty(), "values only: the renderer is owed nothing");

    // The subject moves: every instance of the character follows it, below the root each frame brings.
    let before: Vec<Mat4> = live.live().stage().scene().instances.iter().map(|i| i.transform).collect();
    let step = Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0));
    live.inner.as_mut().unwrap().set_subject_root(step);
    live.frame();
    let after: Vec<Mat4> = live.live().stage().scene().instances.iter().map(|i| i.transform).collect();
    assert_ne!(before.len(), 0);
    assert!(
        before.iter().zip(&after).all(|(b, a)| (*a - step * *b).abs_diff_eq(Mat4::ZERO, 1e-5)),
        "the instances moved by the subject root"
    );
}

#[test]
fn applying_a_scene_allocates_nothing_the_first_submission_included() {
    let mut live =
        Live::new(Quiet(Recorder::new(false)), vec![("cam", turning(30.0, sweep))], Quality::live(), Painter::None);
    live.go_live("cam");
    live.frames(120); // warm-up: no scene has been posted yet
    let mailbox = live.live().scenes();
    let mut def = scene_def();
    // Built off the loop, as the scene service does; the first take is inside the counted frames.
    mailbox.post(resolved(&def));
    let (result, allocations) = allocations_in(|| {
        for _ in 0..300 {
            live.try_frame()?;
        }
        Ok::<(), StageError>(())
    });
    result.unwrap();
    assert_eq!(allocations, 0, "the first scene submission and 299 frames after it allocated {allocations} times");
    assert!(live.live().stage().scene().lights.len() > 1, "the first scene was taken");
    // A value-only edit, posted off the loop; taking it swaps, and nothing on the frame thread allocates or frees.
    def.lighting.lights.get_mut(&LightId::new("rim").unwrap()).unwrap().intensity = 2.0;
    mailbox.post(resolved(&def));
    let (result, allocations) = allocations_in(|| {
        for _ in 0..300 {
            live.try_frame()?;
        }
        Ok::<(), StageError>(())
    });
    result.unwrap();
    assert_eq!(allocations, 0, "a value-only edit and the frames after it allocated {allocations} times");
    assert!(
        live.live()
            .stage()
            .scene()
            .lights
            .iter()
            .any(|l| matches!(l, Light::Directional { intensity, .. } if (*intensity - 2.0).abs() < 1e-6))
    );
}

#[test]
fn a_catch_up_frame_and_a_restart_show_the_performers_time() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    live.frames(3);
    // The loop stalls for three steps: the next frame runs steps 3 to 6 and shows step 6.
    live.step += 3;
    let step = live.step;
    let frame = live.frame();
    let want = step as f64 / f64::from(STEP_HZ);
    assert!((frame.output.tick.time.0 - want).abs() < 1e-9, "stage time {} for step {step}", frame.output.tick.time.0);
    assert!((frame.output.tick.dt - 4.0 / STEP_HZ).abs() < 1e-6, "the frame covers four steps");
    // A restart takes the performer to tick 0, and the stage follows it there.
    live.inner.as_mut().unwrap().restart();
    let frame = live.frame();
    assert!(frame.output.tick.time.0.abs() < 1e-9, "the first frame after a restart is at time 0");
    let next = live.frame();
    assert!((next.output.tick.time.0 - 1.0 / f64::from(STEP_HZ)).abs() < 1e-9);
    assert!(next.output.report.frame > frame.output.report.frame, "submissions keep their order");
}

#[test]
fn resuming_from_a_pause_carries_on_from_the_last_step() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    live.frames(10);
    live.inner.as_mut().unwrap().set_paused(true);
    live.step += 600; // ten seconds go by
    live.frames(3);
    live.inner.as_mut().unwrap().set_paused(false);
    let first = live.frame();
    let want = 10.0 / f64::from(STEP_HZ);
    assert!((first.output.tick.time.0 - want).abs() < 1e-9, "resumes at step 10, not {}", first.output.tick.time.0);
    assert!(
        (first.output.tick.dt - 1.0 / STEP_HZ).abs() < 1e-6,
        "one step, not the paused time: {}",
        first.output.tick.dt
    );
    let second = live.frame();
    assert!((second.output.tick.dt - 1.0 / STEP_HZ).abs() < 1e-6, "and the frame after it too");
}

#[test]
fn moving_the_subject_before_any_scene_is_posted_keeps_the_stages_own_scene() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    live.inner.as_mut().unwrap().stage_mut().scene_mut().lights.push(Light::Directional {
        to_light: Vec3::Y,
        colour: Colour::rgb(10, 200, 30),
        intensity: 1.5,
        casts_shadow: false,
    });
    live.frame();
    let (lights, camera) = (live.live().stage().scene().lights.clone(), live.live().stage().scene().camera);
    assert_eq!(lights.len(), 1);
    let step = Mat4::from_translation(Vec3::new(0.0, 0.0, 1.0));
    let before: Vec<Mat4> = live.live().stage().scene().instances.iter().map(|i| i.transform).collect();
    live.inner.as_mut().unwrap().set_subject_root(step);
    live.frame();
    let stage = live.live().stage();
    assert_eq!(stage.scene().lights, lights, "the lights are still the stage's own");
    assert_eq!(stage.scene().camera, camera);
    let after: Vec<Mat4> = stage.scene().instances.iter().map(|i| i.transform).collect();
    assert!(
        before.iter().zip(&after).all(|(b, a)| (*a - step * *b).abs_diff_eq(Mat4::ZERO, 1e-5)),
        "the subject moved"
    );
}

#[test]
fn the_stage_renders_at_the_schedulers_quality_from_the_first_frame() {
    let live = Live::build(MockRenderer::new(), vec![], Quality::live(), Quality::high(), Painter::None, 64);
    assert_eq!(*live.live().stage().quality(), Quality::live());
}

#[test]
fn committing_the_owed_work_ends_the_debt() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    live.live().scenes().post(resolved(&scene_def()));
    live.frame();
    let owed = live.live().owed();
    assert!(!owed.is_empty(), "the ground is owed to the renderer");
    live.frame();
    assert_eq!(live.live().owed(), owed, "still owed until the app commits it");
    live.inner.as_mut().unwrap().stage_mut().commit_scene(&owed);
    assert!(live.live().owed().is_empty(), "committed: no longer owed");
    live.frame();
    assert!(live.live().owed().is_empty());
}

#[test]
fn a_late_commit_of_a_replaced_scenes_work_is_ignored() {
    let mut live = Live::new(MockRenderer::new(), vec![], Quality::live(), Painter::None);
    let mut def = scene_def();
    live.live().scenes().post(resolved(&def));
    live.frame();
    let late = live.live().owed();
    assert!(late.ground.is_some(), "scene A adds a ground");
    // Scene B removes it before A's work is committed: nothing is owed against the renderer's (empty) baseline.
    def.scene.ground.kind = omuzaki_stage_api::GroundKind::None;
    live.live().scenes().post(resolved(&def));
    live.frame();
    assert!(live.live().owed().is_empty());
    // A's work finishes late: the renderer now has A's ground, which B does not want, so the removal is owed, with no
    // other scene posted.
    live.inner.as_mut().unwrap().stage_mut().commit_scene(&late);
    let removal = live.live().owed();
    assert_eq!(removal.ground.map(|ground| ground.kind), Some(omuzaki_stage_api::GroundKind::None), "{removal:?}");
    live.inner.as_mut().unwrap().stage_mut().commit_scene(&removal);
    assert!(live.live().owed().is_empty(), "the ground is gone again");
    // Scene C wants the ground back: owed again.
    def.scene.ground.kind = omuzaki_stage_api::GroundKind::Shadow;
    live.live().scenes().post(resolved(&def));
    live.frame();
    assert!(live.live().owed().ground.is_some());
}

#[test]
fn an_expression_change_in_the_trace_costs_the_frame_nothing() {
    let mut live =
        Live::new(Quiet(Recorder::new(false)), vec![("cam", turning(30.0, sweep))], Quality::live(), Painter::None);
    live.go_live("cam");
    live.frames(120);
    let handle = live.live().handle().clone();
    // The app changes the expressions and a binding from its own thread, off the frame; the frame thread applies them.
    handle.set_expressions(ExpressionSet::starter(), omuzaki_anim_api::ChannelBuf::NEUTRAL).unwrap();
    let binding = TriggerBinding {
        key: "F1".to_owned(),
        expression: omuzaki_face_api::ExpressionId::new("happy").unwrap(),
        mode: omuzaki_anim::TriggerMode::Toggle,
    };
    handle.bind_trigger(binding).unwrap();
    handle.press("F1", true).unwrap();
    let (result, allocations) = allocations_in(|| {
        for _ in 0..600 {
            live.try_frame()?;
        }
        Ok::<(), StageError>(())
    });
    result.unwrap();
    assert_eq!(allocations, 0, "600 frames across an expression change and a press allocated {allocations} times");
    assert!(handle.triggers()[0].active, "and the press took effect");
}

#[cfg(feature = "wgpu")]
#[test]
fn the_fake_trace_renders_on_the_software_adapter() {
    use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
    let renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
        .expect("a software adapter (install mesa-vulkan-drivers)");
    let mut live = Live::sized(renderer, vec![("cam", turning(4.0, sweep))], Quality::live(), Painter::Standard, 384);
    live.go_live("cam");
    // 1 s into the sweep the head is turned about 0.4 rad.
    live.frames(60);
    let target = live.live().stage().target();
    let image = live.renderer.read_target(target).expect("the frame reads back");
    let background = image.rgba8[..4].to_vec();
    let drawn = image.rgba8.chunks(4).filter(|px| *px != background.as_slice()).count();
    assert!(drawn > image.rgba8.len() / 4 / 20, "the character covers {drawn} pixels of the frame");
    if let Some(dir) = std::env::var_os("OMUZAKI_STAGE_PNG") {
        let png = image::RgbaImage::from_raw(image.size.x, image.size.y, image.rgba8.clone()).expect("sized");
        png.save(std::path::Path::new(&dir).join("live-fake-trace.png")).expect("the PNG is written");
    }
}
