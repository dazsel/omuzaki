//! Splitting a mesh along a level of a scalar field, so a layer's sharp edge (a plane's or an ellipsoid's mask) is as
//! crisp as its softness says rather than as wide as the triangles it crosses: vertex colours interpolate across a
//! triangle, so an edge through a 2 cm triangle would otherwise blur over 2 cm and step from one triangle to the next.
//!
//! A pass cuts every triangle the level crosses into a triangle and a quad. Where the level meets an edge depends only
//! on the edge's two ends, so the triangles either side of it share the new vertex and the mesh stays watertight (no
//! T-junctions); vertices at a seam (the same position with two texture coordinates) are cut at the same point. The
//! shape doesn't change: every new vertex lies on an old edge, with its attributes interpolated along it.

#![allow(clippy::many_single_char_names)] // a mesh's vertices, edges and fractions: the geometry's own names

use std::collections::HashMap;

use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;

/// How close to an end of an edge a crossing snaps to that end, as a fraction of the edge: no sliver triangles.
const SNAP: f32 = 0.04;
/// Bisection steps to find a crossing along an edge (an edge of 2 cm to a fifth of a micrometre).
const STEPS: usize = 16;
/// The most vertices a mesh may grow to: a split never makes a mesh the renderer's 32-bit indices can't hold.
const MAX_VERTICES: usize = 4_000_000;

/// Cuts `mesh` along `field = level`. `field` is read at vertex positions and along edges; a triangle whose corners
/// all lie on one side (a corner exactly on the level counts as above it) is kept as it is.
pub fn split_along(mesh: &mut MeshData, field: &dyn Fn(Vec3) -> f32, level: f32) {
    let values: Vec<f32> = mesh.positions.iter().map(|p| field(*p) - level).collect();
    let above = |v: f32| v >= 0.0;
    let mut cuts: HashMap<(u32, u32), u32> = HashMap::new();
    let mut indices = Vec::with_capacity(mesh.indices.len());
    let triangles: Vec<[u32; 3]> = mesh.indices.as_chunks::<3>().0.to_vec();
    for [a, b, c] in triangles {
        let side = [a, b, c].map(|i| above(values[i as usize]));
        if side[0] == side[1] && side[1] == side[2] || mesh.positions.len() >= MAX_VERTICES {
            indices.extend([a, b, c]);
            continue;
        }
        // Rotate (keeping the winding) so the corner alone on its side comes first.
        let [p, q, r] = if side[1] == side[2] {
            [a, b, c]
        } else if side[0] == side[2] {
            [b, c, a]
        } else {
            [c, a, b]
        };
        let pq = cut(mesh, &mut cuts, field, level, &values, p, q);
        let pr = cut(mesh, &mut cuts, field, level, &values, p, r);
        for t in [[p, pq, pr], [pq, q, r], [pq, r, pr]] {
            if t[0] != t[1] && t[1] != t[2] && t[0] != t[2] {
                indices.extend(t);
            }
        }
    }
    mesh.indices = indices;
}

/// The vertex where the level crosses the edge `a`–`b`: made (or found, for the triangle on the edge's other side), or
/// an end of the edge when the crossing is within [`SNAP`] of it.
fn cut(
    mesh: &mut MeshData,
    cuts: &mut HashMap<(u32, u32), u32>,
    field: &dyn Fn(Vec3) -> f32,
    level: f32,
    values: &[f32],
    a: u32,
    b: u32,
) -> u32 {
    let key = (a.min(b), a.max(b));
    if let Some(&v) = cuts.get(&key) {
        return v;
    }
    // Always walk the edge from its lower index, so both triangles on it find the same point.
    let (lo, hi) = key;
    // Both ends are vertices the pass started with (a pass cuts only the triangles it was given).
    let (p0, p1) = (mesh.positions[lo as usize], mesh.positions[hi as usize]);
    let lo_above = values[lo as usize] >= 0.0;
    let (mut t0, mut t1) = (0.0f32, 1.0f32);
    for _ in 0..STEPS {
        let mid = f32::midpoint(t0, t1);
        if (field(p0.lerp(p1, mid)) - level >= 0.0) == lo_above {
            t0 = mid;
        } else {
            t1 = mid;
        }
    }
    let t = f32::midpoint(t0, t1);
    let v = if t < SNAP {
        lo
    } else if t > 1.0 - SNAP {
        hi
    } else {
        push_between(mesh, lo as usize, hi as usize, t)
    };
    cuts.insert(key, v);
    v
}

/// Adds a vertex `t` of the way from vertex `a` to vertex `b`, every attribute interpolated, and returns its index.
fn push_between(mesh: &mut MeshData, a: usize, b: usize, t: f32) -> u32 {
    let index = u32::try_from(mesh.positions.len()).unwrap_or(u32::MAX);
    let p = mesh.positions[a].lerp(mesh.positions[b], t);
    mesh.positions.push(p);
    let n = mesh.normals[a].lerp(mesh.normals[b], t).normalize_or(mesh.normals[a]);
    mesh.normals.push(n);
    if !mesh.uvs.is_empty() {
        let uv = mesh.uvs[a].lerp(mesh.uvs[b], t);
        mesh.uvs.push(uv);
    }
    if !mesh.colours.is_empty() {
        let (ca, cb) = (mesh.colours[a], mesh.colours[b]);
        mesh.colours.push(std::array::from_fn(|k| ca[k] + (cb[k] - ca[k]) * t));
    }
    if !mesh.joints.is_empty() {
        let (joints, weights) =
            blend_influences((mesh.joints[a], mesh.weights[a]), (mesh.joints[b], mesh.weights[b]), t);
        mesh.joints.push(joints);
        mesh.weights.push(weights);
    }
    for attribute in &mut mesh.extra {
        let k = usize::from(attribute.components);
        for c in 0..k {
            let (x, y) = (attribute.data[a * k + c], attribute.data[b * k + c]);
            attribute.data.push(x + (y - x) * t);
        }
    }
    index
}

/// The four strongest influences of `a` and `b` blended `t` of the way to `b`, summing to 1.
fn blend_influences(a: ([u16; 4], [f32; 4]), b: ([u16; 4], [f32; 4]), t: f32) -> ([u16; 4], [f32; 4]) {
    let mut sums: Vec<(u16, f32)> = Vec::with_capacity(8);
    for ((joints, weights), share) in [(a, 1.0 - t), (b, t)] {
        for (j, w) in joints.iter().zip(weights.iter()) {
            if *w <= 0.0 {
                continue;
            }
            match sums.iter_mut().find(|(k, _)| k == j) {
                Some(entry) => entry.1 += w * share,
                None => sums.push((*j, w * share)),
            }
        }
    }
    // Strongest first; equal weights by joint, so the result doesn't depend on the order they came in.
    sums.sort_by(|x, y| y.1.total_cmp(&x.1).then(x.0.cmp(&y.0)));
    sums.truncate(4);
    let total: f32 = sums.iter().map(|(_, w)| w).sum();
    let mut joints = [0u16; 4];
    let mut weights = [0.0f32; 4];
    for (k, (j, w)) in sums.iter().enumerate() {
        joints[k] = *j;
        weights[k] = if total > 0.0 { w / total } else { 0.0 };
    }
    if total <= 0.0 {
        weights[0] = 1.0;
        joints[0] = a.0[0];
    }
    (joints, weights)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use omuzaki_core::glam::{Vec2, Vec3};
    use omuzaki_core::mesh::MeshData;

    use super::*;

    /// A unit square in the xy plane as two triangles, with colours, texture coordinates and two joints.
    fn square() -> MeshData {
        let positions = vec![Vec3::ZERO, Vec3::X, Vec3::new(1.0, 1.0, 0.0), Vec3::Y];
        MeshData {
            normals: vec![Vec3::Z; 4],
            uvs: positions.iter().map(|p| Vec2::new(p.x, p.y)).collect(),
            colours: vec![[1.0; 4]; 4],
            joints: vec![[0, 0, 0, 0], [1, 0, 0, 0], [1, 0, 0, 0], [0, 0, 0, 0]],
            weights: vec![[1.0, 0.0, 0.0, 0.0]; 4],
            extra: Vec::new(),
            indices: vec![0, 1, 2, 0, 2, 3],
            positions,
        }
    }

    fn area(m: &MeshData) -> f32 {
        m.indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| {
                let [a, b, c] = [t[0], t[1], t[2]].map(|i| m.positions[i as usize]);
                0.5 * (b - a).cross(c - a).z
            })
            .sum()
    }

    #[test]
    fn a_cut_follows_the_level_keeps_the_area_and_the_winding_and_shares_its_vertices() {
        let mut m = square();
        split_along(&mut m, &|p| p.x, 0.3);
        m.validate().unwrap();
        // The line x = 0.3 crosses the bottom, the diagonal and the top: three new vertices, each used by both sides.
        assert_eq!(m.positions.len(), 7);
        assert!(m.positions[4..].iter().all(|p| (p.x - 0.3).abs() < 1e-4));
        assert!((area(&m) - 1.0).abs() < 1e-5, "every triangle still faces +z and the square is whole");
        assert!(m.indices.as_chunks::<3>().0.iter().all(|t| {
            let xs = [t[0], t[1], t[2]].map(|i| m.positions[i as usize].x);
            xs.iter().all(|x| *x <= 0.3 + 1e-4) || xs.iter().all(|x| *x >= 0.3 - 1e-4)
        }));
        // Interpolated attributes: the texture coordinate follows the position, the weights stay normalised.
        for (p, uv) in m.positions.iter().zip(&m.uvs) {
            assert!((p.x - uv.x).abs() < 1e-5 && (p.y - uv.y).abs() < 1e-5);
        }
        for w in &m.weights {
            assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5);
        }
    }

    #[test]
    fn a_level_near_a_corner_snaps_to_it_and_a_level_off_the_mesh_changes_nothing() {
        let mut m = square();
        split_along(&mut m, &|p| p.x, 0.01);
        assert_eq!(m.positions.len(), 4);
        assert_eq!(m.indices.len(), 6);
        let before = m.indices.clone();
        split_along(&mut m, &|p| p.x, 2.0);
        assert_eq!(m.indices, before);
    }

    #[test]
    fn blended_influences_keep_the_four_strongest_and_sum_to_one() {
        let (j, w) = blend_influences(([1, 2, 0, 0], [0.5, 0.5, 0.0, 0.0]), ([3, 4, 5, 1], [0.4, 0.3, 0.2, 0.1]), 0.5);
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-6);
        assert_eq!(j[0], 1, "joint 1 is in both, so it leads");
    }
}
