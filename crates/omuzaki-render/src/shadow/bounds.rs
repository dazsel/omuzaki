//! World-space bounds of shadow casters, including skinned ones.
//!
//! A mesh keeps its bind-pose box and, if it is skinned, one box per joint around the vertices that joint influences.
//! A posed vertex is a weighted average of its joints' images of it, so it lies inside the box of those images; the
//! union over joints of each joint's box moved by that joint's matrix therefore holds the whole posed mesh, without
//! touching a vertex at frame time.

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_render_api::MeshData;

use super::fit::Aabb;
use crate::resource::mesh::{Shape, Surface};

/// A mesh's boxes in its own (bind-pose) space.
#[derive(Clone)]
pub(crate) struct MeshBounds {
    /// Every vertex.
    whole: Aabb,
    /// Joint `j`'s box around the vertices that weight it: empty for a joint nothing weights. Empty for a rigid mesh.
    joints: Vec<Aabb>,
}

impl MeshBounds {
    /// No boxes yet: the starting point of a box that is only ever [`refit`](Self::refit).
    pub(crate) fn empty() -> Self {
        Self { whole: Aabb::EMPTY, joints: Vec::new() }
    }

    /// Overwrites `self` with `base`'s boxes grown by `down` below and `up` above on each axis: the conservative bounds
    /// of an instance whose vertices may each have moved by up to that much from where `base` has them. Allocates only
    /// if the joint count grew.
    pub(crate) fn set_grown(&mut self, base: &Self, down: Vec3, up: Vec3) {
        self.whole = Aabb { min: base.whole.min - down, max: base.whole.max + up };
        self.joints.clear();
        self.joints.extend(base.joints.iter().map(|b| Aabb { min: b.min - down, max: b.max + up }));
    }

    /// The boxes of `mesh`, whose joint count (one past the highest weighted joint) is `joints_needed`.
    pub(crate) fn of(mesh: &MeshData, joints_needed: u32) -> Self {
        let mut whole = Aabb::EMPTY;
        for &p in &mesh.positions {
            whole.include(p);
        }
        let mut joints = vec![Aabb::EMPTY; joints_needed as usize];
        for ((&p, ids), weights) in mesh.positions.iter().zip(&mesh.joints).zip(&mesh.weights) {
            for (&id, &weight) in ids.iter().zip(weights) {
                if weight != 0.0
                    && let Some(bounds) = joints.get_mut(usize::from(id))
                {
                    bounds.include(p);
                }
            }
        }
        Self { whole, joints }
    }

    /// Recomputes the boxes from a mesh's `shapes` (after its vertices moved, or an instance's morphed copy) and its
    /// `skin` data (empty for a rigid mesh), for `joints_needed` joints. Allocates only if the joint count grew.
    pub(crate) fn refit(&mut self, joints_needed: u32, shapes: &[Shape], skin: &[Surface]) {
        self.whole = Aabb::EMPTY;
        for shape in shapes {
            self.whole.include(Vec3::from(shape.position));
        }
        self.joints.clear();
        self.joints.resize(joints_needed as usize, Aabb::EMPTY);
        for (shape, skin) in shapes.iter().zip(skin) {
            let p = Vec3::from(shape.position);
            for (&id, &weight) in skin.joints.iter().zip(&skin.weights) {
                if weight != 0.0
                    && let Some(bounds) = self.joints.get_mut(usize::from(id))
                {
                    bounds.include(p);
                }
            }
        }
    }

    /// The world-space box of an instance with model matrix `model`, posed by `palette` (the skin matrices of its joint
    /// range) if the mesh is skinned and the palette covers its joints; otherwise the bind-pose box moved by `model`.
    pub(crate) fn instance(&self, model: &Mat4, palette: Option<&[Mat4]>) -> Aabb {
        match palette {
            Some(palette) if !self.joints.is_empty() && palette.len() >= self.joints.len() => {
                let mut out = Aabb::EMPTY;
                for (bounds, skin) in self.joints.iter().zip(palette) {
                    out.union(&bounds.transformed(&(*model * *skin)));
                }
                out
            }
            _ => self.whole.transformed(model),
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec3;
    use omuzaki_render_mock::shapes;

    use super::*;

    fn skinned_quad() -> MeshData {
        let mut quad = shapes::quad(omuzaki_core::glam::Vec2::ONE);
        quad.joints = vec![[0, 0, 0, 0], [0, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0]];
        quad.weights = vec![[1.0, 0.0, 0.0, 0.0]; 4];
        quad
    }

    #[test]
    fn a_rigid_mesh_is_its_vertex_box_moved_by_the_model() {
        let cube = shapes::cuboid(Vec3::splat(0.5));
        let bounds = MeshBounds::of(&cube, 0);
        let moved = bounds.instance(&Mat4::from_translation(Vec3::new(1.0, 2.0, 3.0)), None);
        assert_eq!(moved, Aabb::new(Vec3::new(0.5, 1.5, 2.5), Vec3::new(1.5, 2.5, 3.5)));
    }

    #[test]
    fn a_skinned_mesh_follows_its_joints() {
        let quad = skinned_quad();
        let bounds = MeshBounds::of(&quad, 2);
        let rest = bounds.instance(&Mat4::IDENTITY, Some(&[Mat4::IDENTITY, Mat4::IDENTITY]));
        let lifted =
            bounds.instance(&Mat4::IDENTITY, Some(&[Mat4::IDENTITY, Mat4::from_translation(Vec3::new(0.0, 0.0, 5.0))]));
        assert!(rest.max.z.abs() < 1e-6, "rest pose stays flat");
        assert!(lifted.max.z >= 5.0, "the lifted joint's vertices lift the box: {lifted:?}");
        assert!(lifted.min.z <= 0.0, "the other joint's vertices stay put");
    }

    #[test]
    fn a_short_palette_falls_back_to_the_bind_pose_box() {
        let quad = skinned_quad();
        let bounds = MeshBounds::of(&quad, 2);
        assert_eq!(bounds.instance(&Mat4::IDENTITY, Some(&[Mat4::IDENTITY])), bounds.whole);
        assert_eq!(bounds.instance(&Mat4::IDENTITY, None), bounds.whole);
    }

    #[test]
    fn zero_weights_do_not_stretch_a_joints_box() {
        let mesh = MeshData {
            positions: vec![Vec3::new(-5.0, 0.0, 0.0), Vec3::new(5.0, 0.0, 0.0)],
            normals: vec![Vec3::Z; 2],
            joints: vec![[0, 1, 0, 0], [1, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0], [1.0, 0.0, 0.0, 0.0]],
            ..MeshData::default()
        };
        let bounds = MeshBounds::of(&mesh, 2);
        assert_eq!(bounds.joints[1].min.x, 5.0, "vertex 0 weights joint 1 by zero");
        assert_eq!(bounds.joints[0].max.x, -5.0);
    }
}
