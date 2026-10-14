//! The frame loop on the mock character: a deterministic 600-frame run, dynamic meshes re-uploaded once per
//! revision, no allocation in steady state, and skinning against a CPU reference at eight stress poses.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
// The counting allocator is the one unsafe impl: it forwards to `System` unchanged (the other hot-loop tests do the same).
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

#[path = "frame/double.rs"]
mod double;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::BTreeMap;

use omuzaki_anim_api::{HumanoidBone, HumanoidPose, IkGoals, Pose};
use omuzaki_char_api::{DynamicMesh, PartRuntime, PoseSource, RuntimeCost, StepCtx};
use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{DMat4, DVec3, Mat4, Quat, UVec2, Vec3};
use omuzaki_render_api::{AlphaEncoding, Background, Camera, Light, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_rig::pose::HumanoidRig;
use omuzaki_rig::skeleton::RigSkeleton;
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::pose::seed_base_pose;
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use sha2::{Digest, Sha256};

use double::{Recorder, built};

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

const FPS: f32 = 60.0;
const FRAMES: u64 = 600;

fn scene() -> Scene {
    let mut scene = Scene::new(Camera::look_at(
        Vec3::new(0.0, 0.8, 0.95),
        Vec3::new(0.0, 0.78, 0.0),
        Vec3::Y,
        Projection::Perspective { fov_y_deg: 30.0, near: 0.05, far: 10.0 },
    ));
    scene.background = Background::Solid { colour: Colour::hex(0x0018_1220) };
    scene.lights =
        vec![Light::Hemisphere { sky: Colour::hex(0x00f3_e8ff), ground: Colour::hex(0x003a_2a24), intensity: 1.0 }];
    scene
}

/// A stage on `renderer` with the mock character, face texture density lowered to keep the setup quick.
fn stage_with(renderer: &mut Recorder, character: omuzaki_char_api::BuiltCharacter) -> Stage {
    let target = renderer
        .create_target(&TargetDesc { size: UVec2::new(48, 48), alpha: AlphaEncoding::Straight })
        .expect("a target");
    Stage::new(
        renderer,
        character,
        UploadOptions { face_px_per_m: 1200.0 },
        scene(),
        target,
        Quality::offline(),
        StageClock::fixed(FPS),
    )
    .expect("the stage starts")
}

/// The scripted input of frame `n`: a sway of the spine and head, a bob of the hips, a gust of wind.
fn script(base: &HumanoidPose, n: u64) -> FrameInput {
    let t = n as f32 / FPS;
    let mut pose = *base;
    pose.set(HumanoidBone::Spine, base.get(HumanoidBone::Spine) * Quat::from_rotation_z(0.2 * (t * 1.7).sin()));
    pose.set(HumanoidBone::Head, base.get(HumanoidBone::Head) * Quat::from_rotation_y(0.4 * (t * 0.9).sin()));
    pose.hips_offset = Vec3::new(0.02 * (t * 0.5).sin(), 0.01 * (t * 3.0).sin(), 0.0);
    FrameInput { pose, wind: Vec3::new(0.5 * (t * 0.3).sin(), 0.0, 0.2), ..FrameInput::default() }
}

fn digest(image: &omuzaki_render_api::Image, stage: &Stage) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(&image.rgba8);
    for matrix in &stage.scene().joints {
        for value in matrix.to_cols_array() {
            hash.update(value.to_le_bytes());
        }
    }
    hash.finalize().into()
}

/// Runs the scripted 600 frames and hashes every one.
fn run_600() -> Vec<[u8; 32]> {
    let mut renderer = Recorder::new(true);
    let mut stage = stage_with(&mut renderer, built());
    let target = stage.target();
    let mut base = HumanoidPose::REST;
    seed_base_pose(&stage.built().rig, &mut base);
    let mut hashes = Vec::new();
    for n in 0..FRAMES {
        stage.tick(&mut renderer, &script(&base, n)).expect("the frame renders");
        let image = renderer.read_target(target).expect("the target reads back");
        hashes.push(digest(&image, &stage));
    }
    stage.release(&mut renderer).expect("released");
    hashes
}

#[test]
fn a_600_frame_run_is_deterministic() {
    let (first, second) = (run_600(), run_600());
    assert_eq!(first.len(), 600);
    assert_eq!(first, second, "two runs of the same script give the same picture and joints on every frame");
    let distinct: std::collections::BTreeSet<_> = first.iter().collect();
    assert!(distinct.len() > 300, "the script moves the character: {} distinct frames", distinct.len());
}

/// Wraps a runtime so its published revision moves once every third step, whatever the simulation did.
struct EveryThird {
    inner: Box<dyn PartRuntime>,
    steps: u64,
}

impl PartRuntime for EveryThird {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        self.inner.step(ctx);
        self.steps += 1;
    }

    fn reset(&mut self, pose: &dyn PoseSource) {
        self.inner.reset(pose);
    }

    fn dynamic_count(&self) -> usize {
        self.inner.dynamic_count()
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        self.inner.dynamic_mesh(index).map(|view| DynamicMesh { revision: self.steps / 3, ..view })
    }

    fn cost(&self) -> RuntimeCost {
        self.inner.cost()
    }
}

#[test]
fn dynamic_meshes_are_uploaded_once_per_revision() {
    let mut character = built();
    for slot in &mut character.runtimes {
        let inner = std::mem::replace(&mut slot.runtime, Box::new(NoRuntime));
        slot.runtime = Box::new(EveryThird { inner, steps: 0 });
    }
    let meshes: usize = character.runtimes.iter().map(|s| s.runtime.dynamic_count()).sum();
    assert!(meshes > 0, "the mock character has a simulated mesh");

    let mut renderer = Recorder::new(true);
    let mut stage = stage_with(&mut renderer, character);
    let mut uploads = 0;
    for n in 0..FRAMES {
        let out = stage.tick(&mut renderer, &script(&HumanoidPose::REST, n)).expect("frame");
        let expected = if (n + 1) % 3 == 0 { u32::try_from(meshes).unwrap() } else { 0 };
        assert_eq!(out.uploads, expected, "frame {n}: only a revision change uploads");
        uploads += u64::from(out.uploads);
    }
    assert_eq!(uploads, 200 * meshes as u64);
    let mut per_mesh: BTreeMap<String, u64> = BTreeMap::new();
    for id in &renderer.updates {
        *per_mesh.entry(format!("{id:?}")).or_default() += 1;
    }
    assert_eq!(renderer.updates.len() as u64, uploads, "the renderer saw exactly the uploads the stage counted");
    assert!(per_mesh.values().all(|&n| n == 200), "{per_mesh:?}: once per revision for each mesh");
    stage.release(&mut renderer).expect("released");
}

/// Stands in while a runtime is moved out of its slot.
struct NoRuntime;

impl PartRuntime for NoRuntime {
    fn step(&mut self, _: &StepCtx<'_>) {}
    fn reset(&mut self, _: &dyn PoseSource) {}
    fn dynamic_count(&self) -> usize {
        0
    }
    fn dynamic_mesh(&self, _: usize) -> Option<DynamicMesh<'_>> {
        None
    }
    fn cost(&self) -> RuntimeCost {
        RuntimeCost::default()
    }
}

#[test]
fn a_frame_allocates_nothing_in_steady_state() {
    let mut renderer = Recorder::new(false);
    let mut stage = stage_with(&mut renderer, built());
    let mut input = script(&HumanoidPose::REST, 0);
    input.goals = IkGoals {
        look_at: Some(omuzaki_anim_api::LookAt { target: Vec3::new(0.2, 1.5, 1.0), weight: 1.0, eye_share: 0.3 }),
        ..IkGoals::NONE
    };
    for _ in 0..20 {
        stage.tick(&mut renderer, &input).expect("warm-up frame");
    }
    let ((), counted) = allocations_in(|| {
        let _probe = Box::new(16_u8);
    });
    assert!(counted > 0, "the counter sees allocations on this thread");

    let (result, allocations) = allocations_in(|| {
        for n in 0..100 {
            input.wind.x = n as f32 * 0.01;
            stage.tick(&mut renderer, &input)?;
        }
        Ok::<(), omuzaki_stage::StageError>(())
    });
    result.expect("frames");
    assert_eq!(allocations, 0, "100 steady-state frames allocated {allocations} times");
    assert!(!renderer.updates.is_empty(), "the frames did re-upload the swaying hair");
    stage.release(&mut renderer).expect("released");
}

/// A deterministic spread of extreme poses: every bone turned about its own axis by up to ~140 degrees.
fn stress_pose(seed: u32) -> HumanoidPose {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(12_345);
    let mut next = move || {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (state >> 8) as f32 / (1 << 24) as f32
    };
    let mut pose = HumanoidPose::REST;
    for bone in HumanoidBone::ALL {
        let axis = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5).try_normalize().unwrap_or(Vec3::Y);
        pose.set(bone, Quat::from_axis_angle(axis, (next() - 0.5) * 4.9));
    }
    pose.hips_offset = Vec3::new(next() - 0.5, next() - 0.5, next() - 0.5);
    pose
}

/// Linear blend skinning of `positions` in f64, with `joint_matrices` for the mesh's joint run.
fn blend(mesh: &omuzaki_render_api::MeshData, joint_matrices: &[DMat4]) -> Vec<DVec3> {
    let vertices = mesh.positions.iter().zip(&mesh.joints).zip(&mesh.weights);
    vertices
        .map(|((p, joints), weights)| {
            let p = p.as_dvec3();
            joints
                .iter()
                .zip(weights)
                .filter(|(_, w)| **w > 0.0)
                .map(|(j, w)| f64::from(*w) * joint_matrices[usize::from(*j)].transform_point3(p))
                .sum()
        })
        .collect()
}

#[test]
fn skinning_matches_a_cpu_reference_at_eight_stress_poses() {
    let mut renderer = Recorder::new(false);
    let character = built();
    // The reference rig: lower-level pieces than the stage's composed rig (no IK, no springs), and the character's own
    // inverse bind. Humanoid bones are not touched by the spring stage, so the two must agree.
    let rig = character.rig.clone();
    let skeleton = RigSkeleton::with_inverse_bind(rig.skeleton.clone(), rig.inverse_bind()).expect("matching bind");
    let humanoid = HumanoidRig::new(skeleton.clone());
    let mut stage = stage_with(&mut renderer, character);

    let mut worst = 0.0_f64;
    for seed in 0..8 {
        let pose = stress_pose(seed);
        let input = FrameInput { pose, ..FrameInput::default() };
        stage.tick(&mut renderer, &input).expect("frame");

        let mut local = Pose::IDENTITY;
        humanoid.apply_humanoid(&pose, &mut local);
        let mut world = Pose::IDENTITY;
        skeleton.skeleton().world_pose(&local, &mut world);
        let reference: Vec<DMat4> = world.local[..skeleton.len()]
            .iter()
            .zip(skeleton.inverse_bind())
            .map(|(w, ib)| w.to_mat4().as_dmat4() * ib.as_dmat4())
            .collect();

        let scene = stage.scene();
        let mut checked = 0;
        for instance in scene.instances.iter().filter(|i| i.skin.is_some()) {
            let built_mesh = stage
                .built()
                .meshes
                .iter()
                .find(|m| stage.character().mesh(&m.part, m.local).is_some_and(|u| u.mesh == instance.mesh))
                .expect("every instance is a built mesh");
            let run = instance.skin.expect("filtered");
            let got: Vec<DMat4> =
                scene.joints[run.start as usize..(run.start + run.len) as usize].iter().map(Mat4::as_dmat4).collect();
            let want: Vec<DMat4> = built_mesh
                .mesh
                .joint_bones
                .iter()
                .map(|joint| reference[rig.bone(joint).expect("bone").index()])
                .collect();
            let (a, b) = (blend(&built_mesh.mesh.geometry, &got), blend(&built_mesh.mesh.geometry, &want));
            for (a, b) in a.iter().zip(&b) {
                worst = worst.max((*a - *b).abs().max_element());
            }
            checked += a.len();
        }
        assert!(checked > 0, "pose {seed}: vertices were compared");
    }
    println!("worst skinned vertex error over 8 stress poses: {worst:e}");
    assert!(worst < 1e-5, "worst error {worst:e}");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_look_at_goal_turns_the_head() {
    let mut renderer = Recorder::new(false);
    let mut stage = stage_with(&mut renderer, built());
    let head = stage.built().rig.bone(&omuzaki_char_api::JointRef::HEAD).expect("a head").index();
    let facing = |stage: &Stage| stage.skin()[head].transform_vector3(Vec3::Z).normalize();

    stage.tick(&mut renderer, &FrameInput::default()).expect("frame");
    let straight = facing(&stage);
    let goals = IkGoals {
        look_at: Some(omuzaki_anim_api::LookAt { target: Vec3::new(1.0, 1.7, 0.3), weight: 1.0, eye_share: 0.0 }),
        ..IkGoals::NONE
    };
    stage.tick(&mut renderer, &FrameInput { goals, ..FrameInput::default() }).expect("frame");
    let turned = facing(&stage);
    let angle = straight.dot(turned).clamp(-1.0, 1.0).acos();
    assert!(angle > 0.2, "the head turned only {angle} rad toward a target at the character's side");
    assert!(turned.x > straight.x, "and it turned toward the target (+X): {straight:?} to {turned:?}");
    stage.release(&mut renderer).expect("released");
}

#[test]
fn a_hand_goal_holds_while_the_head_looks_away() {
    let mut renderer = Recorder::new(false);
    let mut stage = stage_with(&mut renderer, built());
    let rig = &stage.built().rig;
    let hand = rig
        .bone(&omuzaki_char_api::JointRef::Humanoid(omuzaki_anim_api::HumanoidBone::RightHand))
        .expect("a right hand");
    // The joint's bind-space origin: the translation of the bind transform, the inverse of its inverse-bind matrix.
    let bind_origin = rig.inverse_bind()[hand.index()].inverse().transform_point3(Vec3::ZERO);
    let hand_position = |stage: &Stage| stage.skin()[hand.index()].transform_point3(bind_origin);

    let reach =
        omuzaki_anim_api::Limb { target: Vec3::new(-0.15, 0.5, 0.12), pole: Vec3::new(-0.2, 0.5, -0.5), weight: 1.0 };
    let hand_only = IkGoals { right_hand: Some(reach), ..IkGoals::NONE };
    stage.tick(&mut renderer, &FrameInput { goals: hand_only, ..FrameInput::default() }).expect("frame");
    let reached = hand_position(&stage);
    let miss = reached.distance(reach.target);
    assert!(miss < 0.05, "the hand is {miss} m from its reachable target: the goal did not apply");

    let looking = IkGoals {
        look_at: Some(omuzaki_anim_api::LookAt { target: Vec3::new(1.0, 1.7, 0.3), weight: 1.0, eye_share: 0.0 }),
        ..hand_only
    };
    stage.tick(&mut renderer, &FrameInput { goals: looking, ..FrameInput::default() }).expect("frame");
    let moved = hand_position(&stage).distance(reached);
    println!("right hand moved {moved:e} m when the head looked away");
    assert!(moved < 0.001, "the hand moved {moved} m while only the head was asked to look away");
    stage.release(&mut renderer).expect("released");
}
