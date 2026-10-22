//! `PartRuntime::fork`: the contract a caching builder relies on to serve a part with a simulation from its cache.
//!
//! A builder keeps the runtime the generator returned as a template that never steps, and forks it for every build. So
//! an unstepped fork must equal a fresh build bit for bit, a fork and its original must not influence each other, and
//! many builder threads must be able to fork one shared template.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use omuzaki_char_api::{DynamicMesh, JointRef, PartRuntime, PoseSource, RuntimeCost, StepCtx};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;

/// A skeleton that never moves: the runtimes here ignore the pose.
struct Still;

impl PoseSource for Still {
    fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
        Some(Mat4::IDENTITY)
    }

    fn root(&self) -> Mat4 {
        Mat4::IDENTITY
    }
}

/// A runtime that drifts with the wind and remembers velocity, like a real simulation keeps state beyond its buffers.
#[derive(Clone)]
struct Drift {
    rest: Vec<Vec3>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    velocity: Vec<Vec3>,
    revision: u64,
}

/// The expensive part of a generator: positions from a seeded sequence. A fork must not repeat it.
fn generate(seed: u32) -> Drift {
    let positions: Vec<Vec3> = (0..64)
        .map(|i| {
            #[allow(clippy::cast_precision_loss)] // 64 strands
            let t = i as f32 * 0.37 + seed as f32;
            Vec3::new(t.sin(), t.cos(), 0.1 * t)
        })
        .collect();
    Drift {
        rest: positions.clone(),
        normals: vec![Vec3::Z; positions.len()],
        velocity: vec![Vec3::ZERO; positions.len()],
        positions,
        revision: 0,
    }
}

impl PartRuntime for Drift {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        for ((p, v), rest) in self.positions.iter_mut().zip(&mut self.velocity).zip(&self.rest) {
            *v = (*v + ctx.wind * ctx.dt) * 0.98 + (*rest - *p) * ctx.dt;
            *p += *v * ctx.dt;
        }
        self.revision += 1;
    }

    fn reset(&mut self, _: &dyn PoseSource) {
        self.positions.copy_from_slice(&self.rest);
        self.velocity.fill(Vec3::ZERO);
        self.revision += 1;
    }

    fn dynamic_count(&self) -> usize {
        1
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        (index == 0).then_some(DynamicMesh {
            mesh: 0,
            positions: &self.positions,
            normals: &self.normals,
            revision: self.revision,
        })
    }

    fn cost(&self) -> RuntimeCost {
        RuntimeCost { particles: 64, colliders: 0, vertices: 64 }
    }

    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        // The initial state: the rest shape, no velocity, revision 0, whatever has happened to `self`.
        let mut initial = self.clone();
        initial.positions.copy_from_slice(&self.rest);
        initial.velocity.fill(Vec3::ZERO);
        initial.revision = 0;
        Some(Box::new(initial))
    }

    fn heap_bytes(&self) -> usize {
        // rest, positions, normals and velocity.
        4 * self.rest.len() * std::mem::size_of::<Vec3>()
    }
}

/// A runtime that says nothing about copying or its size: the trait's defaults.
struct Opaque(Drift);

impl PartRuntime for Opaque {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        self.0.step(ctx);
    }

    fn reset(&mut self, pose: &dyn PoseSource) {
        self.0.reset(pose);
    }

    fn dynamic_count(&self) -> usize {
        self.0.dynamic_count()
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        self.0.dynamic_mesh(index)
    }

    fn cost(&self) -> RuntimeCost {
        self.0.cost()
    }
}

fn step(runtime: &mut dyn PartRuntime, frame: u32, wind: Vec3) {
    let ctx = StepCtx::new(1.0 / 60.0, Seconds(f64::from(frame) / 60.0), &Still, wind);
    runtime.step(&ctx);
}

/// Everything observable about a runtime, as bits: equal bits are the same state, not merely close numbers.
fn state(runtime: &dyn PartRuntime) -> (Vec<u32>, Vec<u32>, u64, RuntimeCost) {
    let mesh = runtime.dynamic_mesh(0).expect("one dynamic mesh");
    let bits = |v: &[Vec3]| v.iter().flat_map(|p| p.to_array().map(f32::to_bits)).collect::<Vec<_>>();
    (bits(mesh.positions), bits(mesh.normals), mesh.revision, runtime.cost())
}

#[test]
fn a_runtime_is_not_copyable_unless_it_says_so() {
    let runtime: Box<dyn PartRuntime> = Box::new(Opaque(generate(1)));
    assert!(runtime.fork().is_none(), "the default: the builder runs the generator again");
    let runtime: Box<dyn PartRuntime> = Box::new(generate(1));
    assert!(runtime.fork().is_some());
}

#[test]
fn an_unstepped_fork_is_bit_identical_to_a_fresh_build() {
    for seed in [0, 1, 7] {
        let template: Box<dyn PartRuntime> = Box::new(generate(seed));
        let fresh = generate(seed);
        let fork = template.fork().expect("copyable");
        assert_eq!(state(fork.as_ref()), state(&fresh));
        assert_eq!(fork.dynamic_count(), fresh.dynamic_count());
    }
}

#[test]
fn a_fork_is_the_initial_state_however_far_the_original_has_run() {
    let mut original: Box<dyn PartRuntime> = Box::new(generate(6));
    let fresh = state(&generate(6));
    for frame in 0..20 {
        step(original.as_mut(), frame, Vec3::X);
    }
    assert_ne!(state(original.as_ref()), fresh, "the original moved");
    let mut fork = original.fork().expect("copyable");
    assert_eq!(state(fork.as_ref()), fresh, "forked after 20 frames: still the initial state, not the current one");
    original.reset(&Still);
    assert_eq!(state(original.fork().expect("copyable").as_ref()).2, 0, "forked after a reset: revision 0, not 21");
    step(fork.as_mut(), 0, Vec3::X);
    assert_eq!(state(fork.fork().expect("copyable").as_ref()), fresh, "and a fork of a stepped fork is initial too");
}

#[test]
fn a_runtime_reports_the_memory_it_holds() {
    let opaque = Opaque(generate(1));
    let exposed = 2 * 64 * std::mem::size_of::<Vec3>();
    assert_eq!(opaque.heap_bytes(), exposed, "the default counts the exposed positions and normals");
    let drift = generate(1);
    assert_eq!(drift.heap_bytes(), 2 * exposed, "an override adds the hidden rest shape and velocities");
}

#[test]
fn forks_of_one_template_are_all_the_same_and_the_template_never_changes() {
    let template: Box<dyn PartRuntime> = Box::new(generate(3));
    let before = state(template.as_ref());
    let mut forks: Vec<_> = (0..3).map(|_| template.fork().expect("copyable")).collect();
    for (i, fork) in forks.iter_mut().enumerate() {
        assert_eq!(state(fork.as_ref()), before);
        // Each fork lives its own life.
        for frame in 0..=u32::try_from(i).unwrap() {
            step(fork.as_mut(), frame, Vec3::X);
        }
    }
    assert_eq!(state(template.as_ref()), before, "stepping forks never touches the template");
    assert_eq!(state(template.fork().expect("copyable").as_ref()), before, "a later fork is as fresh as the first");
    let revisions: Vec<_> = forks.iter().map(|f| state(f.as_ref()).2).collect();
    assert_eq!(revisions, [1, 2, 3], "the forks stepped independently");
}

#[test]
fn a_fork_steps_exactly_like_the_original_would_have() {
    let mut original: Box<dyn PartRuntime> = Box::new(generate(5));
    let mut fork = original.fork().expect("copyable");
    for frame in 0..30 {
        let wind = [Vec3::X, Vec3::new(0.0, 0.5, 0.2), Vec3::ZERO][frame as usize % 3];
        step(original.as_mut(), frame, wind);
        step(fork.as_mut(), frame, wind);
    }
    assert_eq!(state(original.as_ref()), state(fork.as_ref()), "same inputs, same bits, whichever was forked");
    assert_ne!(state(original.as_ref()).0, state(&generate(5)).0, "the steps did move it");
}

#[test]
fn resetting_one_leaves_the_other_alone() {
    let mut original: Box<dyn PartRuntime> = Box::new(generate(2));
    let mut fork = original.fork().expect("copyable");
    step(original.as_mut(), 0, Vec3::Y);
    step(fork.as_mut(), 0, Vec3::Y);
    let (stepped_original, stepped_fork) = (state(original.as_ref()), state(fork.as_ref()));
    original.reset(&Still);
    assert_ne!(state(original.as_ref()).0, stepped_original.0);
    assert_eq!(state(fork.as_ref()), stepped_fork, "the fork didn't see the original's reset");
}

#[test]
fn a_shared_template_can_be_forked_from_many_threads() {
    // The caching builder's cache is shared by the threads that build, so this needs `PartRuntime: Sync`.
    let template: Arc<Box<dyn PartRuntime>> = Arc::new(Box::new(generate(9)));
    let expected = state(&generate(9));
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let template = Arc::clone(&template);
                scope.spawn(move || {
                    (0..16).map(|_| state(template.fork().expect("copyable").as_ref())).collect::<Vec<_>>()
                })
            })
            .collect();
        for handle in handles {
            for forked in handle.join().expect("no thread panics") {
                assert_eq!(forked, expected);
            }
        }
    });
}

#[test]
fn forking_is_consistent() {
    let runtime: Box<dyn PartRuntime> = Box::new(generate(4));
    assert!((0..5).all(|_| runtime.fork().is_some()), "a copyable runtime always copies");
    let runtime: Box<dyn PartRuntime> = Box::new(Opaque(generate(4)));
    assert!((0..5).all(|_| runtime.fork().is_none()), "an opaque one never does");
}
