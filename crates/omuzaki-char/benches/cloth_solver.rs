//! Benchmark: the cloth solver step on the issue's budget case, a 24 x 16 skirt (384 particles, wrapped, waist
//! row pinned) at the default 4 sub-steps x 2 iterations per 1/120 s physics step, with the waist turning so the sheet
//! never settles. Budget: <= 0.10 ms per 1/120 s step (spec §10 gives all physics 0.5 ms a frame). Also the same skirt
//! at 60 and 30 fps (two and four physics steps a frame) and an offline-quality sheet (2x finer, 12 sub-steps x 4
//! iterations). Run with `cargo bench -p omuzaki-char --bench cloth_solver`.

#![allow(clippy::expect_used, clippy::cast_precision_loss)] // a sheet that fails to build should stop the run; small counts

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, SheetShape, SolverConfig};
use omuzaki_core::glam::{Quat, Vec3};

/// A flared skirt: `cols` around (wrapped), `rows` from a 14 cm waist down to a 32 cm hem 42 cm below it.
fn skirt(rows: usize, cols: usize) -> ClothSheet {
    let mut rest = Vec::with_capacity(rows * cols);
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        let radius = 0.14 + 0.18 * t;
        for c in 0..cols {
            let angle = std::f32::consts::TAU * c as f32 / cols as f32;
            rest.push(Vec3::new(radius * angle.sin(), -0.42 * t, radius * angle.cos()));
        }
    }
    let pins = (0..rows * cols).map(|i| if i < cols { 1.0 } else { 0.0 }).collect();
    ClothSheet::with_density(SheetShape { rows, cols, wrap: true }, rest, 0.2, pins).expect("a valid skirt")
}

fn bench_case(c: &mut Criterion, name: &str, sheet: &ClothSheet, config: SolverConfig, dt: f32) {
    let mut solver = ClothSolver::new(sheet, &ClothParams::default(), &config).expect("valid parameters");
    let mut state = solver.rest_state();
    let waist: Vec<Vec3> = state.targets().iter().map(|&t| Vec3::from(t)).collect::<Vec<Vec3>>();
    let mut targets = waist.clone();
    let mut frame = 0_u32;
    c.bench_function(name, |b| {
        b.iter(|| {
            frame = frame.wrapping_add(1);
            // Swing the hips +-30 degrees at 1 Hz.
            let turn = Quat::from_rotation_y(0.52 * (frame as f32 * dt * std::f32::consts::TAU).sin());
            for (target, &rest) in targets.iter_mut().zip(&waist) {
                *target = turn * rest;
            }
            solver.step(black_box(dt), &mut state, &targets).expect("matching lengths")
        });
    });
}

fn cloth(c: &mut Criterion) {
    let live = skirt(16, 24);
    let config = SolverConfig::default();
    bench_case(c, "step 1/120 s, skirt 24x16 (384 particles), 4 sub-steps x 2 iterations", &live, config, 1.0 / 120.0);
    bench_case(c, "step 1/60 s, skirt 24x16, 8 sub-steps x 2 iterations", &live, config, 1.0 / 60.0);
    bench_case(c, "step 1/30 s, skirt 24x16, 16 sub-steps x 2 iterations", &live, config, 1.0 / 30.0);
    let offline = SolverConfig { substeps: 12, iterations: 4, ..SolverConfig::default() };
    bench_case(
        c,
        "step 1/120 s, skirt 48x32 (1536 particles), 12 sub-steps x 4 iterations",
        &skirt(32, 48),
        offline,
        1.0 / 120.0,
    );
    c.bench_function("construct, skirt 24x16", |b| {
        b.iter(|| ClothSolver::new(black_box(&live), &ClothParams::default(), &config));
    });
}

criterion_group!(benches, cloth);
criterion_main!(benches);
