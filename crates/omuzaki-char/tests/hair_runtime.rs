//! The hair runtime (#322) on the kit's default hair over the default body: the simulated groups become an unskinned,
//! dynamic mesh the runtime rewrites; at rest the hair hangs near its styled shape, a turn swings it and it settles
//! again, the result is the same for the same steps, a fork starts from the initial state, and a step allocates
//! nothing.
//!
//! The allocation test counts only the measuring thread's allocations, as `omuzaki-rig/tests/hot_loop.rs` does.

// A counting allocator is the only way to prove "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, JointRef, PartDef, PartId,
    PartKindId, PartRuntime, PoseSource, RegistryBuilder, StepCtx,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;
use omuzaki_face_mock::MockRasterizer;
use serde_json::json;

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const FRAME: f32 = 1.0 / 60.0;
/// How long the body takes to turn 90°, seconds.
const TURN_S: f32 = 0.4;

/// Every bone turned by `yaw` about the vertical through the origin (the character turning on the spot).
struct Turned {
    yaw: f32,
}

impl PoseSource for Turned {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(Mat4::from_rotation_y(self.yaw))
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

fn turn(t: f32) -> Turned {
    let u = (t / TURN_S).clamp(0.0, 1.0);
    Turned { yaw: std::f32::consts::FRAC_PI_2 * u * u * (3.0 - 2.0 * u) }
}

fn built(template: &str) -> BuiltCharacter {
    assert!(!omuzaki_char::template::registry::templates().is_empty());
    let mut def: CharacterDef = mock_character().unwrap();
    def.parts.clear();
    def.template = template.parse().unwrap();
    for (layer, kind) in ["body", "head", "hair"].into_iter().enumerate() {
        let part = PartDef {
            kind: PartKindId::new(kind).unwrap(),
            parent: None,
            enabled: true,
            layer: i32::try_from(layer).unwrap() * 10,
            attach: None,
            params: json!({}),
        };
        def.parts.insert(PartId::new(kind).unwrap(), part);
    }
    let services = BuildServices::default().with_rasterizer(|| Box::new(MockRasterizer));
    RegistryBuilder.build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &services).unwrap()
}

fn hair(built: &mut BuiltCharacter) -> &mut Box<dyn PartRuntime> {
    &mut built.runtimes.iter_mut().find(|s| s.part.as_str() == "hair").expect("the hair has a runtime").runtime
}

fn positions(rt: &dyn PartRuntime) -> Vec<Vec3> {
    rt.dynamic_mesh(0).unwrap().positions.to_vec()
}

fn step(rt: &mut dyn PartRuntime, pose: &dyn PoseSource, frame: u32) {
    rt.step(&StepCtx::new(FRAME, Seconds(f64::from(frame) * f64::from(FRAME)), pose, Vec3::ZERO));
}

fn max_gap(a: &[Vec3], b: &[Vec3]) -> f32 {
    a.iter().zip(b).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max)
}

#[test]
fn the_simulated_hair_is_one_unskinned_dynamic_mesh_with_its_runtime() {
    let built = built("soft-sd");
    let rt = &built.runtimes.iter().find(|s| s.part.as_str() == "hair").expect("the hair has a runtime").runtime;
    assert_eq!(rt.dynamic_count(), 1);
    let view = rt.dynamic_mesh(0).unwrap();
    let cost = rt.cost();
    println!("hair runtime: {cost:?}");
    assert!(cost.particles > 50 && cost.colliders >= 6, "{cost:?}");
    let mesh = built.meshes.iter().find(|m| m.part.as_str() == "hair" && m.local == view.mesh).unwrap();
    assert_eq!(mesh.mesh.name, "hair-dynamic");
    assert!(mesh.mesh.dynamic && mesh.mesh.joint_bones.is_empty() && mesh.mesh.geometry.joints.is_empty());
    assert_eq!(view.positions, mesh.mesh.geometry.positions, "it starts at the built shape");
}

#[test]
fn at_rest_the_hair_hangs_near_its_styled_shape() {
    for template in ["soft-sd", "standard"] {
        let mut built = built(template);
        let rt = hair(&mut built);
        let rest = positions(rt.as_ref());
        let still = Turned { yaw: 0.0 };
        for frame in 0..180 {
            step(rt.as_mut(), &still, frame);
        }
        let now = positions(rt.as_ref());
        assert!(now.iter().all(|p| p.is_finite()));
        let gap = max_gap(&rest, &now);
        println!("{template}: after 3 s at rest the hair is at most {:.1} mm from its styled shape", gap * 1e3);
        assert!(gap > 1e-4, "{template}: gravity moves it a little");
        assert!(gap < 0.04, "{template}: it hangs near its styled shape: {gap}");
    }
}

#[test]
fn a_turn_swings_the_hair_and_it_settles_turned() {
    let mut built = built("soft-sd");
    let rt = hair(&mut built);
    let still = Turned { yaw: 0.0 };
    for frame in 0..180 {
        step(rt.as_mut(), &still, frame);
    }
    let settled = positions(rt.as_ref());
    let turned = Mat4::from_rotation_y(std::f32::consts::FRAC_PI_2);
    let rigid: Vec<Vec3> = settled.iter().map(|p| turned.transform_point3(*p)).collect();
    let mut lag = 0.0f32;
    for frame in 0..240 {
        step(rt.as_mut(), &turn(frame as f32 * FRAME), frame);
        let now = positions(rt.as_ref());
        assert!(now.iter().all(|p| p.is_finite()), "frame {frame}");
        if frame as f32 * FRAME > TURN_S {
            lag = lag.max(max_gap(&now, &rigid));
        }
    }
    let after = max_gap(&positions(rt.as_ref()), &rigid);
    println!("turn: the hair swings up to {:.1} mm off the turned shape and ends {:.1} mm off", lag * 1e3, after * 1e3);
    assert!(lag > 0.01, "the hair lags the turn: {lag}");
    assert!(after < 0.01, "and settles on the turned shape: {after}");
}

#[test]
fn the_same_steps_give_the_same_hair_and_a_fork_starts_over() {
    let mut a = built("soft-sd");
    let start = positions(hair(&mut a).as_ref());
    let fork = hair(&mut a).fork().expect("the hair runtime forks");
    let mut b = built("soft-sd");
    for frame in 0..60 {
        step(hair(&mut a).as_mut(), &turn(frame as f32 * FRAME), frame);
        step(hair(&mut b).as_mut(), &turn(frame as f32 * FRAME), frame);
    }
    assert_eq!(positions(hair(&mut a).as_ref()), positions(hair(&mut b).as_ref()));
    let mut fork_after = hair(&mut a).fork().unwrap();
    assert_eq!(positions(fork.as_ref()), start, "a fork is the initial state");
    assert_eq!(fork_after.dynamic_mesh(0).unwrap().revision, 0);
    assert_eq!(positions(fork_after.as_ref()), start, "whenever it is taken");
    for frame in 0..60 {
        step(fork_after.as_mut(), &turn(frame as f32 * FRAME), frame);
    }
    assert_eq!(positions(fork_after.as_ref()), positions(hair(&mut b).as_ref()));
}

#[test]
fn a_step_allocates_nothing() {
    let mut built = built("soft-sd");
    let rt = hair(&mut built);
    step(rt.as_mut(), &turn(0.0), 0);
    let pose = turn(0.2);
    MEASURING.with(|m| m.set(true));
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for frame in 1..30 {
        step(rt.as_mut(), &pose, frame);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(false));
    assert_eq!(after - before, 0, "allocations in 29 steps");
}
