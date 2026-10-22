//! An imported avatar performing live (spec v3 §27 EX-12, §8, §10): a scripted webcam, through `LiveInput`, the
//! performer and the one live loop, to the `AvatarStage` and the scene the renderer draws. The same loop, input and
//! performer as the kit character's (`tests/live.rs`); only the stage differs.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "avatar/kit.rs"]
mod kit;
#[path = "live/support.rs"]
mod support;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim::BusAnimatorParams;
use omuzaki_anim_api::{BoneId, Channel, DeviceProfile, HeadPose, HumanoidBone, TrackFrame};
use omuzaki_core::glam::{EulerRot, Mat4, Quat, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_ops_api::Mode;
use omuzaki_render_api::{
    AlphaEncoding, Camera, Capabilities, FrameReport, Image, Material, MaterialId, MeshData, MeshId, MeshOptions,
    PaintTarget, Painter as RenderPainter, Projection, Quality, RenderError, Renderer, ResourceRef, ResourceStats,
    Scene, TargetDesc, TargetId, TextureDesc, TextureId, budget,
};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::StageError;
use omuzaki_stage::avatar::AvatarStage;
use omuzaki_stage::frame::StageClock;
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{InputHost, LiveInput, NoPrompt, SourceKind, SourceSlot};
use omuzaki_stage::perform::live::{LiveFrame, LiveStage};
use omuzaki_stage::perform::{STEP_HZ, Tracking};
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::TrackerConfig;

use kit::{as_vrm, avatar_of, kit};
use support::{Cameras, mid_step, performer};

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

/// The mock renderer with its own allocations left out of [`allocations_in`]: a GPU renderer's are its own cost.
struct Quiet(MockRenderer);

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
        Vec3::new(0.0, 0.8, 2.0),
        Vec3::new(0.0, 0.8, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ))
}

/// The imported kit performing live on `renderer`, from `cameras`, with no camera bound yet.
struct Live<R: Renderer> {
    renderer: R,
    inner: Option<LiveStage<AvatarStage>>,
    input: LiveInput,
    /// The model matrices of the neck's parent and the head at the avatar's bind pose.
    bind: (Mat4, Mat4),
    neck_parent: BoneId,
    head: BoneId,
    step: u64,
}

impl<R: Renderer> Live<R> {
    fn new(mut renderer: R, cameras: Vec<(&str, Vec<(Duration, TrackFrame)>)>) -> Self {
        let avatar = avatar_of(&as_vrm(&kit()));
        let head = avatar.skeleton.humanoid_bone(HumanoidBone::Head).expect("a head");
        let neck = avatar.skeleton.humanoid_bone(HumanoidBone::Neck).expect("a neck");
        let neck_parent = avatar.skeleton.bones()[neck.index()].parent.expect("the neck hangs from a bone");
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::splat(32), alpha: AlphaEncoding::Straight }).unwrap();
        let mut stage =
            AvatarStage::new(&mut renderer, avatar, scene(), target, Quality::live(), StageClock::live()).unwrap();
        let base = stage.base_pose();
        // The bind-pose matrices, from a frame at the base pose before any performer runs.
        stage.tick(&mut renderer, &omuzaki_stage::frame::FrameInput { pose: base, ..Default::default() }).unwrap();
        let bind = (stage.bone_world(neck_parent).unwrap(), stage.bone_world(head).unwrap());
        let sources = Arc::new(SourceSlot::new());
        let live =
            LiveStage::new(performer(&base), stage, Arc::clone(&sources), Quality::live(), budget::LIVE_GPU).unwrap();
        let perform: Arc<dyn PerformHost> = Arc::new(live.handle().clone());
        let (tracker, _log) = Cameras::new(cameras);
        let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), sources, perform);
        Self { renderer, inner: Some(live), input, bind, neck_parent, head, step: 0 }
    }

    fn live(&self) -> &LiveStage<AvatarStage> {
        self.inner.as_ref().expect("the loop runs until the drop")
    }

    fn try_frame(&mut self) -> Result<Option<LiveFrame>, StageError> {
        let now = mid_step(self.step);
        self.step += 1;
        self.inner.as_mut().expect("the loop runs").frame(&mut self.renderer, now)
    }

    fn frame(&mut self) -> LiveFrame {
        self.try_frame().unwrap().expect("one step is due per frame")
    }

    fn go_live(&self, camera: &str) {
        self.input.select(SourceKind::Webcam, Mode::Apply).unwrap();
        self.input.bind_camera(Some(CameraId(camera.to_owned())), Mode::Apply).unwrap();
        self.input.preview(true, Mode::Apply).unwrap();
    }

    fn bus(&self, channel: Channel) -> f32 {
        self.live().input().channels[channel]
    }

    /// What the neck and head add on top of the body since the bind pose, in model space.
    fn head_turn(&self) -> Quat {
        let stage = self.live().stage();
        let turn = |now: Mat4, bind: Mat4| (now * bind.inverse()).to_scale_rotation_translation().1;
        let parent = turn(stage.bone_world(self.neck_parent).unwrap(), self.bind.0);
        let head = turn(stage.bone_world(self.head).unwrap(), self.bind.1);
        parent.inverse() * head
    }
}

impl<R: Renderer> Drop for Live<R> {
    fn drop(&mut self) {
        if let Some(live) = self.inner.take() {
            live.into_stage().release(&mut self.renderer).expect("the stage releases");
        }
    }
}

/// The angle between two rotations, precise for small angles.
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

#[test]
fn a_webcam_turns_the_imported_avatars_head_with_the_bus() {
    let mut live = Live::new(MockRenderer::new(), vec![("cam", turning(12.0, sweep))]);
    live.go_live("cam");
    for _ in 0..60 {
        live.frame();
    }
    assert_eq!(live.input.state().tracking, Tracking::Tracking);
    let (mut worst, mut widest) = (0.0_f32, 0.0_f32);
    for _ in 0..(4 * 60) {
        let frame = live.frame();
        assert_eq!(live.live().stage().scene().frame.index, frame.output.report.frame);
        let (yaw, pitch, roll) =
            (live.bus(Channel::HEAD_YAW), live.bus(Channel::HEAD_PITCH), live.bus(Channel::HEAD_ROLL));
        widest = widest.max(yaw.abs());
        worst = worst.max(angle(live.head_turn(), bus_turn(yaw, pitch, roll)));
    }
    assert!(widest > 0.3, "the sweep reached the bus: {widest} rad at most");
    assert!(worst <= 1e-3, "the imported head is {worst} rad off the bus's head angles");
}

#[test]
fn a_steady_live_frame_of_an_imported_avatar_allocates_nothing() {
    let mut live = Live::new(Quiet(MockRenderer::new()), vec![("cam", turning(30.0, sweep))]);
    live.go_live("cam");
    for _ in 0..120 {
        live.frame();
    }
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
fn imported_live_frames_fit_the_60_fps_budget() {
    let mut live = Live::new(Quiet(MockRenderer::new()), vec![("cam", turning(30.0, sweep))]);
    live.go_live("cam");
    for _ in 0..120 {
        live.frame();
    }
    let mut times: Vec<Duration> = (0..1200)
        .map(|_| {
            let start = Instant::now();
            uncounted(|| live.try_frame().unwrap());
            start.elapsed()
        })
        .collect();
    times.sort_unstable();
    let (p50, p99) = (times[times.len() / 2], times[times.len() * 99 / 100]);
    eprintln!("imported live frame (stage, performer and the mock's drawing): p50 {p50:?}, p99 {p99:?}");
    if !cfg!(debug_assertions) {
        assert!(p99 < Duration::from_secs_f32(1.0 / STEP_HZ), "p99 {p99:?} is over a 60 fps frame");
    }
}
