//! Real-time behaviour of dynamic parts (hair, cloth-lite, dangling accessories): the contract between a part and the
//! frame loop (spec §8: `... -> IK -> physics (spring bones, hair, cloth) -> face paint -> render`).
//!
//! The frame loop owns time and the skeleton pose; a [`PartRuntime`] owns its simulation state and the geometry it
//! rewrites. Nothing here allocates per step: the runtime holds its buffers, and the renderer reads them through
//! [`DynamicMesh`] views.

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_core::time::Seconds;

use crate::ids::JointRef;

/// The current skeleton pose, as the frame loop evaluated it. Implemented by the rig/animation system
/// (`omuzaki-anim`); `omuzaki-char-mock` has a fixed one.
pub trait PoseSource: Send + Sync {
    /// The skinning matrix of `joint`: bind space to **world** space, i.e. the bone's world matrix times the inverse of
    /// its bind matrix (`omuzaki_anim_api::Skeleton::skin_matrices`). `None` if the skeleton has no such bone. Simulation runs in world space so that walking or
    /// turning the whole character swings the hair.
    fn skin_matrix(&self, joint: &JointRef) -> Option<Mat4>;

    /// The character root's world matrix. Runtimes convert their results back to character space with its inverse,
    /// because the renderer draws the character under that matrix.
    fn root(&self) -> Mat4;
}

/// What a runtime gets each frame.
///
/// `#[non_exhaustive]`: other crates build one with [`StepCtx::new`] and the `with_*` methods and read its fields (all
/// public), never a struct literal, so a field added here edits only this crate.
#[derive(Copy, Clone)]
#[non_exhaustive]
pub struct StepCtx<'a> {
    /// Frame time in seconds. The runtime sub-steps to its own fixed rate (spec §8: 120 Hz, seeded, deterministic).
    pub dt: f32,
    /// Time on the shot's clock.
    pub time: Seconds,
    /// The pose to follow.
    pub pose: &'a dyn PoseSource,
    /// Wind velocity in world space, metres per second.
    pub wind: Vec3,
    /// How many pixels one metre at the character spans on screen this frame (the viewport's height over the view's
    /// height at the character's distance); `Some(0.0)` when the character is off screen, `None` when the frame loop
    /// does not know. A runtime multiplies it by its part's size to choose a level of detail (the cloth's particle
    /// levels, #706); `None` keeps full detail.
    pub pixels_per_metre: Option<f32>,
}

impl<'a> StepCtx<'a> {
    /// A frame of `dt` seconds ending at `time`, following `pose` in `wind`, with no screen size
    /// ([`with_pixels_per_metre`](Self::with_pixels_per_metre)).
    pub fn new(dt: f32, time: Seconds, pose: &'a dyn PoseSource, wind: Vec3) -> Self {
        Self { dt, time, pose, wind, pixels_per_metre: None }
    }

    /// Sets the screen scale at the character ([`StepCtx::pixels_per_metre`]).
    #[must_use]
    pub fn with_pixels_per_metre(mut self, pixels_per_metre: Option<f32>) -> Self {
        self.pixels_per_metre = pixels_per_metre;
        self
    }
}

/// A read-only view of one mesh a runtime rewrites.
#[derive(Copy, Clone, Debug)]
pub struct DynamicMesh<'a> {
    /// Index into the part's own mesh list (`PartOutput::meshes`).
    pub mesh: u16,
    /// Current vertex positions, character space.
    pub positions: &'a [Vec3],
    /// Current vertex normals, character space.
    pub normals: &'a [Vec3],
    /// Bumps whenever the buffers change; the renderer re-uploads only when it differs from the last one it saw.
    pub revision: u64,
}

/// What a runtime costs, for the frame-budget scheduler and the report card (spec §10: physics at most 0.5 ms).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct RuntimeCost {
    /// Simulated particles.
    pub particles: u32,
    /// Collider tests per particle per iteration.
    pub colliders: u32,
    /// Vertices rewritten per step.
    pub vertices: u32,
}

/// The simulation of one dynamic part.
///
/// # Why `Sync`
///
/// A caching builder keeps one runtime per cached part as the template it [`fork`](Self::fork)s for every build, and
/// its build threads share that template (`fork` takes `&self`). Simulation state is plain data (owned buffers,
/// counters, a seeded generator held by value), which is `Sync` automatically, so the shared template needs no lock. A
/// runtime that holds `Cell`, `RefCell` or an `mpsc::Receiver` is not `Sync`: keep that state out of the runtime, or
/// guard it with a `Mutex`. (The alternative, requiring only `Send`, would put a lock around the template in every
/// cache; the lock-free cache is the reason for the stronger bound.) Nothing steps through a shared reference.
pub trait PartRuntime: Send + Sync {
    /// Advances the simulation by `ctx.dt` and refreshes the dynamic meshes. Real-time: no allocation, no locks.
    /// Deterministic for the same sequence of contexts.
    fn step(&mut self, ctx: &StepCtx<'_>);

    /// Snaps to the rest shape under `pose` and clears all velocity (a "settle" start, after a cut or a variant switch).
    fn reset(&mut self, pose: &dyn PoseSource);

    /// How many meshes this runtime rewrites.
    fn dynamic_count(&self) -> usize;

    /// The `index`th dynamic mesh (`index < dynamic_count()`); `None` past the end.
    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>>;

    /// The cost of one step.
    fn cost(&self) -> RuntimeCost;

    /// A new runtime in this simulation's **initial state**: the state its generator first returned, whenever `fork` is
    /// called, whether or not this runtime has stepped or been reset since. Never the current state. The fork is
    /// independent of this runtime: stepping or resetting either never changes the other. `None` (the default) means
    /// "not copyable": a builder that wants this part again has to run its generator again.
    ///
    /// This is what lets a caching builder keep a part with a simulation (hair, cloth, a dangling accessory) like any
    /// other: it keeps the generator's runtime as a template and forks it for every build. So the contract is:
    /// - **Initial state, bit for bit**: the same buffers, [`revision`](DynamicMesh::revision) and
    ///   [`cost`](Self::cost) as the runtime the generator returned, and the same results for the same sequence of
    ///   [`step`](Self::step)s. An implementation therefore keeps its initial state, or can rebuild it from what it
    ///   keeps (the rest shape and a zero velocity, say); it does not just copy its current buffers.
    /// - **Cheap**: copy buffers; do not rebuild meshes, redo generation or recompute what the generator computed
    ///   once. Nothing real-time calls this, so it may allocate, but it runs on every cached build.
    /// - **Consistent**: a runtime that returns `Some` once returns `Some` always, and one that returns `None` never
    ///   returns `Some`. A builder decides whether a part is cacheable from the first answer.
    ///
    /// # When to implement it
    ///
    /// Implement it whenever the runtime's state is owned data: buffers, counters, a generator held by value. The
    /// default is correct but expensive: the builder then runs the part's generator again on every edit and every
    /// palette change, which defeats the cache for exactly the heaviest parts (hair, cloth).
    ///
    /// `#[derive(Clone)]` is a good start but not enough on its own. It copies the *current* state, so reset the
    /// copy to the initial state (or clone from a pristine copy kept for the purpose). And it is wrong if any field
    /// is shared and mutable between clones: an `Arc<Mutex<..>>`, atomics, an `Arc`ed buffer written through
    /// interior mutability, a handle to a shared generator. The fork and the original would then move together, so
    /// stepping one character would shake every other character built from the same cache. Copy such fields
    /// deeply, or leave `fork` at its default. (Sharing a buffer that is only ever read, such as an `Arc<[Vec3]>`
    /// rest shape, is fine.)
    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        None
    }

    /// Bytes of memory this runtime holds, for a cache's byte budget (it counts the template it keeps, not the forks
    /// characters step). The default counts the positions and normals of the [dynamic meshes](Self::dynamic_mesh), a
    /// floor: a runtime whose state is larger (rest shape, velocities, constraints, spatial grids) overrides it with
    /// its real size, or the budget is exceeded without anyone noticing.
    fn heap_bytes(&self) -> usize {
        (0..self.dynamic_count())
            .filter_map(|i| self.dynamic_mesh(i))
            .map(|m| std::mem::size_of_val(m.positions) + std::mem::size_of_val(m.normals))
            .sum()
    }
}
