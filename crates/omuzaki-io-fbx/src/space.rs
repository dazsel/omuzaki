//! The change of basis from the file's space to canonical space, for nodes, vertices and bind matrices alike.
//!
//! ufbx leaves the conversion in the root node's transform (`SpaceConversion::TransformRoot`), a linear map `B` (a signed
//! permutation of the axes times the unit scale). The canonical world is `B · W` for a file world transform `W`. Every
//! node's local transform is conjugated, `B · L · B⁻¹`, so the chain of locals multiplies out to `B · W · B⁻¹`; what a
//! node carries (a vertex, a bind matrix, a blend-shape offset) is therefore mapped by `B` itself (`B · W · v` is
//! `(B · W · B⁻¹) · (B · v)`), and a bind matrix, which maps one such space to another, is conjugated like a local.

use omuzaki_core::glam::{DMat3, DMat4, DQuat, DVec3, Mat4, Vec3};
use omuzaki_io_api::Transform;

/// The change of basis from the file's space to canonical space: the linear part of the root transform ufbx computes from
/// the file's axes and unit, a signed permutation of the axes times the unit scale.
///
/// Every node's local transform is conjugated by it, so a turn about the file's up axis becomes a turn about +Y at the
/// node that has it. ufbx's own `AdjustTransforms` would adjust only the root's children and leave the rest in the file's
/// frame.
pub(crate) struct Basis {
    /// Axes and unit scale together; maps a file position to a canonical one.
    linear: DMat3,
    /// The axes alone (orthogonal; its determinant is -1 for a file of the other handedness).
    axes: DMat3,
}

impl Basis {
    pub(crate) fn new(root: &ufbx::Transform) -> Self {
        let rotation = DQuat::from_xyzw(root.rotation.x, root.rotation.y, root.rotation.z, root.rotation.w).normalize();
        let scale = DVec3::new(root.scale.x, root.scale.y, root.scale.z);
        let linear = DMat3::from_quat(rotation) * DMat3::from_diagonal(scale);
        // The unit scale is uniform; dividing it out leaves the orthogonal part.
        let unit = linear.x_axis.length();
        Self { linear, axes: linear * (1.0 / unit) }
    }

    /// `local` in canonical space: translation scaled and turned, rotation conjugated, scale permuted.
    pub(crate) fn apply(&self, local: &ufbx::Transform) -> Transform {
        let q = DQuat::from_xyzw(local.rotation.x, local.rotation.y, local.rotation.z, local.rotation.w);
        let turn = if q.is_finite() && q.length_squared() > f64::EPSILON { q.normalize() } else { DQuat::IDENTITY };
        // A rotation seen through an axis permutation is a rotation (a mirror reverses its sense, which the matrix
        // conjugation carries), so the result is always a proper one.
        let rotation = DQuat::from_mat3(&(self.axes * DMat3::from_quat(turn) * self.axes.transpose())).normalize();
        let scale = DVec3::new(local.scale.x, local.scale.y, local.scale.z);
        // One non-zero entry per row of `axes`, so this permutes the scale components.
        let abs = DMat3::from_cols(self.axes.x_axis.abs(), self.axes.y_axis.abs(), self.axes.z_axis.abs());
        let translation = self.linear * DVec3::new(local.translation.x, local.translation.y, local.translation.z);
        Transform { translation: translation.as_vec3(), rotation: rotation.as_quat(), scale: (abs * scale).as_vec3() }
    }

    /// A position (or a position offset) in canonical space.
    pub(crate) fn point(&self, p: ufbx::Vec3) -> Vec3 {
        (self.linear * DVec3::new(p.x, p.y, p.z)).as_vec3()
    }

    /// A direction (a normal, or a normal offset) in canonical space: turned by the axes, not scaled.
    pub(crate) fn direction(&self, d: ufbx::Vec3) -> Vec3 {
        (self.axes * DVec3::new(d.x, d.y, d.z)).as_vec3()
    }

    /// True when the file is of the other handedness: the conversion mirrors, so a triangle's corners must be reversed
    /// to keep its front face.
    pub(crate) fn mirrors(&self) -> bool {
        self.axes.determinant() < 0.0
    }

    /// An affine map between two node-carried spaces (a bind matrix) in canonical space: `B · M · B⁻¹`.
    pub(crate) fn conjugate(&self, m: &ufbx::Matrix) -> Mat4 {
        let file = DMat4::from_cols_array(&[
            m.m00, m.m10, m.m20, 0.0, m.m01, m.m11, m.m21, 0.0, m.m02, m.m12, m.m22, 0.0, m.m03, m.m13, m.m23, 1.0,
        ]);
        let b = DMat4::from_mat3(self.linear);
        (b * file * b.inverse()).as_mat4()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root(rotation: DQuat, unit: f64) -> ufbx::Transform {
        ufbx::Transform {
            translation: ufbx::Vec3 { x: 0.0, y: 0.0, z: 0.0 },
            rotation: ufbx::Quat { x: rotation.x, y: rotation.y, z: rotation.z, w: rotation.w },
            scale: ufbx::Vec3 { x: unit, y: unit, z: unit },
        }
    }

    #[test]
    fn a_z_up_centimetre_file_maps_points_directions_and_bind_matrices_consistently() {
        // Z up to Y up is a turn of -90 degrees about X; centimetres to metres is 0.01.
        let basis = Basis::new(&root(DQuat::from_rotation_x(-std::f64::consts::FRAC_PI_2), 0.01));
        let up = ufbx::Vec3 { x: 0.0, y: 0.0, z: 100.0 };
        assert!(basis.point(up).abs_diff_eq(Vec3::Y, 1e-6), "{}", basis.point(up));
        assert!(basis.direction(ufbx::Vec3 { x: 0.0, y: 0.0, z: 1.0 }).abs_diff_eq(Vec3::Y, 1e-6));
        assert!(!basis.mirrors());
        // A bind matrix that moves a point 100 units up the file's Z moves it one metre up canonical Y.
        let shift = ufbx::Matrix {
            m00: 1.0,
            m10: 0.0,
            m20: 0.0,
            m01: 0.0,
            m11: 1.0,
            m21: 0.0,
            m02: 0.0,
            m12: 0.0,
            m22: 1.0,
            m03: 0.0,
            m13: 0.0,
            m23: 100.0,
        };
        let moved = basis.conjugate(&shift).transform_point3(Vec3::ZERO);
        assert!(moved.abs_diff_eq(Vec3::Y, 1e-5), "{moved}");
    }
}
