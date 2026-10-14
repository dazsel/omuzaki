//! Proves the contract's per-frame path allocates nothing: rebuilding the [`Scene`] and validating it, the two things
//! every frame does before a renderer starts (spec §8, "no allocation in the hot loop").
//!
//! It lives in its own test file, and the allocator counts only allocations made on the measuring thread: the test
//! harness's own threads allocate at unpredictable times, which made a process-wide count flaky under load.

// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::expect_used, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_render_api::{Camera, Colour, Instance, JointRange, Light, MaterialId, MeshId, Scene};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it's safe inside the allocator.
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

/// A busy frame: a skinned character (one 128-joint skeleton) plus 200 props and a full light rig.
fn fill(scene: &mut Scene, frame: u64) {
    scene.frame.index = frame;
    let (mesh, material) = (MeshId::from_parts(0, 1), MaterialId::from_parts(0, 1));
    scene.lights.extend((0..8u8).map(|i| Light::Point {
        position: Vec3::splat(f32::from(i)),
        colour: Colour::WHITE,
        intensity: 1.0,
        range: 5.0,
    }));
    scene.joints.extend(
        (0..128u8).map(|j| {
            Mat4::from_translation(Vec3::new(f32::from(j), f32::from(u16::try_from(frame).unwrap_or(0)), 0.0))
        }),
    );
    for i in 0..200u8 {
        let mut instance = Instance::new(mesh, material, Mat4::from_translation(Vec3::splat(f32::from(i))));
        if i == 0 {
            instance.skin = Some(JointRange { start: 0, len: 128 });
        }
        scene.instances.push(instance);
    }
}

#[test]
fn rebuilding_and_validating_a_scene_allocates_nothing_after_warm_up() {
    let mut scene = Scene::new(Camera::default());
    fill(&mut scene, 0); // warm-up: the vectors grow to their working size
    scene.validate().unwrap();

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    for frame in 1..=100 {
        scene.clear();
        fill(&mut scene, frame);
        scene.validate().unwrap();
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert_eq!(allocations, 0, "100 frames of rebuild + validate made {allocations} allocations");
}
