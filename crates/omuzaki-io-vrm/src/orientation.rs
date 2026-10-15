//! Turning a model half way round: the 0.x ↔ 1.0 orientation change.
//!
//! VRM 0.x models face −Z, VRM 1.0 (and the scene, `docs/design/io-api.md` D2) faces +Z. The 0.x importer calls
//! [`rotate_y180`] on the scene it built, the 0.x exporter calls it on a copy before writing, and since the rotation
//! is its own inverse the two directions are the same function.
//!
//! **Why this is exact for any pose, not just the T-pose 0.x requires.** Rotating the whole model by `R` (180° about Y,
//! which is `diag(-1, 1, -1)`) turns every world transform `W` into `R·W·R⁻¹`. Applying that *conjugation* to every
//! node's local transform does exactly this, because the chain of parents multiplies out: a local translation `t`
//! becomes `R·t`, a local rotation `q` becomes `R·q·R⁻¹`, and a local scale (a diagonal matrix) is unchanged. A skinned
//! vertex is then at `R·W·R⁻¹ · (R·B·R⁻¹) · (R·p)` = `R·(W·B·p)` with `B` the inverse bind matrix and `p` the vertex,
//! which is the old world position rotated, as wanted. So: nodes and every node-local vector are conjugated, vertex
//! data is rotated, and inverse bind matrices are conjugated. For identity-rotation bones, which 0.x demands, this
//! is exactly `x, z → −x, −z` on translations and vertex data, which is what `UniVRM`'s migrator does.
//!
//! Everything is a negation, so the conversion is bit-exact and applying it twice gives the original back.

use omuzaki_core::glam::{Mat4, Quat, Vec3};
use omuzaki_io_api::{AimAxis, ColliderShape, ConstraintKind, IoScene, JointLimit};

/// `R·v` for `R` = 180° about Y: negates X and Z.
fn flip(v: Vec3) -> Vec3 {
    Vec3::new(-v.x, v.y, -v.z)
}

/// `R·q·R⁻¹`: the rotation about the rotated axis by the same angle (negates the X and Z parts of the quaternion).
fn flip_rotation(q: Quat) -> Quat {
    Quat::from_xyzw(-q.x, q.y, -q.z, q.w)
}

/// A joint limit after the turn: its axes are conjugated like any node rotation, its angles are unchanged.
fn flip_limit(limit: JointLimit) -> JointLimit {
    match limit {
        JointLimit::Cone { angle, rotation } => JointLimit::Cone { angle, rotation: flip_rotation(rotation) },
        JointLimit::Hinge { angle, rotation } => JointLimit::Hinge { angle, rotation: flip_rotation(rotation) },
        JointLimit::Spherical { pitch, yaw, rotation } => {
            JointLimit::Spherical { pitch, yaw, rotation: flip_rotation(rotation) }
        }
        other => other,
    }
}

/// The shape after the turn: positions and the plane normal flip as vectors do.
fn flip_shape(shape: ColliderShape) -> ColliderShape {
    match shape {
        ColliderShape::Sphere { offset, radius } => ColliderShape::Sphere { offset: flip(offset), radius },
        ColliderShape::Capsule { offset, radius, tail } => {
            ColliderShape::Capsule { offset: flip(offset), radius, tail: flip(tail) }
        }
        ColliderShape::SphereInside { offset, radius } => ColliderShape::SphereInside { offset: flip(offset), radius },
        ColliderShape::CapsuleInside { offset, radius, tail } => {
            ColliderShape::CapsuleInside { offset: flip(offset), radius, tail: flip(tail) }
        }
        ColliderShape::Plane { offset, normal } => ColliderShape::Plane { offset: flip(offset), normal: flip(normal) },
        other => other,
    }
}

/// `R·M·R⁻¹` for a 4x4 matrix: every element is multiplied by the signs of its row and column.
fn flip_matrix(m: Mat4) -> Mat4 {
    const SIGN: [f32; 4] = [-1.0, 1.0, -1.0, 1.0];
    let mut a = m.to_cols_array();
    for (i, v) in a.iter_mut().enumerate() {
        *v *= SIGN[i / 4] * SIGN[i % 4];
    }
    Mat4::from_cols_array(&a)
}

/// The axis a constraint should name after the rotation: the node's local X and Z axes now point the other way.
fn flip_aim(axis: AimAxis) -> AimAxis {
    match axis {
        AimAxis::PositiveX => AimAxis::NegativeX,
        AimAxis::NegativeX => AimAxis::PositiveX,
        AimAxis::PositiveZ => AimAxis::NegativeZ,
        AimAxis::NegativeZ => AimAxis::PositiveZ,
        y => y,
    }
}

/// Rotates the whole scene 180° about Y, in place: 0.x ↔ 1.0. Node transforms, mesh and morph data, inverse bind
/// matrices, and the avatar's node-local vectors (look-at offset, collider shapes, spring gravity, aim axes) all turn
/// with the model. Unknown JSON in passthrough bags can't be interpreted and is left as it is.
pub fn rotate_y180(scene: &mut IoScene) {
    for n in &mut scene.nodes {
        n.transform.translation = flip(n.transform.translation);
        n.transform.rotation = flip_rotation(n.transform.rotation);
    }
    for p in scene.meshes.iter_mut().flat_map(|m| m.primitives.iter_mut()) {
        for v in p.mesh.positions.iter_mut().chain(&mut p.mesh.normals) {
            *v = flip(*v);
        }
        for t in &mut p.morph_targets {
            for v in t.positions.iter_mut().chain(&mut t.normals) {
                *v = flip(*v);
            }
        }
    }
    for s in &mut scene.skins {
        for m in &mut s.inverse_bind {
            *m = flip_matrix(*m);
        }
    }
    let Some(av) = &mut scene.avatar else { return };
    if let Some(look) = &mut av.look_at {
        look.offset_from_head_bone = flip(look.offset_from_head_bone);
    }
    for c in &mut av.spring_bones.colliders {
        c.shape = flip_shape(c.shape);
    }
    for j in av.spring_bones.chains.iter_mut().flat_map(|c| c.joints.iter_mut()) {
        j.gravity_dir = flip(j.gravity_dir);
        j.limit = j.limit.map(flip_limit);
    }
    for c in &mut av.constraints {
        if let ConstraintKind::Aim { axis, .. } = &mut c.kind {
            *axis = flip_aim(*axis);
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_io_api::{Avatar, MeshData, Node, Primitive, Skin};

    use super::*;

    #[test]
    fn conjugation_negates_x_and_z_and_is_its_own_inverse() {
        let q = Quat::from_axis_angle(Vec3::new(0.3, 0.8, -0.5).normalize(), 1.1);
        let r = flip_rotation(q);
        assert_eq!((r.x, r.y, r.z, r.w), (-q.x, q.y, -q.z, q.w));
        assert_eq!(flip_rotation(r), q);
        let m = Mat4::from_rotation_translation(q, Vec3::new(1.0, 2.0, 3.0));
        assert_eq!(flip_matrix(flip_matrix(m)), m);
    }

    /// The point of the conversion: the world position of every skinned vertex is the old one, rotated.
    #[test]
    fn a_skinned_vertex_ends_up_where_the_old_one_was_rotated_to() {
        let mut s = IoScene::new();
        let root = s.add_node(Node::new("root").at(Vec3::new(0.5, 0.0, 0.25)));
        let mut joint = Node::new("joint").at(Vec3::new(0.3, 1.0, -0.2));
        joint.transform.rotation = Quat::from_axis_angle(Vec3::new(1.0, 2.0, 3.0).normalize(), 0.7);
        let joint = s.add_node(joint);
        s.attach(root, joint);
        let bind = Mat4::from_translation(Vec3::new(-0.3, -1.0, 0.2));
        let mut prim = Primitive::new(MeshData {
            positions: vec![Vec3::new(0.1, 1.2, 0.3)],
            normals: vec![Vec3::new(0.0, 0.6, 0.8)],
            indices: vec![0, 0, 0],
            ..MeshData::default()
        });
        prim.mesh.joints = vec![[0, 0, 0, 0]];
        prim.mesh.weights = vec![[1.0, 0.0, 0.0, 0.0]];
        let mut mesh = omuzaki_io_api::Mesh::new("m");
        mesh.primitives = vec![prim];
        let mesh = s.add_mesh(mesh);
        let skin = s.add_skin(Skin::new("s", vec![joint], vec![bind]));
        s.nodes[joint.index()].mesh = Some(mesh);
        s.nodes[joint.index()].skin = Some(skin);
        s.avatar = Some(Avatar::new());

        let world = |s: &IoScene| {
            let w = |n: usize| {
                let t = s.nodes[n].transform;
                Mat4::from_scale_rotation_translation(t.scale, t.rotation, t.translation)
            };
            let joint_world = w(0) * w(1);
            let p = s.meshes[0].primitives[0].mesh.positions[0];
            joint_world.transform_point3(s.skins[0].inverse_bind[0].transform_point3(p))
        };
        let before = world(&s);
        rotate_y180(&mut s);
        let after = world(&s);
        assert!((after - flip(before)).length() < 1e-5, "{after:?} vs {:?}", flip(before));
        assert_eq!(s.meshes[0].primitives[0].mesh.normals[0], Vec3::new(0.0, 0.6, -0.8));
        rotate_y180(&mut s);
        assert!((world(&s) - before).length() < 1e-5);
    }

    #[test]
    fn flip_of_a_plane_normal_and_limit_rotation() {
        let mut s = IoScene::new();
        let node = s.add_node(Node::new("n"));
        let about = |axis, degrees: f32| Quat::from_axis_angle(axis, degrees.to_radians());
        let limit = |rotation| JointLimit::Cone { angle: 0.5, rotation };
        let mut joint = omuzaki_io_api::SpringJoint::new(node);
        joint.limit = Some(limit(about(Vec3::X, 90.0)));
        let mut other = joint;
        other.limit = Some(limit(about(Vec3::Y, 40.0)));
        let mut av = Avatar::new();
        av.spring_bones.colliders = vec![omuzaki_io_api::Collider {
            node,
            shape: ColliderShape::Plane { offset: Vec3::new(1.0, 2.0, 3.0), normal: Vec3::Z },
        }];
        av.spring_bones.chains = vec![omuzaki_io_api::SpringChain { joints: vec![joint, other], ..Default::default() }];
        s.avatar = Some(av);
        rotate_y180(&mut s);
        let av = s.avatar.as_ref().unwrap();
        assert_eq!(
            av.spring_bones.colliders[0].shape,
            ColliderShape::Plane { offset: Vec3::new(-1.0, 2.0, -3.0), normal: Vec3::NEG_Z }
        );
        let rotation = |j: usize| match av.spring_bones.chains[0].joints[j].limit {
            Some(JointLimit::Cone { rotation, .. }) => rotation,
            other => panic!("{other:?}"),
        };
        assert!(rotation(0).abs_diff_eq(about(Vec3::X, -90.0), 1e-6), "+90 about X becomes -90");
        assert!(rotation(1).abs_diff_eq(about(Vec3::Y, 40.0), 1e-6), "about Y is unchanged");
    }

    #[test]
    fn aim_axes_follow_the_flipped_local_axes() {
        assert_eq!(flip_aim(AimAxis::PositiveX), AimAxis::NegativeX);
        assert_eq!(flip_aim(AimAxis::NegativeZ), AimAxis::PositiveZ);
        assert_eq!(flip_aim(AimAxis::PositiveY), AimAxis::PositiveY);
    }
}
