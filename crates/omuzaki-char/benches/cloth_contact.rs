//! Benchmark: the cloth part of spec §10's `physics.step`. The budget scene's skirt, a 24 x 16 sheet (384
//! particles, wrapped, waist pinned; a 48 x 32 one for scale) turns at the hips against eight capsules (thighs, shins,
//! hips, forearms) in a gusty wind, stepped as 30 fps frames (four 120 Hz physics steps, 16 sub-steps of 2 iterations).
//! The budget is 0.5 ms a frame p95 on one E-core for springs, hair and cloth together.
//!
//! Before the criterion cases the bench prints the §10 reading, p50/p95/p99/max over a 3,000-frame replay after a
//! warm-up, so run it pinned: `taskset -c 4 cargo bench -p omuzaki-char --bench cloth_contact` (Linux
//! cannot pick E-cores, so the figure is `reported`; see the PR).

#![allow(clippy::expect_used, clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a sheet that fails to build should stop the run

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use omuzaki_char::cloth::contact::{ClothEnvironment, ContactParams, Shape, Wind, WindParams};
use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, ClothState, SheetShape, SolverConfig};
use omuzaki_core::glam::{Quat, Vec3};

/// A 30 fps frame, the unit of spec §10's `physics.step` (four 120 Hz physics steps).
const FRAME: f32 = 1.0 / 30.0;

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

/// Eight capsules inside and around the skirt: two thighs, two shins, two hips and two swinging forearms.
fn legs(t: f32) -> [Shape; 8] {
    let swing = 0.25 * (t * std::f32::consts::TAU).sin();
    let leg = |x: f32, phase: f32| {
        let s = swing * phase;
        let knee = Vec3::new(x, -0.28, s);
        (
            Shape::capsule(Vec3::new(x, -0.02, 0.0), knee, 0.075),
            Shape::capsule(knee, Vec3::new(x, -0.55, s * 1.8), 0.05),
        )
    };
    let (thigh_l, shin_l) = leg(0.07, 1.0);
    let (thigh_r, shin_r) = leg(-0.07, -1.0);
    let arm = |x: f32| Shape::capsule(Vec3::new(x, -0.05, 0.2), Vec3::new(x * 1.3, -0.3, 0.2 + swing), 0.04);
    [
        thigh_l,
        shin_l,
        thigh_r,
        shin_r,
        Shape::capsule(Vec3::new(0.1, 0.0, 0.0), Vec3::new(-0.1, 0.0, 0.0), 0.09),
        Shape::sphere(Vec3::new(0.0, -0.08, -0.05), 0.1),
        arm(0.2),
        arm(-0.2),
    ]
}

/// The skirt scene: solver, environment and the hips' motion, stepped one frame at a time.
struct Scene {
    solver: ClothSolver,
    state: ClothState,
    env: ClothEnvironment,
    waist: Vec<Vec3>,
    targets: Vec<Vec3>,
    contacts: bool,
    frame: u32,
}

impl Scene {
    fn new(sheet: &ClothSheet, config: SolverConfig, contacts: bool, wind: bool) -> Self {
        let solver = ClothSolver::new(sheet, &ClothParams::default(), &config).expect("valid");
        let state = solver.rest_state();
        let waist: Vec<Vec3> = state.targets().iter().map(|&t| Vec3::from(t)).collect();
        let mut env = ClothEnvironment::new(8, ContactParams::default()).expect("valid parameters");
        if wind {
            let params = WindParams { velocity: Vec3::new(1.5, 0.0, 2.0), ..WindParams::default() };
            env.set_wind(Some(Wind::new(sheet.shape(), params).expect("valid wind")));
        }
        Self { solver, state, env, targets: waist.clone(), waist, contacts, frame: 0 }
    }

    /// One 30 fps frame of the replay.
    fn step(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        let t = self.frame as f32 * FRAME;
        let turn = Quat::from_rotation_y(0.52 * (t * std::f32::consts::TAU).sin());
        for (target, &rest) in self.targets.iter_mut().zip(&self.waist) {
            *target = turn * rest;
        }
        self.env.begin_frame();
        if self.contacts {
            for (i, shape) in legs(t).into_iter().enumerate() {
                self.env.place(i, shape).expect("valid shapes");
            }
        }
        self.solver.step_with(black_box(FRAME), &mut self.state, &self.targets, &mut self.env).expect("lengths match");
    }
}

/// Prints the §10 reading of a scene: per-frame time over a replay, in microseconds.
fn report(name: &str, sheet: &ClothSheet, config: SolverConfig, contacts: bool, wind: bool) {
    let mut scene = Scene::new(sheet, config, contacts, wind);
    for _ in 0..600 {
        scene.step();
    }
    let mut times: Vec<f64> = (0..3000)
        .map(|_| {
            let start = Instant::now();
            scene.step();
            start.elapsed().as_secs_f64() * 1e6
        })
        .collect();
    times.sort_by(f64::total_cmp);
    let at = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
    println!(
        "physics.step[{name}]: p50 {:.0} us, p95 {:.0} us, p99 {:.0} us, max {:.0} us",
        at(0.5),
        at(0.95),
        at(0.99),
        at(1.0)
    );
}

fn bench_case(c: &mut Criterion, name: &str, sheet: &ClothSheet, config: SolverConfig, contacts: bool, wind: bool) {
    report(name, sheet, config, contacts, wind);
    let mut scene = Scene::new(sheet, config, contacts, wind);
    c.bench_function(name, |b| b.iter(|| scene.step()));
}

fn contacts(c: &mut Criterion) {
    let live = SolverConfig::default();
    for (rows, cols) in [(16, 24), (32, 48)] {
        let sheet = skirt(rows, cols);
        let size = format!("skirt {cols}x{rows} ({} particles)", rows * cols);
        bench_case(c, &format!("frame 1/30 s, {size}, no colliders"), &sheet, live, false, false);
        bench_case(c, &format!("frame 1/30 s, {size}, 8 colliders"), &sheet, live, true, false);
        bench_case(c, &format!("frame 1/30 s, {size}, 8 colliders and wind"), &sheet, live, true, true);
    }
    // The solver's quality knobs on the budget skirt, for the choice recorded.
    let sheet = skirt(16, 24);
    for (substeps, iterations) in [(3, 2), (4, 1), (2, 2), (2, 1)] {
        let config = SolverConfig { substeps, iterations, ..live };
        let name =
            format!("frame 1/30 s, skirt 24x16, 8 colliders and wind, {substeps} sub-steps x {iterations} iterations");
        bench_case(c, &name, &sheet, config, true, true);
    }
}

criterion_group!(benches, contacts);
criterion_main!(benches);
