//! The cloth solver suite with the live preset (`SolverConfig::live`): every accuracy, energy, limiter, hostile-input
//! and replay test of `cloth_solver`, so the live config is chosen by the tests and cannot regress unnoticed.

#![allow(clippy::cast_precision_loss)] // small grid indices and frame counts

use omuzaki_char::cloth::solver::SolverConfig;

#[path = "cloth_solver/suite.rs"]
mod suite;

/// The solver config under test.
fn preset() -> SolverConfig {
    SolverConfig::live()
}

/// Writes the skirt sequences of the PR's renders: a 24 x 16 skirt of a stiff and a soft fabric dancing at 60 fps for
/// 4 s (+-30 degrees at 1 Hz and 5 cm of sway, after settling from its cut shape for 1 s), once with a converged
/// reference (8 sub-steps x 4 sweeps), once with the quality preset and once with the live one. Run with
/// `CLOTH_PRESET_OUT=<dir> cargo test -p omuzaki-char --test cloth_live -- --ignored dump_preset_sequences`; one file per
/// fabric and config, a line per frame: time, then every particle's x y z.
#[test]
#[ignore = "writes evidence files; run by hand"]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn dump_preset_sequences() {
    use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, SheetShape};
    use omuzaki_core::glam::{Quat, Vec3};
    use std::f32::consts::TAU;
    use std::fmt::Write as _;

    let dir = std::env::var("CLOTH_PRESET_OUT").expect("set CLOTH_PRESET_OUT to the output directory");
    let (rows, cols) = (16, 24);
    let mut rest = Vec::new();
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        let (radius, drop) = (0.14 + 0.18 * t, -0.42 * t);
        for c in 0..cols {
            let angle = TAU * c as f32 / cols as f32;
            rest.push(Vec3::new(radius * angle.sin(), drop, radius * angle.cos()));
        }
    }
    let pins = (0..rows * cols).map(|i| if i < cols { 1.0 } else { 0.0 }).collect();
    let sheet = ClothSheet::with_density(SheetShape { rows, cols, wrap: true }, rest, 0.2, pins).expect("a skirt");
    let fabrics = [
        ("soft", ClothParams { stretch: 1e-2, shear: 1e-2, ..ClothParams::default() }),
        ("stiff", ClothParams { stretch: 1e-6, shear: 1e-5, bend: 1e2, ..ClothParams::default() }),
    ];
    let configs = [
        ("reference", SolverConfig { substeps: 8, iterations: 4, ..SolverConfig::default() }),
        ("quality", SolverConfig::quality()),
        ("live", SolverConfig::live()),
    ];
    for (fabric, params) in fabrics {
        for (name, config) in configs {
            let mut solver = ClothSolver::new(&sheet, &params, &config).expect("valid");
            let mut state = solver.rest_state();
            let waist: Vec<Vec3> = state.targets().iter().map(|&t| Vec3::from(t)).collect();
            let mut targets = waist.clone();
            let mut out = String::new();
            for frame in 0..=300 {
                let t = frame as f32 / 60.0;
                let dance = (t - 1.0).max(0.0);
                let turn = Quat::from_rotation_y(0.52 * (dance * TAU).sin());
                let sway = Vec3::new(0.05 * (dance * 0.7 * TAU).sin(), 0.0, 0.0);
                for (target, &w) in targets.iter_mut().zip(&waist) {
                    *target = turn * w + sway;
                }
                if frame > 0 {
                    solver.step(1.0 / 60.0, &mut state, &targets).expect("lengths match");
                }
                write!(out, "{t:.4}").expect("string write");
                for x in state.positions() {
                    write!(out, " {:.5} {:.5} {:.5}", x.x, x.y, x.z).expect("string write");
                }
                writeln!(out).expect("string write");
            }
            std::fs::write(format!("{dir}/{fabric}_{name}.txt"), out).expect("write");
        }
    }
}
