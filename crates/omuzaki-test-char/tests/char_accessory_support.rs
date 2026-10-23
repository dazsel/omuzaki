//! Helpers shared by the accessory tests of `omuzaki-test-char`: a character with one accessory, the prototype dump when it is
//! there, and the spring solver run over a built rig.
//!
//! Each test file includes this with `mod char_accessory_support;`, so a helper one file does not use is dead there. (The file
//! is itself a test target with no tests.)

#![allow(dead_code)]
#![allow(clippy::missing_panics_doc, clippy::missing_errors_doc, clippy::must_use_candidate)]
// test helpers; a panic is the failure
// A counting allocator is the only way to *prove* "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // helpers build fixtures; a bad one should stop the test with its reason
#![allow(clippy::cast_possible_truncation, clippy::cast_precision_loss)] // frame counts and metres

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_anim_api::{BoneId, HumanoidBone, Pose, SpringSolver, Tick};
use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, PartDef, PartId, PartKindId, RegistryBuilder,
};
use omuzaki_char_mock::mock_character;
use omuzaki_core::glam::{Quat, Vec3};
use omuzaki_rig::spring::VrmSpringSolver;
use omuzaki_test::jsref::{GEOMETRY_SETS, JsMesh, JsRef};
use serde_json::{Value, json};

/// The physics rate the simulations are stepped at, frames per second.
pub const FPS: f32 = 120.0;
/// One second of frames at [`FPS`].
pub const SECOND: usize = 120;

/// A real body and head on `template` and one accessory `kind` as part `acc`.
pub fn built(template: &str, kind: &str, params: Value) -> BuiltCharacter {
    // The part kinds register when the crate is linked in.
    omuzaki_char::link();
    let mut def = mock_character().unwrap();
    def.template = template.parse().unwrap();
    def.parts.clear();
    let part = |kind: &str, layer: i32, params: Value| PartDef {
        kind: PartKindId::new(kind).unwrap(),
        parent: None,
        enabled: true,
        layer,
        attach: None,
        params,
    };
    def.parts.insert(PartId::new("body").unwrap(), part("body", 0, json!({})));
    def.parts.insert(PartId::new("head").unwrap(), part("head", 0, json!({})));
    def.parts.insert(PartId::new("acc").unwrap(), part(kind, 50, params));
    RegistryBuilder
        .build(&CharId::new("t").unwrap(), &def, &BuildOptions::default(), &BuildServices::default())
        .unwrap_or_else(|e| panic!("{kind} on {template}: {e}"))
}

/// The prototype's meshes whose name contains one of `keywords`, from every geometry set the dump holds; `None` (after saying
/// why) without the dump or when the dump has no such mesh, so the caller skips. The prototype's sample character wears no
/// accessory, so today the dump holds none of these and the parity tests skip; they compare as soon as a dump with one exists.
pub fn reference_meshes(kind: &str, keywords: &[&str]) -> Option<Vec<JsMesh>> {
    let js = JsRef::open()?;
    let found: Vec<JsMesh> = GEOMETRY_SETS
        .iter()
        .filter_map(|set| js.geometry(set).ok())
        .flat_map(|dump| dump.meshes)
        .filter(|m| keywords.iter().any(|k| m.name.contains(k) || m.part.contains(k)))
        .collect();
    if found.is_empty() {
        eprintln!("skipped: the prototype dump has no {kind} mesh (looked for {keywords:?}): nothing to compare");
        return None;
    }
    Some(found)
}

/// The size of a box: its extents.
pub fn extents((lo, hi): (Vec3, Vec3)) -> Vec3 {
    hi - lo
}

/// The bounding box of the accessory's meshes in `b`.
pub fn bounds(b: &BuiltCharacter) -> (Vec3, Vec3) {
    b.meshes
        .iter()
        .filter(|m| m.part == PartId::new("acc").unwrap())
        .flat_map(|m| m.mesh.geometry.positions.iter().copied())
        .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), p| (l.min(p), h.max(p)))
}

/// Prints how the accessory's size compares with each reference mesh's, and fails only on a size that is wrong by a factor
/// of three or more (the wrong kind, or the wrong units): the prototype's own style numbers are the parity target, and a
/// parameter set that matches them is a matter of choosing the parameters, not of this test.
pub fn compare_extents(kind: &str, ours: &BuiltCharacter, reference: &[JsMesh]) {
    let mine = extents(bounds(ours)).max_element();
    for theirs in reference {
        let other = extents(theirs.bbox).max_element();
        println!("{kind} parity: {} is {other} m at its longest, ours {mine} m", theirs.name);
        let ratio = (mine / other.max(1e-6)).max(other / mine.max(1e-6));
        assert!(ratio < 3.0, "{kind}: our longest extent {mine} m is {ratio}x the prototype's {other} m");
    }
}

/// The spring solver over a built character, with its pose.
pub struct Sim {
    pub b: BuiltCharacter,
    pub solver: VrmSpringSolver,
    pub pose: Pose,
    pub frame: u64,
}

impl Sim {
    pub fn new(b: BuiltCharacter) -> Self {
        let solver = VrmSpringSolver::new(&b.rig.springs, &b.rig.skeleton).unwrap();
        let pose = b.rig.skeleton.rest_pose();
        Self { b, solver, pose, frame: 0 }
    }

    pub fn bone(&self, name: &str) -> BoneId {
        self.b.rig.skeleton.find(name).unwrap_or_else(|| panic!("no bone {name}"))
    }

    /// One physics frame.
    pub fn step(&mut self) {
        self.solver.step(Tick::at_rate(self.frame, FPS), &self.b.rig.skeleton, &mut self.pose);
        self.frame += 1;
    }

    /// The world position of `bone` in the current pose.
    pub fn world(&self, bone: BoneId) -> Vec3 {
        let mut world = Pose::IDENTITY;
        self.b.rig.skeleton.world_pose(&self.pose, &mut world);
        world.local[bone.index()].translation
    }

    /// The world rotation of `bone` in the current pose.
    pub fn world_rotation(&self, bone: BoneId) -> Quat {
        let mut world = Pose::IDENTITY;
        self.b.rig.skeleton.world_pose(&self.pose, &mut world);
        world.local[bone.index()].rotation
    }

    /// Turns the hips about the vertical to `degrees` from their rest.
    pub fn turn_hips(&mut self, degrees: f32) {
        let hips = self.b.rig.skeleton.humanoid_bone(HumanoidBone::Hips).unwrap().index();
        self.pose.local[hips].rotation = Quat::from_rotation_y(degrees.to_radians());
    }
}

// ------------------------------------------------------------------------------------------------ allocation

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    // `const` init with no destructor: reading it never allocates, so it is safe inside the allocator.
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

/// How many allocations `f` makes on this thread. The test harness's own threads allocate at unpredictable times, so only the
/// measuring thread counts (as in `omuzaki-rig/tests/hot_loop.rs`).
pub fn allocations_in(f: impl FnOnce()) -> usize {
    ALLOCATIONS.store(0, Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.load(Ordering::Relaxed)
}
