//! The geometry an outline hull needs beyond the smooth normals: where a mesh is open (#819).
//!
//! An inverted hull outlines what its smooth normals push outward past the silhouette. A closed mesh's silhouette is
//! where its normals turn across the view, so the push widens it. An open sheet seen face on has no such place: its
//! edge is a boundary, and pushing a boundary along its normal moves it toward or away from the camera, never past the
//! edge. A single-layer hair ribbon was outlined from neither side.
//!
//! So a double-sided material's hull is drawn twice, once pushed along `+n` and once along `-n`, keeping the faces
//! turned away from the camera of each. Each push also moves every boundary vertex outward in the sheet's own plane by
//! its *rim* vector from [`outline_rims`]. Seen from either side, the copy behind the sheet is wider than the sheet,
//! and it shows as a band around the sheet's edge. The copy in front is turned toward the camera and is not drawn.
//! Every renderer calls the one function, so their lines agree.

use std::collections::HashMap;

use omuzaki_core::glam::Vec3;

use crate::MeshData;

/// Vertices this close (in metres, per axis) are one point when finding boundaries, so a UV seam's duplicated
/// vertices don't read as an open edge. The renderers weld outline normals at the same scale.
const WELD: f32 = 1e-6;

/// The longest a rim vector gets. A corner's rim is mitred, so both of its edges move out by the full width, but a
/// spike (a strand's tip) would otherwise reach far past its own tip.
const MAX_MITRE: f32 = 2.0;

/// Each vertex's rim: zero inside the mesh, and on an open edge the outward direction in the surface's plane.
///
/// - **Which edges are open.** An edge is open when exactly one triangle uses it, after welding the vertices at one
///   point.
/// - **Direction.** Each open edge contributes its outward in-plane normal, which points away from the triangle's
///   third vertex. A boundary point averages the normals of its open edges.
/// - **Length.** The rim is mitred, so each adjoining edge moves out by one width: a straight edge's rim has length 1,
///   and a square corner's has length about 1.41. The length is capped at [`MAX_MITRE`].
///
/// Degenerate triangles are skipped, and so are edges with more than two triangles. A rim is in the mesh's own
/// space; renderers transform it like a tangent and keep its length.
#[must_use]
pub fn outline_rims(mesh: &MeshData) -> Vec<Vec3> {
    let key = |p: Vec3| {
        let q = (p / WELD).round();
        // Finite positions are a validated mesh's; a coordinate past i64 welds with its neighbours, which is harmless.
        #[allow(clippy::cast_possible_truncation)]
        [q.x as i64, q.y as i64, q.z as i64]
    };
    let mut slot_of: HashMap<[i64; 3], usize> = HashMap::with_capacity(mesh.positions.len());
    let slots: Vec<usize> = mesh
        .positions
        .iter()
        .map(|&p| {
            let next = slot_of.len();
            *slot_of.entry(key(p)).or_insert(next)
        })
        .collect();

    // Every edge by its two welded points: how many triangles use it, and the first one's (from, to, opposite).
    let mut edges: HashMap<(usize, usize), (u32, [usize; 3])> = HashMap::with_capacity(mesh.indices.len());
    for tri in mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = tri.map(|i| i as usize);
        let (Some(&sa), Some(&sb), Some(&sc)) = (slots.get(a), slots.get(b), slots.get(c)) else { continue };
        if sa == sb || sb == sc || sc == sa {
            continue;
        }
        for (from, to, opposite) in [(a, b, c), (b, c, a), (c, a, b)] {
            let (sf, st) = (slots[from], slots[to]);
            let entry = edges.entry((sf.min(st), sf.max(st))).or_insert((0, [from, to, opposite]));
            entry.0 += 1;
        }
    }

    // The outward in-plane normal of every open edge, summed per point, and each point's open edges' normals.
    let mut sums = vec![Vec3::ZERO; slot_of.len()];
    let mut outs: Vec<(usize, Vec3)> = Vec::new();
    for &(count, [from, to, opposite]) in edges.values() {
        if count != 1 {
            continue;
        }
        let [p, q, o] = [from, to, opposite].map(|i| mesh.positions[i]);
        let along = q - p;
        let Some(mut out) = along.cross(along.cross(o - p)).try_normalize() else { continue };
        if out.dot(o - p) > 0.0 {
            out = -out;
        }
        for point in [slots[from], slots[to]] {
            sums[point] += out;
            outs.push((point, out));
        }
    }

    // Mitre: the rim's component along each open edge's normal is at least 1, so every edge moves out a full width.
    let directions: Vec<Option<Vec3>> = sums.iter().map(|s| s.try_normalize()).collect();
    let mut nearest = vec![1.0_f32; slot_of.len()];
    for &(point, out) in &outs {
        if let Some(direction) = directions[point] {
            nearest[point] = nearest[point].min(direction.dot(out));
        }
    }
    let rims: Vec<Vec3> = directions
        .iter()
        .zip(&nearest)
        .map(|(direction, &dot)| direction.map_or(Vec3::ZERO, |d| d * (1.0 / dot.max(1.0 / MAX_MITRE))))
        .collect();
    slots.iter().map(|&slot| rims[slot]).collect()
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::Vec2;

    use super::*;

    fn sheet(columns: u32, rows: u32) -> MeshData {
        let mut mesh = MeshData::default();
        for y in 0..=rows {
            for x in 0..=columns {
                mesh.positions.push(Vec3::new(x as f32, y as f32, 0.0));
                mesh.normals.push(Vec3::Z);
                mesh.uvs.push(Vec2::ZERO);
            }
        }
        let at = |x: u32, y: u32| y * (columns + 1) + x;
        for y in 0..rows {
            for x in 0..columns {
                mesh.indices.extend([
                    at(x, y),
                    at(x + 1, y),
                    at(x + 1, y + 1),
                    at(x, y),
                    at(x + 1, y + 1),
                    at(x, y + 1),
                ]);
            }
        }
        mesh
    }

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn a_sheet_is_open_at_its_edges_and_closed_inside() {
        let mesh = sheet(2, 2);
        let rims = outline_rims(&mesh);
        // The centre point (1, 1) is inside; the middle of the left edge points left; a corner is mitred.
        assert_eq!(rims[4], Vec3::ZERO);
        assert!(close(rims[3], Vec3::NEG_X), "{:?}", rims[3]);
        assert!(close(rims[5], Vec3::X), "{:?}", rims[5]);
        assert!(close(rims[1], Vec3::NEG_Y), "{:?}", rims[1]);
        assert!(close(rims[8], Vec3::new(1.0, 1.0, 0.0)), "{:?}", rims[8]);
        assert!(rims.iter().all(|r| r.z.abs() < 1e-6), "a rim stays in the sheet's plane");
    }

    #[test]
    fn a_closed_mesh_has_no_rim() {
        // A tetrahedron: every edge has two triangles.
        let mut mesh = MeshData::default();
        mesh.positions = vec![Vec3::ZERO, Vec3::X, Vec3::Y, Vec3::Z];
        mesh.normals = vec![Vec3::Z; 4];
        mesh.uvs = vec![Vec2::ZERO; 4];
        mesh.indices = vec![0, 2, 1, 0, 1, 3, 0, 3, 2, 1, 2, 3];
        assert!(outline_rims(&mesh).iter().all(|r| *r == Vec3::ZERO));
    }

    #[test]
    fn a_uv_seam_is_not_an_open_edge() {
        // Two squares side by side; the right one uses duplicates of the middle column's vertices, as a UV seam does.
        let plain = sheet(2, 1);
        let mut mesh = plain.clone();
        let (bottom, top) = (mesh.positions.len() as u32, mesh.positions.len() as u32 + 1);
        mesh.positions.extend([mesh.positions[1], mesh.positions[4]]);
        mesh.normals.extend([Vec3::Z; 2]);
        mesh.uvs.extend([Vec2::ONE; 2]);
        for index in &mut mesh.indices[6..] {
            *index = match *index {
                1 => bottom,
                4 => top,
                other => other,
            };
        }
        let (rims, expected) = (outline_rims(&mesh), outline_rims(&plain));
        assert_eq!(rims[..expected.len()], expected[..], "the seam changes no rim");
        assert_eq!((rims[bottom as usize], rims[top as usize]), (expected[1], expected[4]), "a seam's vertices agree");
    }

    #[test]
    fn a_spike_is_capped() {
        let mut mesh = MeshData::default();
        mesh.positions = vec![Vec3::new(-0.01, 0.0, 0.0), Vec3::new(0.01, 0.0, 0.0), Vec3::new(0.0, 1.0, 0.0)];
        mesh.normals = vec![Vec3::Z; 3];
        mesh.uvs = vec![Vec2::ZERO; 3];
        mesh.indices = vec![0, 1, 2];
        let tip = outline_rims(&mesh)[2];
        assert!(tip.y > 0.0 && tip.length() <= MAX_MITRE + 1e-5, "{tip:?}");
    }

    #[test]
    fn degenerate_triangles_are_skipped() {
        let mut mesh = sheet(1, 1);
        mesh.indices.extend([0, 0, 1]);
        assert_eq!(outline_rims(&mesh).len(), mesh.positions.len());
    }
}
