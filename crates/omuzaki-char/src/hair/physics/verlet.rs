//! The hair simulation core (CB-33): Verlet chains that spring back to the styled shape, pushed out of sphere colliders,
//! stepped at a fixed rate. No mesh: the hair `PartRuntime` turns particles into drawn points.
//!
//! [`ChainSet::from_clumps`] makes one chain per simulated [`HairClump`]: a particle at every [`SUBSAMPLE`]th rest point
//! and always at the last one. Each frame the caller sets [`ChainSet::targets`] from the pose (the styled shape carried
//! by the head, or for [`Follow::Body`] by a blend of the head and the chest) and calls [`ChainSet::step`] with the frame
//! time (clamped to [`MAX_DT`]); the set runs whole fixed steps of `1 / HairPhysics::fixed_hz` (at most [`MAX_STEPS`])
//! and carries the remainder to the next frame. The targets hold across a frame's steps. The first [`ChainSet::targets`]
//! after building snaps every particle to its (posed) target, as [`ChainSet::reset`] does. State is in world space;
//! [`ChainSet::to_character`] converts back with the root.
//!
//! One fixed step `h`, chain by chain in clump order, particles root to tip, in the prototype's order (`hair.js`, the
//! Director's reference on #182):
//! 1. The root particle is pinned to its target.
//! 2. Every other particle integrates: `v = (x - x_prev) * damping`, `x += v + (gravity * gravity_scale + wind *
//!    wind_scale) * h^2`, then springs toward its target by `1 - (1 - k)^(h * 90)`. `k` is `HairClump::stiffness` at
//!    the particle's place in the chain, `j / (n - 1)` for particle `j` of `n`.
//! 3. `iterations` passes, each a one-sided distance pass root to tip (only the child moves, onto its rest length from
//!    the parent, so the order matters), then a collider pass (every non-root particle against every collider in
//!    order, pushed out to `radius + collider_margin`). A collider can leave a segment slightly off its rest length.
//!    Overlapping colliders resolve in that fixed order, as in the prototype: a push out of a later collider can leave
//!    a particle slightly inside an earlier one.
//!
//! Not in the prototype: wind (beside gravity), [`ChainSet::reset`] also clearing the carried time (#68), and a particle
//! at a collider's exact centre leaving along its segment instead of staying inside.
//!
//! Nothing here allocates or locks after [`ChainSet::from_clumps`]: every buffer is sized there.

use omuzaki_char_api::{Follow, HairClump, HairPhysics, JointRef, PoseSource, SphereCollider};
use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_face_api::{ParamErrors, Validate, narrow};

/// Particles sit at every `SUBSAMPLE`th rest point (and always the last).
pub const SUBSAMPLE: usize = 2;
/// The most fixed steps one [`ChainSet::step`] runs; time beyond them stays carried.
pub const MAX_STEPS: u32 = 8;
/// The longest frame time [`ChainSet::step`] takes in, seconds: a longer hitch counts as this (the prototype's clamp).
pub const MAX_DT: f32 = 0.1;
/// Gravity, metres per second squared, before `HairPhysics::gravity_scale`.
pub const GRAVITY: f32 = 9.8;
/// The rate the stiffness numbers were tuned at: a spring factor `k` pulls by `k` per step at this rate.
pub const STIFFNESS_HZ: f32 = 90.0;
/// The bind-space height band over which a [`Follow::Body`] particle goes from following the chest (below) to the head
/// (above), as `smoothstep(y, lo, hi)`. The prototype's numbers, for its head centre at 0.778 m.
pub const BODY_BLEND: (f32, f32) = (0.56, 0.70);

/// Why a chain set could not be built or driven.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum VerletError {
    /// The physics parameters are out of range.
    #[error("hair physics: {0}")]
    Physics(#[from] ParamErrors),
    /// A simulated clump has fewer than two rest points, a non-finite one, two particles at the same place or too far
    /// apart for `f32`, or a stiffness outside `0..=1`.
    #[error(
        "clump {clump}: a simulated clump needs at least 2 finite rest points, particles apart by a finite non-zero \
         distance, and a stiffness in 0..=1"
    )]
    BadClump {
        /// The clump's index in the list.
        clump: usize,
    },
    /// A collider's centre or radius is not finite as `f32`, or the radius is not positive.
    #[error("collider {collider}: centre and radius must be finite as f32, radius above 0")]
    BadCollider {
        /// The collider's index in the list used.
        collider: usize,
    },
    /// A stiffness list of the wrong length or with a value outside `0..=1`.
    #[error("stiffness: give {expected} values, each in 0..=1")]
    BadStiffness {
        /// The particle count.
        expected: usize,
    },
    /// The pose has no skinning matrix for a joint the hair follows, or one with non-finite entries.
    #[error("the pose has no usable skinning matrix for {joint:?}")]
    MissingJoint {
        /// The joint.
        joint: JointRef,
    },
    /// A frame time that is negative or not finite, a wind that is not finite, or a root that is not invertible.
    #[error("{what} must be finite (a frame time not negative, a root invertible)")]
    BadInput {
        /// `"dt"`, `"wind"`, `"root"` or `"collider position"`.
        what: &'static str,
    },
}

/// One chain: a range of particles in the set's buffers.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Chain {
    start: usize,
    end: usize,
}

/// Every simulated clump's chain, with the buffers the step works in.
#[derive(Clone, Debug)]
pub struct ChainSet {
    chains: Vec<Chain>,
    /// Bind-space rest position per particle.
    rest: Vec<Vec3>,
    /// The rest point index each particle sits at, within its clump.
    rest_index: Vec<u32>,
    /// Rest distance to the previous particle (0 at a root).
    segment: Vec<f32>,
    /// `HairClump::stiffness` at each particle.
    stiffness: Vec<f32>,
    /// The spring factor per fixed step, `1 - (1 - k)^(h * STIFFNESS_HZ)`.
    spring: Vec<f32>,
    /// How much of the head each particle follows (1 for [`Follow::Head`]).
    head_weight: Vec<f32>,
    /// Whether any particle follows the chest at all (a body-following chain reaching below the blend's top).
    needs_chest: bool,
    pos: Vec<Vec3>,
    prev: Vec<Vec3>,
    target: Vec<Vec3>,
    /// The colliders as checked `f32`: bone, bind-space centre, radius plus margin.
    colliders: Vec<(JointRef, Vec3, f32)>,
    /// Collider centres (world) and radii plus margin, refreshed by [`ChainSet::targets`].
    spheres: Vec<(Vec3, f32)>,
    root_inverse: Mat4,
    wind: Vec3,
    accumulator: f32,
    /// Whether the next [`ChainSet::targets`] snaps the particles to the targets (only after building).
    snap: bool,
    h: f32,
    damping: f32,
    gravity_scale: f32,
    wind_scale: f32,
    iterations: u8,
}

/// three.js `MathUtils.smoothstep`: 0 at or below `lo`, 1 at or above `hi`.
fn smoothstep(x: f32, lo: f32, hi: f32) -> f32 {
    let t = ((x - lo) / (hi - lo)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

impl ChainSet {
    /// Builds a chain for every clump with `simulate` set, in clump order. Colliders are `physics.colliders`, or
    /// `rig_colliders` when that is empty (the rig's, which the caller converts from `BuildCtx::colliders`).
    ///
    /// # Errors
    /// [`VerletError`] when the parameters, a clump's rest points or a collider are invalid.
    #[allow(clippy::cast_precision_loss)] // point indices are small
    pub fn from_clumps(
        clumps: &[HairClump],
        physics: &HairPhysics,
        rig_colliders: &[SphereCollider],
    ) -> Result<Self, VerletError> {
        physics.validate()?;
        let source = if physics.colliders.is_empty() { rig_colliders } else { &physics.colliders };
        let margin = narrow(physics.collider_margin);
        let mut colliders = Vec::with_capacity(source.len());
        for (n, c) in source.iter().enumerate() {
            // Checked after narrowing: a finite `f64` beyond `f32`'s range would become infinite here.
            let centre = Vec3::new(narrow(c.centre[0]), narrow(c.centre[1]), narrow(c.centre[2]));
            let radius = narrow(c.radius) + margin;
            if !(centre.is_finite() && radius.is_finite() && c.radius > 0.0) {
                return Err(VerletError::BadCollider { collider: n });
            }
            colliders.push((c.bone.clone(), centre, radius));
        }
        let h = 1.0 / narrow(physics.fixed_hz);
        let mut set = Self {
            chains: Vec::new(),
            rest: Vec::new(),
            rest_index: Vec::new(),
            segment: Vec::new(),
            stiffness: Vec::new(),
            spring: Vec::new(),
            head_weight: Vec::new(),
            needs_chest: false,
            pos: Vec::new(),
            prev: Vec::new(),
            target: Vec::new(),
            spheres: vec![(Vec3::ZERO, 0.0); colliders.len()],
            colliders,
            root_inverse: Mat4::IDENTITY,
            wind: Vec3::ZERO,
            accumulator: 0.0,
            snap: true,
            h,
            damping: narrow(physics.damping),
            gravity_scale: narrow(physics.gravity_scale),
            wind_scale: narrow(physics.wind_scale),
            iterations: physics.iterations,
        };
        for (n, clump) in clumps.iter().enumerate().filter(|(_, c)| c.simulate) {
            let rest = &clump.rest;
            if rest.len() < 2 || !rest.iter().all(|p| p.is_finite()) {
                return Err(VerletError::BadClump { clump: n });
            }
            let last = rest.len() - 1;
            let start = set.rest.len();
            let indices = (0..last).step_by(SUBSAMPLE).chain(std::iter::once(last));
            for i in indices {
                let p = rest[i];
                let segment = if set.rest.len() == start { 0.0 } else { set.rest[set.rest.len() - 1].distance(p) };
                // A derived length can overflow (two far-apart finite points) or vanish (two equal ones); either
                // breaks the distance pass, so it is refused here.
                if set.rest.len() > start && !(segment.is_finite() && segment > 0.0) {
                    return Err(VerletError::BadClump { clump: n });
                }
                set.segment.push(segment);
                set.rest.push(p);
                set.rest_index.push(u32::try_from(i).map_err(|_| VerletError::BadClump { clump: n })?);
                set.head_weight.push(match clump.follow {
                    Follow::Head => 1.0,
                    Follow::Body => smoothstep(p.y, BODY_BLEND.0, BODY_BLEND.1),
                });
            }
            let count = set.rest.len() - start;
            for j in 0..count {
                let k = clump.stiffness.at(j as f32 / (count - 1) as f32);
                if !(0.0..=1.0).contains(&k) {
                    return Err(VerletError::BadClump { clump: n });
                }
                set.stiffness.push(k);
                set.spring.push(spring_factor(k, h));
            }
            set.chains.push(Chain { start, end: set.rest.len() });
        }
        set.needs_chest = set.head_weight.iter().any(|&w| w < 1.0);
        set.pos.clone_from(&set.rest);
        set.prev.clone_from(&set.rest);
        set.target.clone_from(&set.rest);
        Ok(set)
    }

    /// Replaces the per-particle stiffness (one value in `0..=1` per particle, in [`ChainSet::positions`] order), for a
    /// preset or a tuning tool that sets it directly instead of through `HairClump::stiffness`.
    ///
    /// # Errors
    /// [`VerletError::BadStiffness`] for a wrong count or a value outside `0..=1`; nothing changes then.
    pub fn set_stiffness(&mut self, k: &[f32]) -> Result<(), VerletError> {
        if k.len() != self.stiffness.len() || !k.iter().all(|k| (0.0..=1.0).contains(k)) {
            return Err(VerletError::BadStiffness { expected: self.stiffness.len() });
        }
        self.stiffness.copy_from_slice(k);
        for (spring, &k) in self.spring.iter_mut().zip(k) {
            *spring = spring_factor(k, self.h);
        }
        Ok(())
    }

    /// Recomputes every particle's target and every collider's world position from `pose`. Call once per frame
    /// before [`ChainSet::step`]; the targets hold for all the fixed steps that frame runs.
    ///
    /// # Errors
    /// [`VerletError::MissingJoint`] when the pose lacks the head, the chest (with a body-following chain) or a
    /// collider's bone, [`VerletError::BadInput`] for a root that is not finite and invertible. Nothing changes then.
    pub fn targets(&mut self, pose: &dyn PoseSource) -> Result<(), VerletError> {
        let head = skin(pose, &JointRef::HEAD)?;
        let chest = if self.needs_chest { skin(pose, &JointRef::CHEST)? } else { head };
        let root = pose.root();
        if !(root.is_finite() && root.determinant().abs() > f32::EPSILON) {
            return Err(VerletError::BadInput { what: "root" });
        }
        // Every collider is placed and checked before anything is written, so a failed call leaves the set as it was.
        for (bone, centre, _) in &self.colliders {
            if !skin(pose, bone)?.transform_point3(*centre).is_finite() {
                return Err(VerletError::BadInput { what: "collider position" });
            }
        }
        for (sphere, (bone, centre, radius)) in self.spheres.iter_mut().zip(&self.colliders) {
            *sphere = (skin(pose, bone)?.transform_point3(*centre), *radius);
        }
        self.root_inverse = root.inverse();
        for ((t, &p), &w) in self.target.iter_mut().zip(&self.rest).zip(&self.head_weight) {
            let on_head = head.transform_point3(p);
            *t = if w >= 1.0 { on_head } else { chest.transform_point3(p).lerp(on_head, w) };
        }
        if self.snap {
            self.snap_to_targets();
        }
        Ok(())
    }

    /// Sets the wind velocity (world space, metres per second); `HairPhysics::wind_scale` times it is applied as an
    /// acceleration on every free particle.
    ///
    /// # Errors
    /// [`VerletError::BadInput`] when it is not finite; the wind is then unchanged.
    pub fn set_wind(&mut self, wind: Vec3) -> Result<(), VerletError> {
        if !wind.is_finite() {
            return Err(VerletError::BadInput { what: "wind" });
        }
        self.wind = wind;
        Ok(())
    }

    /// Advances by a frame of `dt` seconds (at most [`MAX_DT`]): as many fixed steps as fit (at most [`MAX_STEPS`]), the
    /// remainder carried to the next call. Returns the steps run.
    ///
    /// # Errors
    /// [`VerletError::BadInput`] for a negative or non-finite `dt`; nothing moves.
    pub fn step(&mut self, dt: f32) -> Result<u32, VerletError> {
        if !(dt.is_finite() && dt >= 0.0) {
            return Err(VerletError::BadInput { what: "dt" });
        }
        self.accumulator += dt.min(MAX_DT);
        let mut steps = 0;
        while self.accumulator >= self.h && steps < MAX_STEPS {
            self.accumulator -= self.h;
            self.fixed_step();
            steps += 1;
        }
        Ok(steps)
    }

    /// Runs `steps` fixed steps toward the current targets without touching the frame accumulator, to settle the hair
    /// after a build or a reset (480 steps settle the slowest strand of the sample hair to a few micrometres).
    pub fn settle(&mut self, steps: u32) {
        for _ in 0..steps {
            self.fixed_step();
        }
    }

    /// Snaps every particle to its target in `pose` (the styled shape), clears velocity and the frame accumulator.
    ///
    /// # Errors
    /// As [`ChainSet::targets`]; on error nothing changes.
    pub fn reset(&mut self, pose: &dyn PoseSource) -> Result<(), VerletError> {
        self.targets(pose)?;
        self.snap_to_targets();
        self.accumulator = 0.0;
        Ok(())
    }

    fn snap_to_targets(&mut self) {
        self.pos.copy_from_slice(&self.target);
        self.prev.copy_from_slice(&self.target);
        self.snap = false;
    }

    fn fixed_step(&mut self) {
        let accel =
            (Vec3::new(0.0, -GRAVITY * self.gravity_scale, 0.0) + self.wind * self.wind_scale) * (self.h * self.h);
        for chain in &self.chains {
            self.pos[chain.start] = self.target[chain.start];
            self.prev[chain.start] = self.target[chain.start];
            for i in chain.start + 1..chain.end {
                let x = self.pos[i];
                let moved = x + (x - self.prev[i]) * self.damping + accel;
                self.prev[i] = x;
                self.pos[i] = moved + (self.target[i] - moved) * self.spring[i];
            }
        }
        for _ in 0..self.iterations {
            for n in 0..self.chains.len() {
                let chain = self.chains[n];
                self.distance_pass(chain);
                self.collider_pass(chain);
            }
        }
    }

    fn distance_pass(&mut self, chain: Chain) {
        for i in chain.start + 1..chain.end {
            let d = self.pos[i] - self.pos[i - 1];
            let len = d.length();
            if len <= f32::EPSILON {
                continue; // coincident particles have no direction to place along; the spring separates them
            }
            self.pos[i] = self.pos[i - 1] + d * (self.segment[i] / len);
        }
    }

    fn collider_pass(&mut self, chain: Chain) {
        for i in chain.start + 1..chain.end {
            let mut p = self.pos[i];
            for &(centre, radius) in &self.spheres {
                let d = p - centre;
                let len = d.length();
                if len >= radius {
                    continue;
                }
                // A particle at the very centre has no direction of its own (the prototype then leaves it inside):
                // it leaves along the segment from its parent, or straight up if that is degenerate too.
                let dir =
                    if len > f32::EPSILON { d / len } else { (p - self.pos[i - 1]).try_normalize().unwrap_or(Vec3::Y) };
                p = centre + dir * radius;
            }
            self.pos[i] = p;
        }
    }

    /// Particle positions, world space, chains one after another in clump order.
    pub fn positions(&self) -> &[Vec3] {
        &self.pos
    }

    /// The particle range of each chain, in the order of the simulated clumps.
    pub fn chains(&self) -> impl ExactSizeIterator<Item = std::ops::Range<usize>> + '_ {
        self.chains.iter().map(|c| c.start..c.end)
    }

    /// Particle rest positions, bind space.
    pub fn rest(&self) -> &[Vec3] {
        &self.rest
    }

    /// The rest point (within its clump) each particle sits at.
    pub fn rest_indices(&self) -> &[u32] {
        &self.rest_index
    }

    /// Rest distance from each particle to the previous one in its chain (0 for a root).
    pub fn segment_lengths(&self) -> &[f32] {
        &self.segment
    }

    /// `HairClump::stiffness` at each particle.
    pub fn stiffness(&self) -> &[f32] {
        &self.stiffness
    }

    /// The colliders after targeting: world centre and radius including the margin.
    pub fn spheres(&self) -> &[(Vec3, f32)] {
        &self.spheres
    }

    /// A world-space point in character space (the root's inverse from the last [`ChainSet::targets`]).
    pub fn to_character(&self, p: Vec3) -> Vec3 {
        self.root_inverse.transform_point3(p)
    }

    /// Frame time carried to the next [`ChainSet::step`], seconds.
    pub fn carried(&self) -> f32 {
        self.accumulator
    }
}

/// The pull toward the target per fixed step `h` of a particle with stiffness `k`.
fn spring_factor(k: f32, h: f32) -> f32 {
    1.0 - (1.0 - k).powf(h * STIFFNESS_HZ)
}

/// The skinning matrix of `joint`, checked finite.
fn skin(pose: &dyn PoseSource, joint: &JointRef) -> Result<Mat4, VerletError> {
    pose.skin_matrix(joint).filter(Mat4::is_finite).ok_or_else(|| VerletError::MissingJoint { joint: joint.clone() })
}
