//! Surface nets: a signed-distance field meshed on a grid. Each grid cell the surface passes through gets one vertex
//! (the mean of where the surface crosses its edges, then pulled onto the surface along the field's gradient), and each
//! grid edge the surface crosses gets one quad joining the four cells round it. The mesh is watertight wherever the
//! surface stays inside the box, its vertices sit on the surface, and its normals are the field's gradient, so curved
//! forms shade smoothly at any cell size.
//!
//! Most of a box is far from the surface: the field is first sampled once per block of cells, and a block whose
//! sample is farther from the surface than the block is wide is filled without sampling each point (a distance field
//! changes no faster than distance does).

use omuzaki_core::glam::{UVec3, Vec3};

/// A field: the signed distance at a point, negative inside.
pub trait Field: Fn(Vec3) -> f32 {}
impl<F: Fn(Vec3) -> f32> Field for F {}

/// A meshed surface: positions, unit normals and triangles (counter-clockwise seen from outside).
#[derive(Clone, Debug, Default)]
pub struct Surface {
    /// Vertex positions.
    pub positions: Vec<Vec3>,
    /// Unit normals, pointing out.
    pub normals: Vec<Vec3>,
    /// Triangles, three indices each.
    pub indices: Vec<u32>,
}

/// The field's gradient at `p`, by central differences `h` apart, normalised (zero where it is flat).
pub fn gradient(field: &impl Field, p: Vec3, h: f32) -> Vec3 {
    let (dx, dy, dz) = (Vec3::new(h, 0.0, 0.0), Vec3::new(0.0, h, 0.0), Vec3::new(0.0, 0.0, h));
    Vec3::new(field(p + dx) - field(p - dx), field(p + dy) - field(p - dy), field(p + dz) - field(p - dz))
        .normalize_or_zero()
}

const BLOCK: u32 = 6;
/// A cell's corners, and the corner pairs that are its edges.
const CORNERS: [(u32, u32, u32); 8] =
    [(0, 0, 0), (1, 0, 0), (0, 1, 0), (1, 1, 0), (0, 0, 1), (1, 0, 1), (0, 1, 1), (1, 1, 1)];
const EDGES: [(usize, usize); 12] =
    [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)];

/// Meshes the surface of `field` inside the box `min`–`max` at `cell` metres per grid step.
pub fn mesh(field: &impl Field, min: Vec3, max: Vec3, cell: f32) -> Surface {
    let cell = cell.max(1e-6);
    // In range: a box of a few centimetres at a fraction of a millimetre is a few hundred steps a side.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let n = (((max - min) / cell).ceil().as_uvec3() + UVec3::ONE).max(UVec3::splat(2));
    let values = sample(field, min, cell, n);
    let at = |i: u32, j: u32, k: u32| values[((k * n.y + j) * n.x + i) as usize];
    #[allow(clippy::cast_precision_loss)]
    let point = |i: u32, j: u32, k: u32| min + Vec3::new(i as f32, j as f32, k as f32) * cell;

    // One vertex per cell the surface crosses.
    let cells = n - UVec3::ONE;
    let mut vertex_of = vec![u32::MAX; (cells.x * cells.y * cells.z) as usize];
    let cell_index = |i: u32, j: u32, k: u32| ((k * cells.y + j) * cells.x + i) as usize;
    let mut surface = Surface::default();
    for k in 0..cells.z {
        for j in 0..cells.y {
            for i in 0..cells.x {
                let v: [f32; 8] = CORNERS.map(|(a, b, c)| at(i + a, j + b, k + c));
                let inside = v.iter().filter(|d| **d < 0.0).count();
                if inside == 0 || inside == 8 {
                    continue;
                }
                let mut sum = Vec3::ZERO;
                let mut count = 0.0;
                for (e0, e1) in EDGES {
                    if (v[e0] < 0.0) != (v[e1] < 0.0) {
                        let t = v[e0] / (v[e0] - v[e1]);
                        let (a, b) = (CORNERS[e0], CORNERS[e1]);
                        let pa = point(i + a.0, j + a.1, k + a.2);
                        let pb = point(i + b.0, j + b.1, k + b.2);
                        sum += pa + (pb - pa) * t;
                        count += 1.0;
                    }
                }
                let mut p = sum / count;
                // Pull it onto the surface, but never out of its own cell's neighbourhood.
                let lo = point(i, j, k) - Vec3::splat(0.5 * cell);
                let hi = point(i + 1, j + 1, k + 1) + Vec3::splat(0.5 * cell);
                for _ in 0..2 {
                    let g = gradient(field, p, 0.25 * cell);
                    p = (p - g * field(p)).clamp(lo, hi);
                }
                vertex_of[cell_index(i, j, k)] = u32::try_from(surface.positions.len()).unwrap_or(u32::MAX);
                surface.positions.push(p);
                surface.normals.push(gradient(field, p, 0.25 * cell));
            }
        }
    }

    // One quad per grid edge the surface crosses, joining the four cells that share it.
    let quad = |cs: [(u32, u32, u32); 4], surface: &mut Surface| {
        let ids = cs.map(|(i, j, k)| vertex_of[cell_index(i, j, k)]);
        if ids.contains(&u32::MAX) {
            return;
        }
        let ps = ids.map(|id| surface.positions[id as usize]);
        let ns: Vec3 = ids.iter().map(|id| surface.normals[*id as usize]).sum();
        // Wind it to face out, then split along the shorter diagonal.
        let facing = (ps[1] - ps[0]).cross(ps[2] - ps[0]) + (ps[2] - ps[0]).cross(ps[3] - ps[0]);
        let ids = if facing.dot(ns) >= 0.0 { ids } else { [ids[0], ids[3], ids[2], ids[1]] };
        let ps = ids.map(|id| surface.positions[id as usize]);
        if (ps[0] - ps[2]).length_squared() <= (ps[1] - ps[3]).length_squared() {
            surface.indices.extend_from_slice(&[ids[0], ids[1], ids[2], ids[0], ids[2], ids[3]]);
        } else {
            surface.indices.extend_from_slice(&[ids[0], ids[1], ids[3], ids[1], ids[2], ids[3]]);
        }
    };
    // Grid points strictly inside, so the four cells round each edge exist.
    for k in 0..n.z {
        for j in 0..n.y {
            for i in 0..n.x {
                let here = at(i, j, k) < 0.0;
                let (in_i, in_j, in_k) = (i > 0 && i < cells.x, j > 0 && j < cells.y, k > 0 && k < cells.z);
                if i < cells.x && in_j && in_k && here != (at(i + 1, j, k) < 0.0) {
                    quad([(i, j - 1, k - 1), (i, j, k - 1), (i, j, k), (i, j - 1, k)], &mut surface);
                }
                if j < cells.y && in_i && in_k && here != (at(i, j + 1, k) < 0.0) {
                    quad([(i - 1, j, k - 1), (i, j, k - 1), (i, j, k), (i - 1, j, k)], &mut surface);
                }
                if k < cells.z && in_i && in_j && here != (at(i, j, k + 1) < 0.0) {
                    quad([(i - 1, j - 1, k), (i, j - 1, k), (i, j, k), (i - 1, j, k)], &mut surface);
                }
            }
        }
    }
    surface
}

/// The field at every grid point, skipping blocks far from the surface.
fn sample(field: &impl Field, min: Vec3, cell: f32, n: UVec3) -> Vec<f32> {
    let mut values = vec![0.0f32; (n.x * n.y * n.z) as usize];
    let blocks = (n + UVec3::splat(BLOCK - 1)) / BLOCK;
    #[allow(clippy::cast_precision_loss)]
    let reach = (BLOCK as f32) * cell * 0.5 * 3f32.sqrt() + cell;
    for bk in 0..blocks.z {
        for bj in 0..blocks.y {
            for bi in 0..blocks.x {
                let lo = UVec3::new(bi, bj, bk) * BLOCK;
                let hi = (lo + UVec3::splat(BLOCK)).min(n);
                #[allow(clippy::cast_precision_loss)]
                let centre = min + (lo.as_vec3() + (hi - lo).as_vec3() * 0.5) * cell;
                let d = field(centre);
                let far = d.abs() > reach;
                for k in lo.z..hi.z {
                    for j in lo.y..hi.y {
                        for i in lo.x..hi.x {
                            #[allow(clippy::cast_precision_loss)]
                            let p = min + Vec3::new(i as f32, j as f32, k as f32) * cell;
                            values[((k * n.y + j) * n.x + i) as usize] = if far { d } else { field(p) };
                        }
                    }
                }
            }
        }
    }
    values
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feature::sdf::sphere;

    #[test]
    fn a_sphere_meshes_closed_on_its_surface_with_outward_normals() {
        let r = 0.01;
        let field = |p: Vec3| sphere(p, Vec3::ZERO, r);
        let s = mesh(&field, Vec3::splat(-0.012), Vec3::splat(0.012), 0.0008);
        assert!(s.positions.len() > 500, "{}", s.positions.len());
        for (p, n) in s.positions.iter().zip(&s.normals) {
            assert!((p.length() - r).abs() < 2e-5, "a vertex off the surface: {}", p.length());
            assert!(n.dot(p.normalize()) > 0.99, "a normal not pointing out");
        }
        // Closed: every edge is shared by exactly two triangles, in opposite directions.
        let mut edges = std::collections::HashMap::new();
        for t in s.indices.chunks(3) {
            for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
                *edges.entry((a, b)).or_insert(0) += 1;
            }
        }
        for ((a, b), count) in &edges {
            assert_eq!(*count, 1, "an edge used twice the same way");
            assert!(edges.contains_key(&(*b, *a)), "an open edge");
        }
        // Every triangle faces out.
        for t in s.indices.chunks(3) {
            let (a, b, c) = (s.positions[t[0] as usize], s.positions[t[1] as usize], s.positions[t[2] as usize]);
            assert!((b - a).cross(c - a).dot(a + b + c) > 0.0, "a triangle facing in");
        }
    }
}
