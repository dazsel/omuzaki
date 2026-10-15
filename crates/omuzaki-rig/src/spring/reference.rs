//! The oracle: a direct transcription of the `VRMC_springBone` 1.0 pseudocode (its algorithm section) in `f64`, with
//! full matrices and a naive walk up the tree for every world transform. It shares no code with the solver, so
//! agreement between the two means the solver's incremental, `f32`, flat-buffer version computes the spec's numbers.
//!
//! Where the spec's pseudocode is loose, this follows the structure the spec describes: tails are stored in centre
//! space, inertia is evaluated there, and gravity, stiffness and collisions are in world space.

use omuzaki_anim_api::{ColliderShape, PHYSICS_STEP, Pose, Skeleton, SpringSet};
use omuzaki_core::glam::{DMat4, DQuat, DVec3};

/// The spec's `SpringBoneJointState` plus the joint's properties.
struct Joint {
    node: usize,
    prev_tail: DVec3,
    current_tail: DVec3,
    bone_axis: DVec3,
    bone_length: f64,
    initial_local_matrix: DMat4,
    initial_local_rotation: DQuat,
    stiffness: f64,
    drag: f64,
    gravity: DVec3,
    hit_radius: f64,
}

struct Chain {
    joints: Vec<Joint>,
    center: Option<usize>,
    colliders: Vec<(usize, ColliderShape)>,
}

/// A spring system evolving by the spec's algorithm.
pub struct Reference {
    parent: Vec<Option<usize>>,
    /// Every node's current local matrix: the animated pose, with swinging bones at their solved rotation.
    local: Vec<DMat4>,
    chains: Vec<Chain>,
}

fn matrix(t: &omuzaki_anim_api::Transform) -> DMat4 {
    DMat4::from_rotation_translation(t.rotation.as_dquat(), t.translation.as_dvec3())
}

/// The shortest rotation from unit vector `from` to unit vector `to` (three.js `setFromUnitVectors`).
fn from_to(from: DVec3, to: DVec3) -> DQuat {
    let r = from.dot(to) + 1.0;
    let q = if r < 1e-12 {
        if from.x.abs() > from.z.abs() {
            DQuat::from_xyzw(-from.y, from.x, 0.0, 0.0)
        } else {
            DQuat::from_xyzw(0.0, -from.z, from.y, 0.0)
        }
    } else {
        let c = from.cross(to);
        DQuat::from_xyzw(c.x, c.y, c.z, r)
    };
    q.normalize()
}

impl Reference {
    /// The system at rest in `pose`: swinging bones at their rest rotation, each tail where it rests.
    pub fn new(skeleton: &Skeleton, set: &SpringSet, pose: &Pose) -> Self {
        let bones = skeleton.bones();
        let parent = bones.iter().map(|b| b.parent.map(omuzaki_anim_api::BoneId::index)).collect();
        let mut this = Self { parent, local: bones.iter().map(|b| matrix(&b.rest)).collect(), chains: Vec::new() };
        let rest_world: Vec<DMat4> = (0..bones.len()).map(|i| this.world(i)).collect();
        this.local = (0..bones.len()).map(|i| matrix(&pose.local[i])).collect();

        // The spec updates a joint after its ancestors: chains in order of their first bone.
        let mut order: Vec<_> = set.chains.iter().collect();
        order.sort_by_key(|c| c.joints[0].bone);
        for chain in order {
            let colliders = chain
                .collider_groups
                .iter()
                .flat_map(|name| set.groups.iter().filter(move |g| &g.name == name))
                .flat_map(|g| g.colliders.iter().map(|c| (c.bone.index(), c.shape)))
                .collect();
            let mut joints = Vec::new();
            for pair in chain.joints.windows(2) {
                let (head, tail) = (pair[0].bone.index(), pair[1].bone.index());
                let rest = &skeleton.bones()[head].rest;
                let tail_local = rest_world[head].inverse().transform_point3(rest_world[tail].w_axis.truncate());
                let p = pair[0].params;
                joints.push(Joint {
                    node: head,
                    prev_tail: DVec3::ZERO,
                    current_tail: DVec3::ZERO,
                    bone_axis: tail_local.normalize(),
                    bone_length: tail_local.length(),
                    initial_local_matrix: matrix(rest),
                    initial_local_rotation: rest.rotation.as_dquat(),
                    stiffness: f64::from(p.stiffness),
                    drag: f64::from(p.drag_force),
                    gravity: p.gravity_dir.as_dvec3() * f64::from(p.gravity_power),
                    hit_radius: f64::from(p.hit_radius),
                });
            }
            let center = chain.center.map(omuzaki_anim_api::BoneId::index);
            this.chains.push(Chain { joints, center, colliders });
        }
        // Spring bones start at their rest local matrix; tails start where that puts the next joint.
        for chain in &mut this.chains {
            for joint in &chain.joints {
                this.local[joint.node] = joint.initial_local_matrix;
            }
        }
        for c in 0..this.chains.len() {
            let to_center = this.center_inverse(c);
            for j in 0..this.chains[c].joints.len() {
                let joint = &this.chains[c].joints[j];
                let tail_world = this.world(joint.node).transform_point3(joint.bone_axis * joint.bone_length);
                let tail = to_center.transform_point3(tail_world);
                let joint = &mut this.chains[c].joints[j];
                joint.prev_tail = tail;
                joint.current_tail = tail;
            }
        }
        this
    }

    /// The world matrix of `node`, walking up the tree.
    pub fn world(&self, node: usize) -> DMat4 {
        match self.parent[node] {
            Some(p) => self.world(p) * self.local[node],
            None => self.local[node],
        }
    }

    fn center_inverse(&self, chain: usize) -> DMat4 {
        self.chains[chain].center.map_or(DMat4::IDENTITY, |c| self.world(c).inverse())
    }

    /// The solved local rotation of a swinging bone.
    pub fn rotation(&self, node: usize) -> DQuat {
        DQuat::from_mat4(&self.local[node])
    }

    /// One fixed step with the animated bones in `pose`.
    pub fn substep(&mut self, pose: &Pose) {
        let swinging: Vec<usize> = self.chains.iter().flat_map(|c| c.joints.iter().map(|j| j.node)).collect();
        for i in 0..self.local.len() {
            if !swinging.contains(&i) {
                self.local[i] = matrix(&pose.local[i]);
            }
        }
        for c in 0..self.chains.len() {
            for j in 0..self.chains[c].joints.len() {
                self.update_joint(c, j);
            }
        }
    }

    fn update_joint(&mut self, c: usize, j: usize) {
        let dt = f64::from(PHYSICS_STEP);
        let center_to_world = self.chains[c].center.map_or(DMat4::IDENTITY, |n| self.world(n));
        let world_to_center = center_to_world.inverse();
        let joint = &self.chains[c].joints[j];
        let world_position = self.world(joint.node).w_axis.truncate();
        let parent_world = self.parent[joint.node].map_or(DMat4::IDENTITY, |p| self.world(p));
        let parent_world_rotation = DQuat::from_mat4(&parent_world);

        // Inertia, in centre space; then to the world, where stiffness and gravity are added.
        let inertia = (joint.current_tail - joint.prev_tail) * (1.0 - joint.drag);
        let stiffness = dt * (parent_world_rotation * joint.initial_local_rotation * joint.bone_axis) * joint.stiffness;
        let external = dt * joint.gravity;
        let mut next_tail = center_to_world.transform_point3(joint.current_tail + inertia) + stiffness + external;
        next_tail = world_position + (next_tail - world_position).normalize() * joint.bone_length;

        // Collision with colliders.
        for &(bone, shape) in &self.chains[c].colliders {
            let collider_world = self.world(bone);
            let (direction, distance) = collide(&collider_world, shape, next_tail, joint.hit_radius);
            if distance < 0.0 {
                next_tail -= direction * distance;
                next_tail = world_position + (next_tail - world_position).normalize() * joint.bone_length;
            }
        }

        // Applying rotation.
        let to = (parent_world * joint.initial_local_matrix).inverse().transform_point3(next_tail).normalize();
        let rotation = joint.initial_local_rotation * from_to(joint.bone_axis, to);
        let translation = joint.initial_local_matrix.w_axis.truncate();
        let node = joint.node;

        let joint = &mut self.chains[c].joints[j];
        joint.prev_tail = joint.current_tail;
        joint.current_tail = world_to_center.transform_point3(next_tail);
        self.local[node] = DMat4::from_rotation_translation(rotation, translation);
    }
}

/// The spec's collider pseudocode: the direction from the collider to the joint, and the signed distance between their
/// surfaces (negative when they overlap). Note the spec's capsule listing reads `delta` before it assigns it; the order
/// here is the intended one. The extended colliders (`VRMC_springBone_extended_collider`) flip the sign of the distance
/// for the inside shapes, and a plane's direction is its normal.
fn collide(world: &DMat4, shape: ColliderShape, next_tail: DVec3, joint_radius: f64) -> (DVec3, f64) {
    let capsule_delta = |offset: omuzaki_core::glam::Vec3, tail: omuzaki_core::glam::Vec3| {
        let transformed_offset = world.transform_point3(offset.as_dvec3());
        let transformed_tail = world.transform_point3(tail.as_dvec3());
        let offset_to_tail = transformed_tail - transformed_offset;
        let mut delta = next_tail - transformed_offset;
        if offset_to_tail.length_squared() == 0.0 {
            // The ends coincide: a ball. The spec's division by the span would be 0/0.
            return delta;
        }
        let dot = offset_to_tail.dot(delta);
        if dot < 0.0 {
            // At the head side of the capsule: nothing to do.
        } else if dot > offset_to_tail.length_squared() {
            delta -= offset_to_tail;
        } else {
            delta -= offset_to_tail * (dot / offset_to_tail.length_squared());
        }
        delta
    };
    let sphere_delta = |offset: omuzaki_core::glam::Vec3| next_tail - world.transform_point3(offset.as_dvec3());
    let outside =
        |delta: DVec3, radius: f32| (delta.normalize_or_zero(), delta.length() - f64::from(radius) - joint_radius);
    // Inside: the joint must stay within the radius, and is pushed back toward the axis.
    let inside =
        |delta: DVec3, radius: f32| (-delta.normalize_or_zero(), f64::from(radius) - delta.length() - joint_radius);
    match shape {
        ColliderShape::Sphere { offset, radius } => outside(sphere_delta(offset), radius),
        ColliderShape::Capsule { offset, radius, tail } => outside(capsule_delta(offset, tail), radius),
        ColliderShape::SphereInside { offset, radius } => inside(sphere_delta(offset), radius),
        ColliderShape::CapsuleInside { offset, radius, tail } => inside(capsule_delta(offset, tail), radius),
        ColliderShape::Plane { offset, normal } => {
            let normal = world.transform_vector3(normal.as_dvec3()).normalize();
            let delta = next_tail - world.transform_point3(offset.as_dvec3());
            (normal, delta.dot(normal) - joint_radius)
        }
        _ => (DVec3::ZERO, 0.0),
    }
}
