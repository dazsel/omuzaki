//! The hair `PartRuntime` (#322): the simulated groups' Verlet chains ([`ChainSet`]) carry the hair's dynamic mesh.
//!
//! **Binding.** At build every vertex of the dynamic mesh is bound to the nearest point of the chains' rest polylines
//! (bind space): a segment between two neighbouring particles, the place `t` along it, and the vertex's offset from that
//! point. A tube's rings bind to their own lock's chain; a curtain strip's vertices to the guide they lie along.
//!
//! **Each step.** The chains get their targets from the pose (the styled shape carried by the head, or by the head and
//! the chest) and step. Each particle then has a frame: the rotation that takes its chain's target direction to the
//! simulated one (the mean of the segments either side), after the linear part of the transform its target follows.
//! A vertex is the point at `t` between its two particles plus its offset through the blend of their frames, brought
//! back to character space with the root's inverse; its normal goes through the same frame. Nothing allocates: every
//! buffer is sized at build.
//!
//! **Failures.** A pose that lacks a bone the hair follows leaves the hair where it was (a step changes nothing and the
//! revision does not move); the stage keeps drawing the last good shape.

use omuzaki_char_api::{
    DynamicMesh, HairClump, HairPhysics, JointRef, PartRuntime, PoseSource, RuntimeCost, SphereCollider, StepCtx,
};
use omuzaki_core::glam::{Mat3, Mat4, Quat, Vec3};
use omuzaki_core::mesh::MeshData;

use super::verlet::{ChainSet, VerletError};

/// One vertex's place on the chains: particles `a` and `a + 1`, the share `t` of the way from `a`, and the offset from
/// that point, bind space.
#[derive(Copy, Clone, Debug)]
struct Binding {
    a: u32,
    t: f32,
    offset: Vec3,
    normal: Vec3,
}

/// The simulated hair of one part.
#[derive(Clone, Debug)]
pub struct HairRuntime {
    chains: ChainSet,
    /// The chains as built, for [`PartRuntime::fork`].
    initial: ChainSet,
    bindings: Vec<Binding>,
    /// The rest mesh, character space: the initial state of `positions` and `normals`.
    rest_positions: Vec<Vec3>,
    rest_normals: Vec<Vec3>,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    /// Per particle, scratch for a step: the character-space position and frame.
    particle: Vec<Vec3>,
    frame: Vec<Mat3>,
    mesh: u16,
    revision: u64,
}

impl HairRuntime {
    /// The runtime of `geometry`, the part's `mesh`th mesh, carried by chains for the simulated `clumps`.
    /// `rig_colliders` are used when `physics` has none of its own; `head_y` is the head centre's bind-space height
    /// ([`ChainSet::set_head_height`]).
    ///
    /// # Errors
    /// [`VerletError`] when the chains can't be built, or [`VerletError::BadClump`] when no clump is simulated.
    pub fn new(
        clumps: &[HairClump],
        physics: &HairPhysics,
        rig_colliders: &[SphereCollider],
        head_y: f32,
        mesh: u16,
        geometry: &MeshData,
    ) -> Result<Self, VerletError> {
        let mut chains = ChainSet::from_clumps(clumps, physics, rig_colliders)?;
        chains.set_head_height(head_y);
        if chains.positions().is_empty() {
            return Err(VerletError::BadClump { clump: 0 });
        }
        let bindings = bind(&chains, geometry);
        let count = chains.positions().len();
        Ok(Self {
            initial: chains.clone(),
            chains,
            bindings,
            rest_positions: geometry.positions.clone(),
            rest_normals: geometry.normals.clone(),
            positions: geometry.positions.clone(),
            normals: geometry.normals.clone(),
            particle: vec![Vec3::ZERO; count],
            frame: vec![Mat3::IDENTITY; count],
            mesh,
            revision: 0,
        })
    }

    /// The chains, for tests and tools.
    pub fn chains(&self) -> &ChainSet {
        &self.chains
    }

    /// Rewrites the mesh from the chains' current state under the transforms of `pose`.
    fn rewrite(&mut self, pose: &dyn PoseSource) {
        let (Some(head), chest) = (pose.skin_matrix(&JointRef::HEAD), pose.skin_matrix(&JointRef::CHEST)) else {
            return;
        };
        let head = Mat3::from_mat4(head);
        let chest = chest.map_or(head, Mat3::from_mat4);
        let root_inverse: Mat4 = self.chains.root_inverse();
        let back = Mat3::from_mat4(root_inverse);
        let (x, target, weight) = (self.chains.positions(), self.chains.target_positions(), self.chains.head_weights());
        for range in self.chains.chains() {
            for i in range.clone() {
                // The rotation of the segment into `i` and of the one out of it; the root takes its outgoing one.
                let arc = |from: usize, to: usize| {
                    let rest = (target[to] - target[from]).normalize_or_zero();
                    let now = (x[to] - x[from]).normalize_or_zero();
                    if rest == Vec3::ZERO || now == Vec3::ZERO {
                        Quat::IDENTITY
                    } else {
                        Quat::from_rotation_arc(rest, now)
                    }
                };
                let incoming = (i > range.start).then(|| arc(i - 1, i));
                let outgoing = (i + 1 < range.end).then(|| arc(i, i + 1));
                let q = match (incoming, outgoing) {
                    (Some(a), Some(b)) => a.lerp(b, 0.5).normalize(),
                    (Some(q), None) | (None, Some(q)) => q,
                    (None, None) => Quat::IDENTITY,
                };
                let follow = if weight[i] >= 1.0 { head } else { lerp3(chest, head, weight[i]) };
                self.frame[i] = back * Mat3::from_quat(q) * follow;
                self.particle[i] = root_inverse.transform_point3(x[i]);
            }
        }
        for ((b, p), n) in self.bindings.iter().zip(&mut self.positions).zip(&mut self.normals) {
            let a = b.a as usize;
            let frame = lerp3(self.frame[a], self.frame[a + 1], b.t);
            *p = self.particle[a].lerp(self.particle[a + 1], b.t) + frame * b.offset;
            *n = (frame * b.normal).normalize_or(b.normal);
        }
        self.revision += 1;
    }
}

/// `a` to `b` by `t`, entry by entry.
fn lerp3(a: Mat3, b: Mat3, t: f32) -> Mat3 {
    a * (1.0 - t) + b * t
}

/// Binds every vertex of `geometry` to the nearest point of the chains' rest polylines. Vertices come in locks and
/// strips, so the chain the last vertex bound to is tried first, and a chain whose box is further than the best point
/// found so far is skipped.
///
/// Vertices at one point (within [`SAME_POINT`]) take one place on the chains, the first one's, and keep their own
/// normals. Neighbouring curtain strips meet halfway between their guides with a copy each, which the renderer welds
/// into one seam when it creates the mesh; bound each to its own guide, the copies would part as soon as the guides
/// moved differently, tearing the seam.
fn bind(chains: &ChainSet, geometry: &MeshData) -> Vec<Binding> {
    let mut out = bind_nearest(chains, geometry);
    for (k, first) in first_at_same_point(&geometry.positions).into_iter().enumerate() {
        if first != k {
            let Binding { a, t, offset, .. } = out[first];
            out[k] = Binding { a, t, offset, normal: out[k].normal };
        }
    }
    out
}

/// How close, in metres on every axis, two vertices are to count as one point: twice the renderer's weld tolerance, so
/// every pair it welds is caught.
const SAME_POINT: f32 = 2e-6;

/// For each position, the index of the first position at the same point (itself when it is the first). Positions are
/// hashed into cells [`SAME_POINT`] wide, so each looks only at its own and the neighbouring cells.
#[allow(clippy::cast_possible_truncation)] // cell coordinates of a character-sized mesh fit an i64
fn first_at_same_point(positions: &[Vec3]) -> Vec<usize> {
    let cell = |p: Vec3| (p / SAME_POINT).floor().to_array().map(|c| c as i64);
    let mut cells: std::collections::HashMap<[i64; 3], Vec<usize>> = std::collections::HashMap::new();
    let mut first = Vec::with_capacity(positions.len());
    for (k, &p) in positions.iter().enumerate() {
        let [x, y, z] = cell(p);
        let mut found = k;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let near = cells.get(&[x + dx, y + dy, z + dz]).into_iter().flatten();
                    for &j in near.filter(|&&j| (positions[j] - p).abs().max_element() <= SAME_POINT) {
                        found = found.min(first[j]);
                    }
                }
            }
        }
        first.push(found);
        cells.entry([x, y, z]).or_default().push(k);
    }
    first
}

/// [`bind`]'s nearest point for each vertex on its own.
fn bind_nearest(chains: &ChainSet, geometry: &MeshData) -> Vec<Binding> {
    let rest = chains.rest();
    let ranges: Vec<std::ops::Range<usize>> = chains.chains().collect();
    let boxes: Vec<(Vec3, Vec3)> = ranges
        .iter()
        .map(|r| rest[r.clone()].iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), p| (lo.min(*p), hi.max(*p))))
        .collect();
    let nearest_on = |r: &std::ops::Range<usize>, v: Vec3, best: &mut (f32, usize, f32)| {
        for a in r.start..r.end.saturating_sub(1) {
            let (p, q) = (rest[a], rest[a + 1]);
            let d = q - p;
            let t = if d.length_squared() > 0.0 { ((v - p).dot(d) / d.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
            let distance = v.distance_squared(p + d * t);
            if distance < best.0 {
                *best = (distance, a, t);
            }
        }
    };
    let mut last = 0;
    let mut out = Vec::with_capacity(geometry.positions.len());
    for (k, &v) in geometry.positions.iter().enumerate() {
        let mut best = (f32::INFINITY, ranges[last].start, 0.0);
        nearest_on(&ranges[last], v, &mut best);
        for (c, r) in ranges.iter().enumerate() {
            let (lo, hi) = boxes[c];
            if c == last || v.clamp(lo, hi).distance_squared(v) >= best.0 {
                continue;
            }
            let before = best.1;
            nearest_on(r, v, &mut best);
            if best.1 != before {
                last = c;
            }
        }
        let (a, t) = (best.1, best.2);
        let normal = geometry.normals.get(k).copied().unwrap_or(Vec3::Y);
        #[allow(clippy::cast_possible_truncation)] // a mesh holds far fewer particles than u32::MAX
        out.push(Binding { a: a as u32, t, offset: v - rest[a].lerp(rest[a + 1], t), normal });
    }
    out
}

impl PartRuntime for HairRuntime {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        if self.chains.targets(ctx.pose).is_err() || self.chains.set_wind(ctx.wind).is_err() {
            return;
        }
        if self.chains.step(ctx.dt).is_err() {
            return;
        }
        self.rewrite(ctx.pose);
    }

    fn reset(&mut self, pose: &dyn PoseSource) {
        if self.chains.reset(pose).is_ok() {
            self.rewrite(pose);
        }
    }

    fn dynamic_count(&self) -> usize {
        1
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        (index == 0).then_some(DynamicMesh {
            mesh: self.mesh,
            positions: &self.positions,
            normals: &self.normals,
            revision: self.revision,
        })
    }

    fn cost(&self) -> RuntimeCost {
        let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
        RuntimeCost {
            particles: count(self.chains.positions().len()),
            colliders: count(self.chains.spheres().len()),
            vertices: count(self.positions.len()),
        }
    }

    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        Some(Box::new(Self {
            chains: self.initial.clone(),
            initial: self.initial.clone(),
            bindings: self.bindings.clone(),
            rest_positions: self.rest_positions.clone(),
            rest_normals: self.rest_normals.clone(),
            positions: self.rest_positions.clone(),
            normals: self.rest_normals.clone(),
            particle: self.particle.clone(),
            frame: self.frame.clone(),
            mesh: self.mesh,
            revision: 0,
        }))
    }

    fn heap_bytes(&self) -> usize {
        let vectors = 4 * self.positions.len() + 6 * self.particle.len();
        vectors * size_of::<Vec3>()
            + self.bindings.len() * size_of::<Binding>()
            + self.frame.len() * size_of::<Mat3>()
            + 2 * self.chains.positions().len() * 4 * size_of::<f32>()
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss)] // a few points and frames

    use omuzaki_char_api::{Bulge, Falloff, Follow, GroupId, HairClump, HairPhysics, Taper};
    use omuzaki_core::time::Seconds;

    use super::*;

    /// No bone moves: the hair moves only by its own simulation.
    struct Still;

    impl PoseSource for Still {
        fn skin_matrix(&self, _: &JointRef) -> Option<Mat4> {
            Some(Mat4::IDENTITY)
        }

        fn root(&self) -> Mat4 {
            Mat4::IDENTITY
        }
    }

    fn clump(x: f32, stiffness: f64) -> HairClump {
        HairClump {
            group: GroupId::new("c-curtain").unwrap_or_else(|e| panic!("{e}")),
            rest: (0..8).map(|k| Vec3::new(x, 1.0 - 0.04 * k as f32, -0.1)).collect(),
            width: Taper::default(),
            thickness: Bulge::default(),
            stiffness: Falloff { root: 1.0, tip: stiffness, from: 0.0, to: 1.0 },
            shade: 1.0,
            simulate: true,
            follow: Follow::Body,
            tint: None,
        }
    }

    #[test]
    fn copies_of_one_point_bound_beside_different_chains_move_as_one() {
        // Two strips, each with its own copy of the line halfway between their guides, listed after the strip's own
        // vertices, so each copy is nearest (by a tie) to the chain its strip lies along.
        let (stiff, floppy) = (clump(-0.02, 0.9), clump(0.02, 0.02));
        let line = |x: f32| (0..8).map(move |k| Vec3::new(x, 1.0 - 0.04 * k as f32, -0.1));
        let positions: Vec<Vec3> = line(-0.03).chain(line(0.0)).chain(line(0.03)).chain(line(0.0)).collect();
        let geometry = MeshData { normals: vec![Vec3::Z; positions.len()], positions, ..MeshData::default() };
        let mut runtime = HairRuntime::new(&[stiff, floppy], &HairPhysics::default(), &[], 1.0, 0, &geometry)
            .unwrap_or_else(|e| panic!("{e}"));
        let mut parted = 0.0_f32;
        for frame in 0..120 {
            let t = frame as f32 / 60.0;
            let wind = Vec3::new(3.0 * (4.0 * t).sin(), 0.0, 2.0);
            runtime.step(&StepCtx::new(1.0 / 60.0, Seconds(f64::from(t)), &Still, wind));
            let view = runtime.dynamic_mesh(0).unwrap_or_else(|| panic!("the mesh"));
            parted = (0..8).map(|k| view.positions[8 + k].distance(view.positions[24 + k])).fold(parted, f32::max);
        }
        assert!(parted <= 1e-6, "the copies parted by {parted} m");
    }
}
