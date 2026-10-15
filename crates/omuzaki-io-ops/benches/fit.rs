//! `decimate` on a 100,000-triangle skinned sphere with 52 sparse morph targets, down to 20,000. Run with
//! `cargo bench -p omuzaki-io-ops --bench fit`; the budget is 5 s and 400 MB peak (issue #1075). The peak resident
//! size is read from `/proc/self/status` after the first run and printed with its time.

#![allow(clippy::cast_precision_loss)]

use std::f32::consts::PI;
use std::hint::black_box;
use std::time::Instant;

use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use omuzaki_core::glam::{Vec2, Vec3};
use omuzaki_io_api::{MeshData, MorphTarget, Primitive};
use omuzaki_io_ops::fit_decimate::{DecimateOptions, decimate};

const STACKS: u32 = 201;
const SLICES: u32 = 250;
const TARGETS: usize = 52;

/// A UV sphere (99,500 triangles) with two joints and sparse targets: target `k` moves the vertices in one band.
fn fixture() -> Primitive {
    let mut mesh = MeshData::default();
    for i in 0..=STACKS {
        for j in 0..=SLICES {
            let (phi, theta) = (PI * i as f32 / STACKS as f32, 2.0 * PI * j as f32 / SLICES as f32);
            let n = Vec3::new(phi.sin() * theta.cos(), phi.cos(), phi.sin() * theta.sin());
            mesh.positions.push(n * 0.1);
            mesh.normals.push(n);
            mesh.uvs.push(Vec2::new(j as f32 / SLICES as f32, i as f32 / STACKS as f32));
            let w = 0.5 + 0.5 * n.y;
            mesh.joints.push([0, 1, 0, 0]);
            mesh.weights.push([w, 1.0 - w, 0.0, 0.0]);
        }
    }
    let at = |i: u32, j: u32| i * (SLICES + 1) + j;
    for i in 0..STACKS {
        for j in 0..SLICES {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i, j + 1), at(i + 1, j + 1));
            if i > 0 {
                mesh.indices.extend([a, c, b]);
            }
            if i + 1 < STACKS {
                mesh.indices.extend([b, c, d]);
            }
        }
    }
    let mut p = Primitive::new(mesh);
    p.morph_targets = (0..TARGETS)
        .map(|k| {
            let band = (k as f32 + 0.5) / TARGETS as f32;
            let deltas = p
                .mesh
                .normals
                .iter()
                .map(|n| if ((0.5 - 0.5 * n.y) - band).abs() < 0.01 { *n * 0.002 } else { Vec3::ZERO });
            MorphTarget::new(deltas.collect())
        })
        .collect();
    p
}

/// The peak resident size in MB, or 0 where `/proc` is not there.
fn peak_mb() -> u64 {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let kb = status
        .lines()
        .find_map(|l| l.strip_prefix("VmHWM:"))
        .and_then(|v| v.trim().trim_end_matches("kB").trim().parse::<u64>().ok());
    kb.unwrap_or(0) / 1024
}

fn decimate_100k(c: &mut Criterion) {
    let base = fixture();
    let options = DecimateOptions { max_error: 1.0, ..DecimateOptions::new(20_000) };
    let mut once = base.clone();
    let start = Instant::now();
    let report = decimate(&mut once, &options);
    println!("decimate 100k -> 20k, 52 targets: {:?}, peak {} MB, {report:?}", start.elapsed(), peak_mb());
    let mut group = c.benchmark_group("fit_decimate");
    group.sample_size(10);
    group.bench_function("100k to 20k triangles, 52 sparse morph targets", |b| {
        b.iter_batched(|| base.clone(), |mut p| black_box(decimate(&mut p, &options)), BatchSize::LargeInput);
    });
    group.finish();
}

criterion_group!(benches, decimate_100k);
criterion_main!(benches);
