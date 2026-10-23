//! A cast on one stage (`omuzaki_stage::cast`): a lead and another member prepared together and rendered once, each at
//! its own place with its own input, through the render mock; the steady-state frame allocates nothing; the live loop
//! runs a cast as it runs one body. With `OMUZAKI_CAST_DIR` set, the kit's two default characters are rendered side by
//! side for the evidence (on lavapipe with the `wgpu` feature).

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

use omuzaki_anim_api::{Channel, HumanoidPose};
use omuzaki_char_api::BuiltCharacter;
use omuzaki_core::glam::{Mat4, UVec2, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc, budget};
use omuzaki_render_mock::MockRenderer;
use omuzaki_stage::cast::CastStage;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::perform::input::SourceSlot;
use omuzaki_stage::perform::live::LiveStage;
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage::{StageError, UploadOptions};
use omuzaki_stage_api::MemberId;

use double::{Recorder, built};
use support::{mid_step, performer};

// ------------------------------------------------------------------------------------------------ counting

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

// ------------------------------------------------------------------------------------------------ helpers

fn id(name: &str) -> MemberId {
    MemberId::checked(name).expect("a valid member id")
}

fn scene() -> Scene {
    Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 3.0),
        Vec3::new(0.0, 0.8, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 40.0, near: 0.05, far: 20.0 },
    ))
}

/// A stage for `character` on `renderer`, rendering into a 64 px target of its own.
fn stage(renderer: &mut dyn Renderer, character: BuiltCharacter) -> Stage {
    let target =
        renderer.create_target(&TargetDesc { size: UVec2::splat(64), alpha: AlphaEncoding::Straight }).unwrap();
    let options = UploadOptions { face_px_per_m: 2000.0 };
    Stage::new(renderer, character, options, scene(), target, Quality::live(), StageClock::live()).unwrap()
}

/// The mock character's rest input at `t` seconds, a 60 Hz frame.
fn input(t: f64) -> FrameInput {
    let character = built();
    let mut pose = HumanoidPose::REST;
    seed_base_pose(&character.rig, &mut pose);
    FrameInput { pose, dt: 1.0 / 60.0, time: Some(Seconds(t)), ..FrameInput::default() }
}

fn left() -> Mat4 {
    Mat4::from_translation(Vec3::new(-0.5, 0.0, 0.0))
}

fn right() -> Mat4 {
    Mat4::from_translation(Vec3::new(0.5, 0.0, 0.0))
}

/// A two-member cast of the mock character: `lead` on the left, `guest` on the right.
fn pair(renderer: &mut dyn Renderer) -> CastStage<Stage> {
    let mut lead = stage(renderer, built());
    lead.set_subject_root(left());
    let mut cast = CastStage::new(id("lead"), lead);
    let guest = stage(renderer, built());
    assert!(cast.add_member(id("guest"), guest, right()).is_none(), "a new member replaces nothing");
    cast
}

// ------------------------------------------------------------------------------------------------ tests

#[test]
fn a_second_member_is_drawn_in_the_same_frame_at_its_own_place() {
    let mut renderer = Recorder::new(false);
    let mut alone = stage(&mut renderer, built());
    alone.tick(&mut renderer, &input(0.0)).unwrap();
    let (n, joints) = (alone.scene().instances.len(), alone.scene().joints.len());
    assert!(n > 0 && joints > 0, "the mock character draws skinned meshes");

    let mut cast = pair(&mut renderer);
    assert_eq!(cast.len(), 2);
    assert_eq!(cast.ids().map(MemberId::as_str).collect::<Vec<_>>(), ["lead", "guest"], "the lead draws first");
    FrameLoop::tick(&mut cast, &mut renderer, &input(0.0)).unwrap();
    let scene = cast.scene();
    assert_eq!(scene.instances.len(), 2 * n, "both bodies, in one scene");
    assert_eq!(scene.joints.len(), 2 * joints, "both joint palettes");
    let (lead, guest) = scene.instances.split_at(n);
    for (a, b) in lead.iter().zip(guest) {
        assert_eq!((a.order, a.casts_shadow), (b.order, b.casts_shadow), "the same character, uploaded twice");
        assert!((a.transform.w_axis.x + 0.5).abs() < 1e-6, "the lead stands at its root");
        assert!((b.transform.w_axis.x - 0.5).abs() < 1e-6, "the guest stands at its own");
        match (a.skin, b.skin) {
            (Some(sa), Some(sb)) => {
                assert_eq!(sb.len, sa.len);
                assert_eq!(sb.start, sa.start + joints as u32, "the guest's skin reads its own palette");
                let (pa, pb) = (sa.start as usize, sb.start as usize);
                assert_eq!(
                    scene.joints[pa..pa + sa.len as usize],
                    scene.joints[pb..pb + sb.len as usize],
                    "one character in one pose: the same skin, each in its own range"
                );
            }
            (None, None) => {}
            other => panic!("the two copies differ in skinning: {other:?}"),
        }
    }
    FrameLoop::release(cast, &mut renderer).unwrap();
    alone.release(&mut renderer).unwrap();
}

#[test]
fn the_cast_renders_once_with_every_member_in_the_report() {
    let mut renderer = MockRenderer::new();
    let mut alone = stage(&mut renderer, built());
    let one = alone.tick(&mut renderer, &input(0.0)).unwrap().report;
    let mut cast = pair(&mut renderer);
    let both = FrameLoop::tick(&mut cast, &mut renderer, &input(0.0)).unwrap().report;
    assert_eq!(both.triangles, 2 * one.triangles, "one render drew both members' triangles");
    assert_eq!(both.draw_calls, 2 * one.draw_calls);
    // Taking the guest out takes it off the next frame; the lead's own scene is as it was.
    let guest = cast.remove_member(&id("guest")).expect("the guest is in the cast");
    let after = FrameLoop::tick(&mut cast, &mut renderer, &input(1.0 / 60.0)).unwrap().report;
    assert_eq!(after.triangles, one.triangles);
    assert_eq!(cast.scene().instances.len(), alone.scene().instances.len());
    guest.release(&mut renderer).unwrap();
    FrameLoop::release(cast, &mut renderer).unwrap();
    alone.release(&mut renderer).unwrap();
}

#[test]
fn each_member_runs_its_own_input_on_the_leads_clock() {
    let mut renderer = Recorder::new(false);
    let mut cast = pair(&mut renderer);
    let guest = id("guest");
    let mut theirs = input(0.0);
    theirs.root = Mat4::from_translation(Vec3::new(0.0, 0.25, 0.0));
    // Another clock than the lead's: the cast runs on the lead's.
    theirs.time = Some(Seconds(9.0));
    let lead = input(2.0);
    cast.tick_members(&mut renderer, &[(id("lead"), &lead), (guest.clone(), &theirs)]).unwrap();
    let n = cast.scene().instances.len() / 2;
    let raised = &cast.scene().instances[n..];
    assert!(raised.iter().all(|i| (i.transform.w_axis.y - 0.25).abs() < 1e-6), "the guest's own input moved it");
    assert!(cast.scene().instances[..n].iter().all(|i| i.transform.w_axis.y.abs() < 1e-6), "the lead's didn't");
    let time = |stage: &Stage| stage.scene().frame.time;
    assert_eq!(time(cast.member(&guest).unwrap()), Seconds(2.0), "one clock for the cast");
    assert_eq!(time(cast.lead()), Seconds(2.0));

    // A member keeps its last input; a redraw draws every member again without stepping.
    FrameLoop::tick(&mut cast, &mut renderer, &input(2.0 + 1.0 / 60.0)).unwrap();
    assert!(cast.scene().instances[n..].iter().all(|i| (i.transform.w_axis.y - 0.25).abs() < 1e-6));
    FrameLoop::redraw(&mut cast, &mut renderer, &input(0.0)).unwrap();
    assert_eq!(cast.scene().instances.len(), 2 * n, "a redraw draws the whole cast, once");

    // Placing a member moves it alone.
    assert!(cast.set_member_root(&guest, Mat4::from_translation(Vec3::new(1.5, 0.0, 0.0))));
    assert!(!cast.set_member_root(&id("nobody"), Mat4::IDENTITY), "an id not in the cast");
    FrameLoop::tick(&mut cast, &mut renderer, &input(3.0)).unwrap();
    assert!(cast.scene().instances[n..].iter().all(|i| (i.transform.w_axis.x - 1.5).abs() < 1e-6));
    assert!(cast.scene().instances[..n].iter().all(|i| (i.transform.w_axis.x + 0.5).abs() < 1e-6));
    FrameLoop::release(cast, &mut renderer).unwrap();
}

#[test]
fn the_lead_cannot_be_replaced_and_a_member_can() {
    let mut renderer = Recorder::new(false);
    let mut cast = pair(&mut renderer);
    let spare = stage(&mut renderer, built());
    let back = cast.add_member(id("lead"), spare, Mat4::IDENTITY).expect("the lead's id hands the stage back");
    let old = cast.add_member(id("guest"), back, right()).expect("the guest's stage comes back to release");
    assert_eq!(cast.len(), 2);
    assert!(cast.remove_member(&id("lead")).is_none(), "the lead stays");
    assert!(cast.trigger_expressions_of(&id("guest")).is_some());
    assert!(cast.trigger_expressions_of(&id("nobody")).is_none());
    old.release(&mut renderer).unwrap();
    FrameLoop::release(cast, &mut renderer).unwrap();
}

#[test]
fn a_two_member_frame_allocates_nothing_in_steady_state() {
    let mut renderer = Recorder::new(false);
    let mut cast = pair(&mut renderer);
    let (lead, guest) = (id("lead"), id("guest"));
    // `input` builds a character to seed its pose: the inputs are made before anything is counted.
    let frames: Vec<(FrameInput, FrameInput)> = (0..90)
        .map(|i| {
            let t = f64::from(i) / 60.0;
            let (mut ours, mut theirs) = (input(t), input(t));
            ours.channels[Channel::JAW_OPEN] = (t.sin() * 0.5 + 0.5) as f32;
            theirs.channels[Channel::JAW_OPEN] = (t.cos() * 0.5 + 0.5) as f32;
            theirs.root = Mat4::from_rotation_y(t as f32);
            (ours, theirs)
        })
        .collect();
    let run = |cast: &mut CastStage<Stage>, renderer: &mut Recorder, frames: &[(FrameInput, FrameInput)]| {
        for (ours, theirs) in frames {
            cast.tick_members(renderer, &[(lead.clone(), ours), (guest.clone(), theirs)])?;
        }
        Ok::<(), StageError>(())
    };
    run(&mut cast, &mut renderer, &frames[..30]).unwrap();
    renderer.updates.clear();
    // The ids are cloned per frame above; this loop names members as a live loop does, through the stored inputs.
    let (result, count) = allocations_in(|| {
        for (ours, theirs) in &frames[30..] {
            cast.set_member_input(&guest, theirs);
            FrameLoop::tick(&mut cast, &mut renderer, ours)?;
        }
        Ok::<(), StageError>(())
    });
    result.unwrap();
    assert_eq!(count, 0, "the two-member frame allocated {count} times in 60 frames");
    FrameLoop::release(cast, &mut renderer).unwrap();
}

#[test]
fn the_live_loop_runs_a_cast() {
    let mut renderer = Recorder::new(false);
    let mut alone = stage(&mut renderer, built());
    alone.tick(&mut renderer, &input(0.0)).unwrap();
    let n = alone.scene().instances.len();
    let cast = pair(&mut renderer);
    let mut base = HumanoidPose::REST;
    seed_base_pose(&built().rig, &mut base);
    let sources = Arc::new(SourceSlot::new());
    let mut live = LiveStage::new(performer(&base), cast, sources, Quality::live(), budget::LIVE_GPU).unwrap();
    for step in 0..10 {
        live.frame(&mut renderer, mid_step(step)).unwrap();
    }
    let cast = live.into_stage();
    assert_eq!(cast.scene().instances.len(), 2 * n, "the loop rendered both members");
    FrameLoop::release(cast, &mut renderer).unwrap();
    alone.release(&mut renderer).unwrap();
}

// ------------------------------------------------------------------------------------------------ evidence

/// The kit's default character from `template`, made by `character.create` as a person would.
fn kit(template: &str) -> BuiltCharacter {
    use omuzaki_char::builder::CachingBuilder;
    use omuzaki_char_api::{BuildOptions, BuildServices, CharId, CharacterBuilder, CharacterDef};
    use omuzaki_core::time::FakeClock;
    use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
    use omuzaki_doc_mock::MemDoc;
    use omuzaki_ops_api::{Actor, OpCtx, find};

    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(serde_json::json!({"id": "kit", "name": "Kit", "template": template, "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).expect("character.create plans");
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("the character is stored").clone();
    let def = CharacterDef::from_node(&node).expect("the stored character reads back");
    CachingBuilder::new()
        .build(&CharId::new("kit").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

/// Renders the kit's two default characters as one cast into `OMUZAKI_CAST_DIR`, each with its own face input, for
/// the spine's evidence: on lavapipe with the `wgpu` feature, through the render mock without it.
#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn render_the_cast_when_asked() {
    use omuzaki_stage::frame::face::{FaceRig, InlineExecutor};
    use omuzaki_stage::ops::render::still::encode_png;
    use omuzaki_stage::recipe::LightRig;

    let Some(directory) = std::env::var_os("OMUZAKI_CAST_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_CAST_DIR to write the pictures");
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    #[cfg(feature = "wgpu")]
    let mut renderer = {
        use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
            .expect("a software adapter (install mesa-vulkan-drivers)")
    };
    #[cfg(not(feature = "wgpu"))]
    let mut renderer = MockRenderer::new();

    let (standard, soft) = (kit("standard"), kit("soft-sd"));
    let top =
        standard.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter()).fold(0.0_f32, |top, p| top.max(p.y));
    let (fov_y_deg, size) = (30.0_f32, UVec2::new(1280, 720));
    let centre = Vec3::new(0.0, 0.5 * top, 0.0);
    let distance = 0.6 * top / (0.5 * fov_y_deg.to_radians()).tan();
    let mut view = Scene::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, distance),
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
    ));
    let rig = LightRig::load("viewer-studio").unwrap();
    view.lights = rig.to_lights().unwrap();
    view.background = rig.background();
    view.post = rig.post(false);

    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
    let options = UploadOptions { face_px_per_m: 2000.0 };
    let face = || FaceRig::standard().with_executor(Box::new(InlineExecutor));
    let mut base = [HumanoidPose::REST; 2];
    seed_base_pose(&standard.rig, &mut base[0]);
    seed_base_pose(&soft.rig, &mut base[1]);
    let mut lead =
        Stage::new(&mut renderer, standard, options, view, target, Quality::high(), StageClock::live()).unwrap();
    lead.set_face(&mut renderer, face()).unwrap();
    lead.set_subject_root(Mat4::from_translation(Vec3::new(-0.4, 0.0, 0.0)) * Mat4::from_rotation_y(0.35));
    // The member's target and camera are never used: the cast renders into the lead's.
    let spare = renderer.create_target(&TargetDesc { size: UVec2::splat(8), alpha: AlphaEncoding::Straight }).unwrap();
    let mut guest =
        Stage::new(&mut renderer, soft, options, scene(), spare, Quality::high(), StageClock::live()).unwrap();
    guest.set_face(&mut renderer, face()).unwrap();
    let mut cast = CastStage::new(id("lead"), lead);
    let root = Mat4::from_translation(Vec3::new(0.4, 0.0, 0.0)) * Mat4::from_rotation_y(-0.35);
    assert!(cast.add_member(id("guest"), guest, root).is_none());

    // The lead smiles with its eyes shut; the guest opens its mouth: each face reads its own input.
    let frames = 60;
    for i in 0..frames {
        let t = f64::from(i) / 60.0;
        let mut ours = FrameInput { pose: base[0], dt: 1.0 / 60.0, time: Some(Seconds(t)), ..FrameInput::default() };
        let mut theirs = FrameInput { pose: base[1], ..ours.clone() };
        for channel in
            [Channel::MOUTH_SMILE_LEFT, Channel::MOUTH_SMILE_RIGHT, Channel::EYE_BLINK_LEFT, Channel::EYE_BLINK_RIGHT]
        {
            ours.channels[channel] = 1.0;
        }
        theirs.channels[Channel::JAW_OPEN] = 0.7;
        cast.tick_members(&mut renderer, &[(id("lead"), &ours), (id("guest"), &theirs)]).unwrap();
    }
    let image = renderer.read_target(target).unwrap();
    std::fs::write(directory.join("cast-two-members.png"), encode_png(&image).unwrap()).unwrap();
    FrameLoop::release(cast, &mut renderer).unwrap();
    renderer.release(spare.into()).unwrap();
}

#[test]
fn the_scenes_skin_floor_reaches_every_member() {
    use omuzaki_stage::scene::resolve::ResolvedScene;
    use omuzaki_stage_api::GroundKind;

    let mut renderer = MockRenderer::new();
    let mut cast = pair(&mut renderer);
    let floor_of = |cast: &CastStage<Stage>, who: &str| cast.member(&id(who)).unwrap().character().skin_floor();
    let mut resolved = ResolvedScene::new();
    resolved.ground.kind = GroundKind::None;
    resolved.skin_floor = Some(0.3);
    let owed = FrameLoop::set_scene(&mut cast, &resolved, left());
    assert_eq!(owed.skin_floor, Some(Some(0.3)), "the floor is owed");
    FrameLoop::pay_scene(&mut cast, &mut renderer).unwrap();
    assert!(FrameLoop::owed(&cast).is_empty(), "the lead and the guest were both paid");
    assert_eq!((floor_of(&cast, "lead"), floor_of(&cast, "guest")), (Some(0.3), Some(0.3)));

    // A member added later owes the floor until the next pay.
    let late = stage(&mut renderer, built());
    assert!(cast.add_member(id("late"), late, Mat4::IDENTITY).is_none());
    assert_eq!(FrameLoop::owed(&cast).skin_floor, Some(Some(0.3)));
    FrameLoop::pay_scene(&mut cast, &mut renderer).unwrap();
    assert_eq!(floor_of(&cast, "late"), Some(0.3));
    assert!(FrameLoop::owed(&cast).is_empty());

    // Clearing the floor gives every member its own back.
    resolved.skin_floor = None;
    FrameLoop::set_scene(&mut cast, &resolved, left());
    FrameLoop::pay_scene(&mut cast, &mut renderer).unwrap();
    assert!(cast.ids().all(|who| cast.member(who).unwrap().character().skin_floor().is_none()));
    FrameLoop::release(cast, &mut renderer).unwrap();
}

#[test]
fn a_two_member_frame_fits_the_60_fps_budget() {
    let mut renderer = Recorder::new(false);
    let mut alone = stage(&mut renderer, built());
    let mut cast = pair(&mut renderer);
    let frames: Vec<FrameInput> = (0..1320).map(|i| input(f64::from(i) / 60.0)).collect();
    let time = |run: &mut dyn FnMut(&FrameInput)| {
        for frame in &frames[..120] {
            run(frame);
        }
        let mut times: Vec<std::time::Duration> = frames[120..]
            .iter()
            .map(|frame| {
                let start = std::time::Instant::now();
                run(frame);
                start.elapsed()
            })
            .collect();
        times.sort_unstable();
        (times[times.len() / 2], times[times.len() * 99 / 100])
    };
    let one = time(&mut |frame| {
        alone.tick(&mut renderer, frame).unwrap();
    });
    let two = time(&mut |frame| {
        FrameLoop::tick(&mut cast, &mut renderer, frame).unwrap();
    });
    eprintln!(
        "one member: p50 {:?}, p99 {:?}; two members: p50 {:?}, p99 {:?} (stage only, no drawing)",
        one.0, one.1, two.0, two.1
    );
    // The frame budget is 1/60 s; the optimised build is what runs live, so only it is held to it.
    if !cfg!(debug_assertions) {
        assert!(two.1 < std::time::Duration::from_secs_f32(1.0 / 60.0), "p99 {:?} is over a 60 fps frame", two.1);
    }
    FrameLoop::release(cast, &mut renderer).unwrap();
    alone.release(&mut renderer).unwrap();
}
