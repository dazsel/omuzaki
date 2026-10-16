//! `decimate` and `decimate_scene` on synthetic meshes built here: a UV sphere, a grid, a cube. Nothing is a character.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

use std::collections::{BTreeMap, BTreeSet};
use std::f32::consts::PI;

use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_io_api::{IoScene, Mesh, MeshData, MorphTarget, Primitive};
use omuzaki_io_ops::fit_decimate::{DecimateOptions, decimate, decimate_scene};

/// A UV sphere with `stacks` rows and `slices` columns; the seam column and the poles are duplicated vertices, as in
/// a file. `slices * (2 * stacks - 2)` triangles, counter-clockwise from outside.
fn sphere(radius: f32, stacks: u32, slices: u32) -> Primitive {
    let mut mesh = MeshData::default();
    for i in 0..=stacks {
        for j in 0..=slices {
            let (phi, theta) = (PI * i as f32 / stacks as f32, 2.0 * PI * j as f32 / slices as f32);
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.positions.push(n * radius);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(j as f32 / slices as f32, i as f32 / stacks as f32));
        }
    }
    let at = |i: u32, j: u32| i * (slices + 1) + j;
    for i in 0..stacks {
        for j in 0..slices {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            if i > 0 {
                mesh.indices.extend([a, c, b]);
            }
            if i + 1 < stacks {
                mesh.indices.extend([b, c, d]);
            }
        }
    }
    Primitive::new(mesh)
}

/// Two joints, blended by height.
fn skin(p: &mut Primitive) {
    let top = p.mesh.positions.iter().map(|v| v.y).fold(f32::MIN, f32::max);
    p.mesh.joints = vec![[0, 1, 0, 0]; p.mesh.positions.len()];
    p.mesh.weights = p
        .mesh
        .positions
        .iter()
        .map(|v| {
            let w = (0.5 + 0.5 * v.y / top).clamp(0.0, 1.0);
            [w, 1.0 - w, 0.0, 0.0]
        })
        .collect();
}

/// Three sparse targets, each pushing a different region of the surface outwards.
fn morphs(p: &mut Primitive) {
    let radius = p.mesh.positions.iter().map(|v| v.length()).fold(0.0, f32::max);
    p.morph_targets = (0..3)
        .map(|k| {
            let axis = [Vec3::X, Vec3::Y, Vec3::Z][k];
            let deltas = p
                .mesh
                .positions
                .iter()
                .map(|v| if v.normalize().dot(axis) > 0.7 { v.normalize() * 0.05 * radius } else { Vec3::ZERO });
            MorphTarget::new(deltas.collect())
        })
        .collect();
}

/// A flat grid of `w * h` cells in the XY plane, `scale` metres per cell, facing +Z.
fn grid(w: u32, h: u32, scale: Vec2) -> Primitive {
    let mut mesh = MeshData::default();
    for y in 0..=h {
        for x in 0..=w {
            mesh.positions.push(Vec3::new(x as f32 * scale.x, y as f32 * scale.y, 0.0));
            mesh.normals.push(Vec3::Z);
        }
    }
    for y in 0..h {
        for x in 0..w {
            let a = y * (w + 1) + x;
            mesh.indices.extend([a, a + 1, a + w + 2, a, a + w + 2, a + w + 1]);
        }
    }
    Primitive::new(mesh)
}

/// A cube of side 1 whose six faces are each `cells * cells` quads with their own UVs in 0..1 (so every cube edge is a
/// UV seam). Each face's UV axes and normal are right-handed, so every triangle has positive UV area.
fn cube(cells: u32) -> Primitive {
    let mut mesh = MeshData::default();
    for (u, v) in [
        (Vec3::Y, Vec3::Z),
        (Vec3::Z, Vec3::X),
        (Vec3::X, Vec3::Y),
        (Vec3::Z, Vec3::Y),
        (Vec3::X, Vec3::Z),
        (Vec3::Y, Vec3::X),
    ] {
        let n = u.cross(v);
        let base = mesh.positions.len() as u32;
        for j in 0..=cells {
            for i in 0..=cells {
                let (s, t) = (i as f32 / cells as f32, j as f32 / cells as f32);
                mesh.positions.push(n * 0.5 + u * (s - 0.5) + v * (t - 0.5));
                mesh.normals.push(n);
                mesh.uvs.push(Vec2::new(s, t));
            }
        }
        for j in 0..cells {
            for i in 0..cells {
                let a = base + j * (cells + 1) + i;
                mesh.indices.extend([a, a + 1, a + cells + 2, a, a + cells + 2, a + cells + 1]);
            }
        }
    }
    Primitive::new(mesh)
}

fn diagonal(p: &Primitive) -> f32 {
    let (lo, hi) = p
        .mesh
        .positions
        .iter()
        .fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(l, h), v| (l.min(*v), h.max(*v)));
    (hi - lo).length()
}

/// Points on a surface: vertices, triangle centroids and edge midpoints, optionally with morph target `k` applied.
fn samples(p: &Primitive, target: Option<usize>) -> Vec<Vec3> {
    let pos =
        |i: u32| p.mesh.positions[i as usize] + target.map_or(Vec3::ZERO, |k| p.morph_targets[k].positions[i as usize]);
    let mut out: Vec<Vec3> = (0..p.mesh.positions.len() as u32).map(pos).collect();
    for t in p.mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(pos);
        out.extend([(a + b + c) / 3.0, (a + b) / 2.0, (b + c) / 2.0, (c + a) / 2.0]);
    }
    out
}

/// The largest distance from a point of `of` to the nearest of `to`, by a uniform grid of cell `cell`.
fn worst_distance(of: &[Vec3], to: &[Vec3], cell: f32) -> f32 {
    let key = |v: Vec3| (v / cell).floor().as_ivec3().to_array();
    let mut cells: BTreeMap<[i32; 3], Vec<Vec3>> = BTreeMap::new();
    for &v in to {
        cells.entry(key(v)).or_default().push(v);
    }
    of.iter()
        .map(|&v| {
            let [x, y, z] = key(v);
            let near =
                (-1..=1).flat_map(|dx| (-1..=1).flat_map(move |dy| (-1..=1).map(move |dz| [x + dx, y + dy, z + dz])));
            near.filter_map(|k| cells.get(&k)).flatten().map(|w| w.distance(v)).fold(f32::INFINITY, f32::min)
        })
        .fold(0.0, f32::max)
}

fn assert_valid(p: &Primitive) {
    p.mesh.validate().expect("a valid mesh");
    let n = p.mesh.positions.len();
    for t in &p.morph_targets {
        assert_eq!(t.positions.len(), n);
    }
}

fn boundary_positions(p: &Primitive) -> BTreeSet<[u32; 3]> {
    let mut count: BTreeMap<(u32, u32), u32> = BTreeMap::new();
    for t in p.mesh.indices.as_chunks::<3>().0 {
        for e in 0..3 {
            let (a, b) = (t[e], t[(e + 1) % 3]);
            *count.entry((a.min(b), a.max(b))).or_default() += 1;
        }
    }
    let bits = |i: u32| p.mesh.positions[i as usize].to_array().map(f32::to_bits);
    count.into_iter().filter(|(_, c)| *c == 1).flat_map(|((a, b), _)| [bits(a), bits(b)]).collect()
}

#[test]
fn a_sphere_loses_three_quarters_of_its_triangles_and_stays_a_sphere() {
    let mut p = sphere(0.1, 81, 200);
    assert_eq!(p.mesh.triangle_count(), 32_000);
    let original = samples(&p, None);
    let diag = diagonal(&p);
    let report = decimate(&mut p, &DecimateOptions::new(8_000)).unwrap();
    assert_valid(&p);
    assert!(report.reached_target);
    assert!(p.mesh.triangle_count().abs_diff(8_000) <= 2, "{} triangles", p.mesh.triangle_count());
    assert_eq!(report.triangles, (32_000, p.mesh.triangle_count()));
    assert_eq!(report.vertices.1, p.mesh.positions.len());
    assert!(report.max_error <= 0.01);
    let worst = worst_distance(&p.mesh.positions, &original, diag / 20.0);
    assert!(worst <= 0.01 * diag, "{worst} m off the surface");
    for t in p.mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| p.mesh.positions[i as usize]);
        let normal = (b - a).cross(c - a);
        assert!(normal.dot((a + b + c) / 3.0) > 0.0, "a triangle faces inwards");
    }
}

#[test]
fn skin_weights_and_morph_targets_follow_the_collapses() {
    let mut p = sphere(0.1, 81, 200);
    skin(&mut p);
    morphs(&mut p);
    let originals: Vec<Vec<Vec3>> = (0..3).map(|k| samples(&p, Some(k))).collect();
    let diag = diagonal(&p);
    let report = decimate(&mut p, &DecimateOptions::new(8_000)).unwrap();
    assert_valid(&p);
    assert!(report.reached_target);
    assert_eq!(p.morph_targets.len(), 3);
    for (w, j) in p.mesh.weights.iter().zip(&p.mesh.joints) {
        assert!((w.iter().sum::<f32>() - 1.0).abs() < 1e-5, "{w:?}");
        assert!(w.iter().zip(j).all(|(w, j)| *w == 0.0 || *j < 2));
        assert!(w.iter().filter(|w| **w > 0.0).count() <= 4);
    }
    for (k, original) in originals.iter().enumerate() {
        let moved: Vec<Vec3> = samples(&p, Some(k))[..p.mesh.positions.len()].to_vec();
        let worst = worst_distance(&moved, original, diag / 20.0);
        assert!(worst <= 0.02 * diag, "target {k}: {worst} m off");
    }
}

#[test]
fn morph_weight_keeps_a_shape_that_the_base_alone_would_lose() {
    let target = 6_000;
    let (mut with, mut without) = (sphere(0.1, 81, 200), sphere(0.1, 81, 200));
    morphs(&mut with);
    morphs(&mut without);
    let shape = |p: &Primitive| samples(p, Some(0))[..p.mesh.positions.len()].to_vec();
    let reference = shape(&with);
    let options = DecimateOptions { max_error: 1.0, ..DecimateOptions::new(target) };
    decimate(&mut with, &DecimateOptions { morph_weight: 50.0, ..options }).unwrap();
    decimate(&mut without, &DecimateOptions { morph_weight: 0.0, ..options }).unwrap();
    let (a, b) = (worst_distance(&shape(&with), &reference, 0.01), worst_distance(&shape(&without), &reference, 0.01));
    assert!(a <= b, "weighted {a} should not be worse than unweighted {b}");
}

#[test]
fn a_locked_boundary_keeps_its_vertices() {
    let mut p = grid(40, 40, Vec2::splat(0.01));
    let before = boundary_positions(&p);
    let report = decimate(&mut p, &DecimateOptions::new(800)).unwrap();
    assert_valid(&p);
    assert!(report.reached_target);
    assert_eq!(boundary_positions(&p), before);
}

#[test]
fn locked_uv_seams_keep_the_unwrap() {
    let mut p = cube(10);
    let count = p.mesh.triangle_count();
    let report = decimate(&mut p, &DecimateOptions::new(count / 2)).unwrap();
    assert_valid(&p);
    assert!(report.triangles.1 < count, "the faces interiors collapse: {report:?}");
    for uv in &p.mesh.uvs {
        assert!((-1e-6..=1.0 + 1e-6).contains(&uv.x) && (-1e-6..=1.0 + 1e-6).contains(&uv.y), "{uv}");
    }
    for t in p.mesh.indices.as_chunks::<3>().0 {
        let [a, b, c] = t.map(|i| p.mesh.uvs[i as usize]);
        assert!((b - a).perp_dot(c - a) > 0.0, "a triangle's UV area changed sign");
    }
    let seams = |p: &Primitive| {
        p.mesh
            .positions
            .iter()
            .filter(|v| {
                v.abs().max_element() > 0.4999 && (v.abs().cmpgt(Vec3::splat(0.4999)).bitmask().count_ones() >= 2)
            })
            .count()
    };
    assert_eq!(seams(&p), seams(&cube(10)), "no vertex on a cube edge was merged");
}

#[test]
fn a_plain_cube_of_24_vertices_cannot_lose_a_seam() {
    let mut p = cube(1);
    assert_eq!(p.mesh.positions.len(), 24);
    let report = decimate(&mut p, &DecimateOptions::new(6)).unwrap();
    assert_eq!((report.vertices, report.reached_target), ((24, 24), false));
    assert!(p.mesh.uvs.iter().all(|uv| (0.0..=1.0).contains(&uv.x) && (0.0..=1.0).contains(&uv.y)));
}

#[test]
fn a_primitive_under_budget_is_unchanged() {
    let mut p = sphere(0.1, 10, 12);
    skin(&mut p);
    let before = p.clone();
    let count = p.mesh.triangle_count();
    let report = decimate(&mut p, &DecimateOptions::new(count)).unwrap();
    assert!(report.reached_target);
    assert_eq!(report.triangles.0, report.triangles.1);
    assert_eq!(p, before);
}

#[test]
fn two_runs_give_the_same_primitive() {
    let mut base = sphere(0.1, 40, 60);
    skin(&mut base);
    morphs(&mut base);
    let run = || {
        let mut p = base.clone();
        let report = decimate(&mut p, &DecimateOptions::new(1_000)).unwrap();
        (p, report)
    };
    assert_eq!(run(), run());
}

#[test]
fn bad_input_is_an_error_and_changes_nothing() {
    let mut p = sphere(0.1, 10, 12);
    morphs(&mut p);
    p.morph_targets[1].positions.pop();
    let before = p.clone();
    assert!(decimate(&mut p, &DecimateOptions::new(10)).is_err());
    assert!(decimate(&mut p, &DecimateOptions { morph_weight: f32::NAN, ..DecimateOptions::new(10) }).is_err());
    assert!(decimate(&mut p, &DecimateOptions { max_error: -1.0, ..DecimateOptions::new(10) }).is_err());
    assert_eq!(p, before);
}

#[test]
fn a_scene_budget_follows_surface_area_with_a_floor() {
    let mut scene = IoScene::new();
    for (w, h, scale) in
        [(150, 100, Vec2::new(0.01, 0.0066)), (100, 50, Vec2::new(0.03, 0.06)), (10, 8, Vec2::splat(0.001))]
    {
        let mut mesh = Mesh::new("m");
        mesh.primitives.push(grid(w, h, scale));
        scene.add_mesh(mesh);
    }
    let sizes: Vec<usize> = scene.meshes.iter().map(|m| m.primitives[0].mesh.triangle_count()).collect();
    assert_eq!(sizes, [30_000, 10_000, 160]);
    assert!(!scene.check().has_errors());
    let options = DecimateOptions { lock_boundary: false, ..DecimateOptions::new(0) };
    let reports = decimate_scene(&mut scene, 10_000, &options).unwrap();
    assert!(!scene.check().has_errors());
    let after: Vec<usize> = reports.iter().map(|r| r.triangles.1).collect();
    assert!(after.iter().all(|&n| n >= 100));
    assert_eq!(after[2], 100, "the small mesh gets the floor");
    let total: usize = after.iter().sum();
    assert!(total.abs_diff(10_000) <= 100, "{after:?}");
    assert!(after[1] > 2 * after[0], "{after:?}: the larger area gets the larger share");
}

#[test]
fn a_share_a_small_primitive_cannot_use_goes_to_the_others() {
    // A 100-triangle primitive of large area and a 1,900-triangle one of small area, 1,000 triangles in all. By area the
    // first would get 900, but it only has 100: the 800 it cannot use go to the second, so the result keeps the full
    // budget (100 + 900) and not 200.
    let mut scene = IoScene::new();
    for (w, h, scale) in [(10, 5, Vec2::new(1.0, 1.0)), (50, 19, Vec2::new(0.01, 0.01))] {
        let mut mesh = Mesh::new("m");
        mesh.primitives.push(grid(w, h, scale));
        scene.add_mesh(mesh);
    }
    let sizes: Vec<usize> = scene.meshes.iter().map(|m| m.primitives[0].mesh.triangle_count()).collect();
    assert_eq!(sizes, [100, 1_900]);
    let options = DecimateOptions { lock_boundary: false, ..DecimateOptions::new(0) };
    let reports = decimate_scene(&mut scene, 1_000, &options).unwrap();
    let after: Vec<usize> = reports.iter().map(|r| r.triangles.1).collect();
    assert_eq!(after[0], 100, "the primitive that is under its share is left as it is");
    assert!(after[1].abs_diff(900) <= 20, "{after:?}: the second gets what the first could not use");
}

#[test]
fn an_infinite_max_error_is_refused() {
    let mut p = sphere(1.0, 12, 16);
    let before = p.clone();
    let options = DecimateOptions { max_error: f32::INFINITY, ..DecimateOptions::new(100) };
    assert!(decimate(&mut p, &options).is_err());
    assert_eq!(p, before, "an error leaves the primitive untouched");
}

#[test]
fn a_failed_scene_decimation_leaves_the_scene_alone() {
    let mut scene = IoScene::new();
    let mut mesh = Mesh::new("m");
    mesh.primitives.push(grid(20, 20, Vec2::splat(0.01)));
    mesh.primitives.push(grid(20, 20, Vec2::splat(0.01)));
    mesh.primitives[1].mesh.normals.pop();
    scene.add_mesh(mesh);
    let before = scene.clone();
    assert!(decimate_scene(&mut scene, 400, &DecimateOptions::new(0)).is_err());
    assert_eq!(scene, before);
}
