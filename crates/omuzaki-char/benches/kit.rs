//! Benchmarks for the geometry kit at the size of a body-part loft: a 129 x 65 grid (8 385 vertices, 16 384 triangles),
//! the largest single surface the prototype builds; and for the curve and sweep helpers at the size of a hair lock or a
//! piping path. Run with `cargo bench -p omuzaki-char --bench kit`.
//!
//! Building runs on every parameter edit, not every frame, so the budget is interactivity: a few milliseconds a part.

#![allow(clippy::expect_used)] // a grid that fails to build should stop the run with its reason

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_anim_api::HumanoidBone;
use omuzaki_char::kit::catmull::spaced_points;
use omuzaki_char::kit::curve::{Pchip, range};
use omuzaki_char::kit::grid::{Grid, GridOptions, MeshBuilder, Skin, grid_normals};
use omuzaki_char::kit::loft::loft_y;
use omuzaki_char::kit::resample::resample;
use omuzaki_char::kit::skin::{CapsuleParams, SegmentSet, Side, Symmetry, mirror, smooth};
use omuzaki_char::kit::sweep::{DEFAULT_UP, SweepOptions, frames, sweep, tube};
use omuzaki_char_api::{JointFit, JointRef, MaterialName, MeshData, PartMesh};
use omuzaki_core::glam::{DVec3, Vec3};

/// A closed vase: radius from a section curve, seam closed in `j`.
fn vase(curve: &Pchip, rows: usize, seg: usize) -> Grid {
    loft_y(&range(0.0, 1.0, rows - 1), seg, |y, phi, _, _| {
        let r = curve.eval(y);
        DVec3::new(r * phi.sin(), y, r * phi.cos())
    })
    .expect("a vase grid is well-formed")
}

fn kit(c: &mut Criterion) {
    let curve = Pchip::new(&[(0.0, 0.2), (0.25, 0.3), (0.5, 0.18), (0.75, 0.28), (1.0, 0.1)]).expect("valid knots");
    let inside = |i: usize, _: usize| DVec3::new(0.0, f64::from(u32::try_from(i).unwrap_or(0)) / 128.0, 0.0);
    let options = GridOptions { wrap_j: true, inside: Some(&inside), ..GridOptions::default() };

    c.bench_function("pchip, 5 knots, 1 000 evaluations", |b| {
        b.iter(|| (0..1000).map(|k| curve.eval(f64::from(k) / 1000.0)).sum::<f64>());
    });
    c.bench_function("loft_y, 129 x 65", |b| b.iter(|| vase(black_box(&curve), 129, 64)));

    let grid = vase(&curve, 129, 64);
    c.bench_function("grid_normals, 129 x 65, wrap + inside", |b| {
        b.iter(|| grid_normals(black_box(&grid), &options).expect("valid options"));
    });

    let blend = |_: DVec3, i: usize, _: usize| {
        let t = f32::from(u16::try_from(i).unwrap_or(0)) / 128.0;
        vec![(JointRef::HIPS, 1.0 - t), (JointRef::CHEST, t)]
    };
    let skinned = GridOptions { skin: Skin::Per(&blend), ..options.clone() };
    c.bench_function("MeshBuilder::add_grid, 129 x 65, per-vertex skin", |b| {
        b.iter(|| {
            let mut builder = MeshBuilder::new();
            builder.add_grid(black_box(&grid), &skinned).expect("valid grid");
            builder
        });
    });

    let mut builder = MeshBuilder::new();
    builder.add_grid(&grid, &skinned).expect("valid grid");
    c.bench_function("MeshBuilder::mirrored, 8 385 vertices", |b| b.iter(|| black_box(&builder).mirrored()));
}

/// A helix of `count` points, `per_turn` to a turn: a stand-in for a hair lock or a piping path.
fn helix(count: u16, per_turn: f64) -> Vec<DVec3> {
    (0..count)
        .map(|i| {
            let a = std::f64::consts::TAU * f64::from(i) / per_turn;
            DVec3::new(0.05 * a.cos(), 1.5 - 0.004 * f64::from(i), 0.05 * a.sin())
        })
        .collect()
}

/// Kit II: Catmull-Rom spacing at the size of a hair lock (9 control points to 26) and of a long path, and sweeps at
/// the size of piping along a seam (200 rings of 9 points).
fn kit_paths(c: &mut Criterion) {
    let lock = helix(9, 8.0);
    let long = helix(40, 20.0);
    let path = helix(200, 40.0);

    c.bench_function("spaced_points, 9 control points to 26", |b| {
        b.iter(|| spaced_points(black_box(&lock), 26).expect("a lock is a curve"));
    });
    c.bench_function("spaced_points, 40 control points to 200", |b| {
        b.iter(|| spaced_points(black_box(&long), 200).expect("a path is a curve"));
    });
    c.bench_function("resample, 200 points to 1 000", |b| {
        b.iter(|| resample(black_box(&path), 1000).expect("a path has length"));
    });
    c.bench_function("frames, 200 points", |b| {
        b.iter(|| frames(black_box(&path), DEFAULT_UP).expect("a path has frames"));
    });
    c.bench_function("tube, 200 rings x 9", |b| {
        b.iter(|| tube(black_box(&path), |_| 0.002, 8).expect("a path can be piped"));
    });
    let shape = |t: f64, phi: f64| (1.0 + 0.2 * (2.0 * phi).cos(), 1.0 - 0.1 * t);
    let options = SweepOptions { shape: Some(&shape), ..SweepOptions::default() };
    c.bench_function("sweep with a shape, 200 rings x 9", |b| {
        b.iter(|| sweep(black_box(&path), |t| (0.01, 0.02 * (1.0 - t)), 8, &options).expect("a path can be swept"));
    });
}

/// Auto-weights at the size of the issue's budget: 50 000 vertices against 25 bones, which `cargo bench -p
/// omuzaki-char --bench kit -- skin` reports (authoring time: it runs when a part is built, never per frame, so the budget is
/// 30 ms). Then the weight editors on the 8 385-vertex vase.
fn kit_skin(c: &mut Criterion) {
    // All 55 humanoid joints on a made-up body (a rising chain, spread sideways), so every bone has an axis. The bench
    // weights 25 of them: the size of a fingerless body.
    let joints = HumanoidBone::ALL.iter().enumerate().map(|(i, bone)| {
        let i = f32::from(u8::try_from(i).unwrap_or(0));
        (JointRef::Humanoid(*bone), Vec3::new(0.1 * (i % 5.0) - 0.2, 0.03 * i, 0.02 * ((i * 7.0) % 5.0)))
    });
    let roles: Vec<JointRef> = HumanoidBone::ALL.iter().map(|b| JointRef::Humanoid(*b)).collect();
    let set = SegmentSet::from_fit(&JointFit::new(joints), &roles).expect("finite joints");
    let candidates = &roles[..25];

    // A xorshift cloud in the body's box: no two runs differ.
    let mut state = 0x2545_F491_u32;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        f32::from(u16::try_from(state >> 16).unwrap_or(0)) / 65_536.0
    };
    let positions: Vec<Vec3> = (0..50_000).map(|_| Vec3::new(next() * 0.4 - 0.2, next() * 1.7, next() * 0.1)).collect();
    let n = positions.len();
    let mut cloud = PartMesh {
        name: "cloud".to_owned(),
        geometry: MeshData {
            positions: positions.clone(),
            normals: vec![Vec3::Y; n],
            indices: vec![0, 1, 2],
            ..MeshData::default()
        },
        joint_bones: Vec::new(),
        material: MaterialName::new("m").expect("a slug"),
        casts_shadow: true,
        render_order: 0,
        dynamic: false,
    };
    let params = CapsuleParams::default();
    c.bench_function("skin_mesh, 50 000 vertices x 25 segments", |b| {
        b.iter(|| {
            set.skin_mesh(black_box(&mut cloud), candidates, params).expect("fewer joints than a mesh can index");
        });
    });
    c.bench_function("weights_at, 1 000 points x 25 segments", |b| {
        b.iter(|| {
            positions[..1000]
                .iter()
                .map(|p| set.weights_at(black_box(p.as_dvec3()), candidates, params).len())
                .sum::<usize>()
        });
    });

    let curve = Pchip::new(&[(0.0, 0.2), (0.25, 0.3), (0.5, 0.18), (0.75, 0.28), (1.0, 0.1)]).expect("valid knots");
    let inside = |i: usize, _: usize| DVec3::new(0.0, f64::from(u32::try_from(i).unwrap_or(0)) / 128.0, 0.0);
    let blend = |_: DVec3, i: usize, _: usize| {
        let t = f32::from(u16::try_from(i).unwrap_or(0)) / 128.0;
        vec![(JointRef::HIPS, 1.0 - t), (JointRef::CHEST, t)]
    };
    let options =
        GridOptions { wrap_j: true, inside: Some(&inside), skin: Skin::Per(&blend), ..GridOptions::default() };
    let mut builder = MeshBuilder::new();
    builder.add_grid(&vase(&curve, 129, 64), &options).expect("valid grid");
    let vase_mesh = builder.finish("vase", MaterialName::new("m").expect("a slug")).expect("a vase is a mesh");
    c.bench_function("smooth, 8 385 vertices, 4 passes", |b| {
        b.iter_batched(
            || vase_mesh.clone(),
            |mut mesh| smooth(black_box(&mut mesh), 4),
            criterion::BatchSize::LargeInput,
        );
    });
    c.bench_function("mirror, 8 385 vertices", |b| {
        b.iter_batched(
            || vase_mesh.clone(),
            |mut mesh| mirror(black_box(&mut mesh), Symmetry::from_side(Side::Left)).expect("a positive tolerance"),
            criterion::BatchSize::LargeInput,
        );
    });
}

criterion_group!(benches, kit, kit_paths, kit_skin);
criterion_main!(benches);
