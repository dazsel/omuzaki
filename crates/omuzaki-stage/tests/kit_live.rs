//! The kit's own character, made as a person or an agent makes it and performing live: the character spine's
//! exists-when test (CLAUDE.md, spine map §3).
//!
//! `character.create` with its default parts (a body with hands, a head, hair, and an outfit of a shirt, trousers and
//! boots) is planned and applied on an in-memory project as the executor does, built by the caching builder the app
//! uses, and checked to be complete and rigged on the VRM humanoid skeleton. Then it performs: a scripted camera,
//! through `LiveInput`, the real retarget, life layer and animator, the rig and its spring chains, and the part runtimes
//! (hair, cloth), stepped at 60 Hz to the scene the renderer draws. Everything must hold: the head follows the tracked
//! turn, no vertex leaves the character's reach or stops being finite, and the simulations come back to rest once the
//! performance stops.
//!
//! With `OMUZAKI_KIT_LIVE_DIR` set, the evidence test writes the character's turnaround and frames of its performance
//! there as PNG, on the software adapter (lavapipe) when the stage is built with `wgpu`, through the render mock
//! otherwise.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

#[path = "live/support.rs"]
mod support;

use std::sync::Arc;
use std::time::{Duration, Instant};

use omuzaki_anim_api::{Channel, DeviceProfile, HeadPose, HumanoidBone, HumanoidPose, TrackFrame};
use omuzaki_avatar_api::body::Body;
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef};
use omuzaki_core::glam::{EulerRot, Quat, UVec2, Vec3};
use omuzaki_core::time::{FakeClock, Seconds};
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, Mode, OpCtx, find};
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc, budget};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{Stage, StageClock};
use omuzaki_stage::perform::host::PerformHost;
use omuzaki_stage::perform::input::{InputHost, LiveInput, NoPrompt, SourceKind, SourceSlot};
use omuzaki_stage::perform::live::LiveStage;
use omuzaki_stage::perform::{STEP_HZ, Tracking};
use omuzaki_stage::recipe::LightRig;
use omuzaki_stage::{StageError, UploadOptions};
use omuzaki_track_api::camera::CameraId;
use omuzaki_track_api::tracker::TrackerConfig;
use serde_json::json;

use support::{Cameras, mid_step, performer};

/// The templates the spine is checked on: the chibi default and the anime-standard proportions.
const TEMPLATES: [&str; 2] = ["soft-sd", "standard"];

/// The parts `character.create` gives a new character, in build order.
const DEFAULT_PARTS: [&str; 6] = ["body", "head", "hair", "shirt", "trousers", "boots"];

/// The live loop's rate: the performer steps at it, and so do the springs and runtimes.
const HZ: u64 = STEP_HZ as u64;

// ------------------------------------------------------------------------------------------------ the character

/// The default character of `template`, created through `character.create` on an empty project.
fn create(template: &str) -> CharacterDef {
    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(json!({"id": "kit", "name": "Kit", "template": template, "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).unwrap_or_else(|e| panic!("{template}: refused: {e:?}"));
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("the character is stored").clone();
    CharacterDef::from_node(&node).expect("the stored character reads back")
}

/// The default character of `template`, built by the caching builder the app runs.
fn kit(template: &str) -> BuiltCharacter {
    let def = create(template);
    CachingBuilder::new()
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("{template}: the default character doesn't build: {e}"))
}

#[test]
fn the_default_character_is_complete_dressed_and_rigged_on_the_vrm_humanoid() {
    for template in TEMPLATES {
        let built = kit(template);
        let parts: Vec<&str> = built.report.parts.iter().map(|p| p.part.as_str()).collect();
        assert_eq!(parts, DEFAULT_PARTS, "{template}: the default parts, each built");
        for part in &built.report.parts {
            assert!(part.triangles > 0, "{template}: `{}` built no geometry", part.part);
        }
        let hands = built.meshes.iter().filter(|m| m.mesh.name.as_str().contains("hand")).count();
        assert!(hands >= 2, "{template}: two hands, as meshes of their own ({hands})");

        // Every humanoid bone VRM requires, and the fingers the hands are skinned to.
        let skeleton = &built.rig.skeleton;
        for bone in HumanoidBone::ALL.into_iter().filter(|b| b.is_required()) {
            assert!(skeleton.humanoid_bone(bone).is_some(), "{template}: no {bone:?}");
        }
        for bone in [HumanoidBone::LeftIndexProximal, HumanoidBone::RightLittleDistal, HumanoidBone::LeftThumbDistal] {
            assert!(skeleton.humanoid_bone(bone).is_some(), "{template}: no {bone:?}");
        }

        // What the stage, export and the doctor see: the same meshes, the skeleton, the painted face, the simulations.
        let (meshes, runtimes) = (built.meshes.len(), built.runtimes.len());
        let body = Body::Built(Box::new(built));
        let view = body.view();
        assert_eq!(view.meshes.len(), meshes, "{template}");
        assert!(view.face.is_some(), "{template}: the painted face is bound to the head");
        assert!(view.expressions.is_none() && view.export.is_none(), "{template}: a built body exports from its parts");
        assert!(runtimes > 0, "{template}: the hair (and any cloth) simulates");

        // Built for export, the hair and the garments' cloth carry spring chains, which VRM writes as spring bones.
        let options = BuildOptions { rig_export: true, ..BuildOptions::default() };
        let exported = CachingBuilder::new()
            .build(&CharId::new("kit").unwrap(), &create(template), &options, &BuildServices::default())
            .unwrap_or_else(|e| panic!("{template}: the export build fails: {e}"));
        let chains = exported.rig.springs.chains.len();
        assert!(chains > 0, "{template}: no spring chains to export");
        eprintln!(
            "{template}: {meshes} meshes, {} triangles, {} bones, {} spring chains, {runtimes} part runtimes",
            view.meshes.iter().map(|m| m.mesh.geometry.triangle_count()).sum::<usize>(),
            view.skeleton.len(),
            view.springs.chains.len(),
        );
        eprintln!("{template}: built for export, {chains} spring chains");
    }
}

// ------------------------------------------------------------------------------------------------ performing live

/// The camera's framing: the whole character, from the front.
fn scene(built: &BuiltCharacter) -> Scene {
    let (lo, hi) = bounds(built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().copied()));
    let centre = 0.5 * (lo + hi);
    let height = hi.y - lo.y;
    let fov_y_deg = 30.0_f32;
    let distance = 0.62 * height / (0.5 * fov_y_deg.to_radians()).tan();
    let mut scene = Scene::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, distance),
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
    ));
    // The studio's own viewport light, so the performance frames show the character as the studio does.
    let rig = LightRig::load("viewer-studio").unwrap();
    scene.lights = rig.to_lights().unwrap();
    scene.background = rig.background();
    scene.post = rig.post(false);
    scene
}

fn bounds(points: impl Iterator<Item = Vec3>) -> (Vec3, Vec3) {
    points.fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), p| (lo.min(p), hi.max(p)))
}

/// A camera that turns, nods and tilts the head through a slow figure for `seconds`, 30 frames a second, with the face
/// talking and blinking now and then.
fn performance(seconds: f32) -> Vec<(Duration, TrackFrame)> {
    (0..(seconds * 30.0) as u64)
        .map(|i| {
            let t = i as f32 / 30.0;
            let mut frame = TrackFrame::empty(i, Seconds(f64::from(t)), DeviceProfile::Webcam);
            let (yaw, pitch, roll) = (0.6 * (0.9 * t).sin(), 0.25 * (1.7 * t).sin(), 0.15 * (1.3 * t).cos());
            frame.head = Some(HeadPose { yaw, pitch, roll, position: None });
            let blink = if t % 3.0 < 0.15 { 1.0 } else { 0.0 };
            let jaw = 0.3 * (1.0 + (7.0 * t).sin());
            for (channel, value) in
                [(Channel::JAW_OPEN, jaw), (Channel::EYE_BLINK_LEFT, blink), (Channel::EYE_BLINK_RIGHT, blink)]
            {
                frame.blend[channel.index()] = value;
                frame.present.insert(channel);
            }
            (Duration::from_nanos((f64::from(t) * 1e9) as u64), frame)
        })
        .collect()
}

/// The live world around the kit character: the loop, the input the person drives, and the renderer.
struct Live<R: Renderer> {
    renderer: R,
    inner: Option<LiveStage>,
    input: LiveInput,
    step: u64,
}

impl<R: Renderer> Live<R> {
    fn new(mut renderer: R, built: BuiltCharacter, camera: Vec<(Duration, TrackFrame)>, size: u32) -> Self {
        let mut base = HumanoidPose::REST;
        seed_base_pose(&built.rig, &mut base);
        let scene = scene(&built);
        let target =
            renderer.create_target(&TargetDesc { size: UVec2::splat(size), alpha: AlphaEncoding::Straight }).unwrap();
        let options = UploadOptions { face_px_per_m: 2000.0 };
        let mut stage =
            Stage::new(&mut renderer, built, options, scene, target, Quality::live(), StageClock::live()).unwrap();
        stage.set_face(&mut renderer, FaceRig::standard().with_executor(Box::new(InlineExecutor))).unwrap();
        let sources = Arc::new(SourceSlot::new());
        let live =
            LiveStage::new(performer(&base), stage, Arc::clone(&sources), Quality::live(), budget::LIVE_GPU).unwrap();
        let perform: Arc<dyn PerformHost> = Arc::new(live.handle().clone());
        let (tracker, _log) = Cameras::new(vec![("cam", camera)]);
        let input = LiveInput::new(Box::new(tracker), Box::new(NoPrompt), TrackerConfig::default(), sources, perform);
        input.select(SourceKind::Webcam, Mode::Apply).unwrap();
        input.bind_camera(Some(CameraId("cam".to_owned())), Mode::Apply).unwrap();
        input.preview(true, Mode::Apply).unwrap();
        Self { renderer, inner: Some(live), input, step: 0 }
    }

    fn live(&self) -> &LiveStage {
        self.inner.as_ref().expect("the loop runs until the drop")
    }

    fn stage(&self) -> &Stage {
        self.live().stage()
    }

    /// One display frame at the middle of the next step: exactly one step of everything.
    fn frame(&mut self) -> Result<(), StageError> {
        let now = mid_step(self.step);
        self.step += 1;
        let live = self.inner.as_mut().expect("the loop runs until the drop");
        live.frame(&mut self.renderer, now)?.expect("one step is due per frame");
        Ok(())
    }

    fn set_wind(&mut self, wind: Vec3) {
        self.inner.as_mut().expect("the loop runs until the drop").input_mut().wind = wind;
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

/// The turn of the head relative to the bone the neck hangs from, in the submitted joint palette.
fn head_turn(stage: &Stage) -> Quat {
    let rig = &stage.built().rig;
    let turned = |index: usize| stage.skin()[index].to_scale_rotation_translation().1;
    let neck = rig.bone(&JointRef::Humanoid(HumanoidBone::Neck)).expect("a neck");
    let parent = rig.skeleton.bones()[neck.index()].parent.expect("the neck has a parent");
    turned(parent.index()).inverse() * turned(rig.bone(&JointRef::HEAD).expect("a head").index())
}

/// What the animator turns the neck and head through together for the bus's head angles: the whole rotation, which
/// it splits between them so that Neck · Head is exactly this.
fn bus_turn(yaw: f32, pitch: f32, roll: f32) -> Quat {
    Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll)
}

fn angle(a: Quat, b: Quat) -> f32 {
    2.0 * (a.inverse() * b).xyz().length().min(1.0).asin()
}

/// The largest distance between matching vertices of two frames.
fn largest_move(a: &[Vec3], b: &[Vec3]) -> f32 {
    a.iter().zip(b).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max)
}

#[test]
fn the_default_character_performs_live_and_everything_holds() {
    for template in TEMPLATES {
        let built = kit(template);
        let rest = {
            let (lo, hi) = bounds(built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().copied()));
            (lo, hi, (hi - lo).length())
        };
        let seconds = 8.0;
        let mut live = Live::new(MockRenderer::new(), built, performance(seconds), 64);

        // Half a second for the source to bind, then the performance with a gusting breeze from the side.
        for _ in 0..HZ / 2 {
            live.frame().unwrap();
        }
        assert_eq!(live.input.state().tracking, Tracking::Tracking, "{template}");
        let (mut worst_turn, mut steps, mut spent) = (0.0_f32, 0u32, Duration::ZERO);
        let mut previous = live.stage().drawn_vertices();
        let mut fastest = 0.0_f32;
        let performing = (seconds as u64 - 1) * HZ;
        for i in 0..performing {
            let gust = 1.5 * (0.5 + 0.5 * (i as f32 * 0.05).sin());
            live.set_wind(Vec3::new(gust, 0.0, 0.3 * gust));
            let start = Instant::now();
            live.frame().unwrap();
            spent += start.elapsed();
            steps += 1;
            let stage = live.stage();
            let want =
                bus_turn(live.bus(Channel::HEAD_YAW), live.bus(Channel::HEAD_PITCH), live.bus(Channel::HEAD_ROLL));
            worst_turn = worst_turn.max(angle(head_turn(stage), want));
            let drawn = stage.drawn_vertices();
            for p in &drawn {
                assert!(p.is_finite(), "{template}: a vertex went non-finite at step {i}");
                let reach = (p.clamp(rest.0, rest.1) - *p).length();
                assert!(reach < 0.5 * rest.2, "{template}: a vertex flew {reach} m out of the character at step {i}");
            }
            fastest = fastest.max(largest_move(&previous, &drawn) * STEP_HZ);
            previous = drawn;
        }
        assert!(worst_turn <= 1e-4, "{template}: the head is {worst_turn} rad off the tracked turn");
        let scale = rest.2;
        assert!(fastest < 8.0 * scale, "{template}: a vertex moved at {fastest} m/s (the character is {scale} m)");

        // The camera has stopped and the wind dropped: the life layer keeps breathing, the simulations settle.
        live.set_wind(Vec3::ZERO);
        for _ in 0..3 * HZ {
            live.frame().unwrap();
        }
        let mut settled = 0.0_f32;
        let mut before = live.stage().drawn_vertices();
        for _ in 0..HZ {
            live.frame().unwrap();
            let after = live.stage().drawn_vertices();
            settled = settled.max(largest_move(&before, &after) * STEP_HZ);
            before = after;
        }
        assert!(settled < 0.5, "{template}: still moving at {settled} m/s once the performance stopped");
        eprintln!(
            "{template}: head within {worst_turn:.2e} rad of the bus; fastest vertex {fastest:.2} m/s; at rest \
             {settled:.3} m/s; one frame (performer, rig and springs, runtimes, face, scene) {:.2} ms on average in \
             this build",
            spent.as_secs_f64() * 1e3 / f64::from(steps),
        );
    }
}

// ------------------------------------------------------------------------------------------------ evidence

/// Writes the default character's turnaround and frames of its performance into `OMUZAKI_KIT_LIVE_DIR`, for the spine's
/// evidence: on lavapipe with the `wgpu` feature, through the render mock without it.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_default_character_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_KIT_LIVE_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_KIT_LIVE_DIR to write the pictures");
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    for template in TEMPLATES {
        #[cfg(feature = "wgpu")]
        let renderer = {
            use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
            WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
                .expect("a software adapter (install mesa-vulkan-drivers)")
        };
        #[cfg(not(feature = "wgpu"))]
        let renderer = MockRenderer::new();
        evidence(renderer, template, &directory);
    }
}

#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn evidence<R: Renderer>(renderer: R, template: &str, directory: &std::path::Path) {
    use omuzaki_stage::ops::render::still::encode_png;
    use omuzaki_stage::sheet::turnaround::{Lens, TurnaroundSpec, render_turnaround};

    let mut live = Live::new(renderer, kit(template), performance(4.0), 720);
    let target = live.stage().target();
    for i in 0..4 * HZ {
        live.frame().unwrap();
        if i % HZ == HZ - 1 {
            let image = live.renderer.read_target(target).unwrap();
            let name = format!("{template}-live-{}s.png", (i + 1) / HZ);
            std::fs::write(directory.join(name), encode_png(&image).unwrap()).unwrap();
        }
    }
    let spec = TurnaroundSpec::new(UVec2::new(360, 720)).with_lens(Lens::Orthographic).with_settle_steps(60);
    let inner = live.inner.as_mut().expect("the loop runs until the drop");
    let sheet = render_turnaround(inner.stage_mut(), &mut live.renderer, &spec).expect("the turnaround renders");
    for (i, view) in sheet.views.iter().enumerate() {
        let name = format!("{template}-turnaround-{i}.png");
        std::fs::write(directory.join(name), encode_png(&view.image).unwrap()).unwrap();
    }
}
