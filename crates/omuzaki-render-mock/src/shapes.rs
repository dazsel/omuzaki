//! Tiny meshes for tests: enough geometry to exercise lighting, outlines, overlays and skinning without assets.

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_render_api::MeshData;

/// An axis-aligned box centred on the origin with hard edges (24 vertices, per-face normals) and UVs that put the
/// whole image on every face.
pub fn cuboid(half: Vec3) -> MeshData {
    // Each face: outward normal, then the two in-plane axes with u × v = normal, so (-u,-v) → (+u,-v) → (+u,+v) is
    // counter-clockwise seen from outside.
    let faces = [
        (Vec3::Z, Vec3::X, Vec3::Y),
        (Vec3::NEG_Z, Vec3::NEG_X, Vec3::Y),
        (Vec3::X, Vec3::NEG_Z, Vec3::Y),
        (Vec3::NEG_X, Vec3::Z, Vec3::Y),
        (Vec3::Y, Vec3::X, Vec3::NEG_Z),
        (Vec3::NEG_Y, Vec3::X, Vec3::Z),
    ];
    let mut mesh = MeshData::default();
    for (normal, u, v) in faces {
        let base = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
        for (su, sv, uv) in
            [(-1.0, -1.0, (0.0, 1.0)), (1.0, -1.0, (1.0, 1.0)), (1.0, 1.0, (1.0, 0.0)), (-1.0, 1.0, (0.0, 0.0))]
        {
            mesh.positions.push((normal + u * su + v * sv) * half);
            mesh.normals.push(normal);
            mesh.uvs.push(Vec2::new(uv.0, uv.1));
        }
        mesh.indices.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
}

/// A flat rectangle in the XY plane facing +Z, `2 * half.x` wide and `2 * half.y` tall, UVs `(0,0)` top-left.
pub fn quad(half: Vec2) -> MeshData {
    MeshData {
        positions: vec![
            Vec3::new(-half.x, -half.y, 0.0),
            Vec3::new(half.x, -half.y, 0.0),
            Vec3::new(half.x, half.y, 0.0),
            Vec3::new(-half.x, half.y, 0.0),
        ],
        normals: vec![Vec3::Z; 4],
        uvs: vec![Vec2::new(0.0, 1.0), Vec2::new(1.0, 1.0), Vec2::new(1.0, 0.0), Vec2::new(0.0, 0.0)],
        indices: vec![0, 1, 2, 0, 2, 3],
        ..MeshData::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_are_valid_meshes() {
        assert_eq!(cuboid(Vec3::ONE).validate(), Ok(()));
        assert_eq!(quad(Vec2::ONE).validate(), Ok(()));
        assert_eq!(cuboid(Vec3::ONE).triangle_count(), 12);
    }

    #[test]
    fn every_cube_triangle_faces_outward() {
        let cube = cuboid(Vec3::new(1.0, 2.0, 3.0));
        for tri in cube.indices.as_chunks::<3>().0 {
            let p = |i: usize| cube.positions[tri[i] as usize];
            let n = (p(1) - p(0)).cross(p(2) - p(0));
            let centre = (p(0) + p(1) + p(2)) / 3.0;
            assert!(n.dot(centre) > 0.0, "triangle {tri:?} winds inward");
            assert!(
                n.normalize().dot(cube.normals[tri[0] as usize]) > 0.999,
                "winding disagrees with the stored normal"
            );
        }
    }
}
