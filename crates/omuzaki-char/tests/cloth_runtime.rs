//! `ClothRuntime` (#197): the render mesh bound to a coarse sheet, skeleton pins, dynamic mesh output; and #124's
//! measurement of the stretch next to the pins. On a synthetic skirt pinned at the waist ([`skirt`] in
//! `cloth_runtime/skirt.rs`).
//!
//! The allocation test counts only the measuring thread's allocations, as `omuzaki-rig/tests/hot_loop.rs` does.

// A counting allocator is the only way to prove "no allocation"; the workspace denies unsafe code by default.
#![allow(unsafe_code, clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]
#![allow(clippy::similar_names, clippy::too_many_arguments, clippy::items_after_statements)] // the evidence rasteriser
#![allow(
    clippy::disallowed_methods,
    reason = "the evidence case writes images to a folder the developer names; the fence is for ops"
)]

#[path = "cloth_runtime/skirt.rs"]
mod skirt;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicUsize, Ordering};

use omuzaki_char::cloth::runtime::{ClothRuntime, ClothSpec, SheetSize};
use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, SheetShape, SolverConfig};
use omuzaki_char_api::{PartRuntime, Quality, StepCtx};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use skirt::{Pose, skinned, skirt, skirt_grid};

struct Counting;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const FRAME: f32 = 1.0 / 60.0;

fn step(rt: &mut ClothRuntime, pose: &Pose, frame: u32) {
    rt.step(&StepCtx::new(FRAME, Seconds(f64::from(frame) * f64::from(FRAME)), pose, Vec3::ZERO));
}

fn positions(rt: &ClothRuntime) -> Vec<Vec3> {
    rt.dynamic_mesh(0).unwrap().positions.to_vec()
}

fn max_gap(a: &[Vec3], b: &[Vec3]) -> f32 {
    a.iter().zip(b).map(|(p, q)| p.distance(*q)).fold(0.0, f32::max)
}

/// How long the hips take to turn 90°, seconds (a brisk turn on the spot).
const TURN_S: f32 = 0.4;

/// A 90° hips turn over [`TURN_S`], eased in and out, held after.
fn turn(t: f32) -> Pose {
    let u = (t / TURN_S).clamp(0.0, 1.0);
    Pose { yaw: std::f32::consts::FRAC_PI_2 * u * u * (3.0 - 2.0 * u), ..Pose::default() }
}

#[test]
fn at_rest_the_output_is_the_skinned_static_mesh() {
    let grid = skirt_grid();
    let mut rt = skirt();
    // Built: the bind pose.
    assert!(max_gap(&positions(&rt), &grid.positions) < 1e-5);
    // Reset under any pose: exactly the skinned mesh, in character space.
    for pose in [Pose { yaw: 1.1, flex_l: 0.4, root: Vec3::new(2.0, 0.1, -3.0), ..Pose::default() }, Pose::squat(0.6)] {
        rt.reset(&pose);
        let gap = max_gap(&positions(&rt), &skinned(&grid, &pose));
        assert!(gap < 1e-5, "reset differs from the skinned mesh by {gap} m");
    }
}

/// Turns the hips 90° under a settled skirt of `params`; returns the hem's peak lag behind the skin and the farthest
/// the hem strays from where it comes to rest, from 0.6 s after the turn on.
fn turn_and_settle(params: ClothParams) -> (f32, f32) {
    let spec = ClothSpec { params, ..ClothSpec::default() };
    let mut rt = ClothRuntime::new(&skirt_grid(), &spec, skirt::leg_colliders()).unwrap();
    rt.reset(&Pose::default());
    rt.settle(&Pose::default(), 3.0);
    let grid = skirt_grid();
    let hem: Vec<usize> = (0..grid.cols).map(|c| grid.index(grid.rows - 1, c)).collect();
    let mut lag_peak = 0.0_f32;
    let mut track = Vec::new();
    for frame in 1..=180 {
        let t = frame as f32 * FRAME;
        step(&mut rt, &turn(t), frame);
        let p = positions(&rt);
        // Lag: how far the hem trails the skinned hem (which turns rigidly with the hips).
        let skin = skinned(&grid, &turn(t));
        lag_peak = lag_peak.max(hem.iter().map(|&v| p[v].distance(skin[v])).fold(0.0, f32::max));
        track.push((t - TURN_S, p));
    }
    let end = &track[track.len() - 1].1;
    let late = track
        .iter()
        .filter(|(s, _)| *s >= 0.6)
        .map(|(_, p)| hem.iter().map(|&v| p[v].distance(end[v])).fold(0.0, f32::max))
        .fold(0.0, f32::max);
    (lag_peak, late)
}

/// The hem trails a 90° hips turn, then settles: from 0.6 s after the turn it stays within 3 cm of where it comes to
/// rest. Measured on a toon skirt fabric that holds its A-line (bending ten times the default's stiffness); the
/// default, softer fabric folds as it hangs, and its folds unwind more slowly (printed, held to 7 cm).
#[test]
fn the_hem_lags_a_hips_turn_then_settles_within_0_6_s() {
    let toon = ClothParams { bend: 2.0e3, ..ClothParams::default() };
    let (lag, late) = turn_and_settle(toon);
    println!(
        "toon skirt: hem lag peak {:.1} mm; from 0.6 s after the turn within {:.1} mm of its rest",
        lag * 1e3,
        late * 1e3
    );
    assert!(lag > 0.005, "the hem should lag the turn, peak {lag} m");
    assert!(late < 0.03, "the hem should settle within 0.6 s, still {late} m from its rest");
    let (lag, late) = turn_and_settle(ClothParams::default());
    println!(
        "default fabric: hem lag peak {:.1} mm; from 0.6 s after the turn within {:.1} mm of its rest",
        lag * 1e3,
        late * 1e3
    );
    assert!(late < 0.07, "the default fabric's hem is still {late} m from its rest");
}

#[test]
fn the_revision_bumps_only_when_positions_change() {
    let mut rt = skirt();
    let pose = Pose::default();
    rt.reset(&pose);
    let r0 = rt.dynamic_mesh(0).unwrap().revision;
    // No time: nothing moves.
    rt.step(&StepCtx::new(0.0, Seconds(0.0), &pose, Vec3::ZERO));
    assert_eq!(rt.dynamic_mesh(0).unwrap().revision, r0);
    // A reset to where it already is changes nothing either.
    rt.reset(&pose);
    assert_eq!(rt.dynamic_mesh(0).unwrap().revision, r0);
    step(&mut rt, &pose, 1);
    let r1 = rt.dynamic_mesh(0).unwrap().revision;
    assert_eq!(r1, r0 + 1, "a step that moves the sheet bumps once");
    // Asleep (off screen): held, no bump.
    rt.step(&StepCtx::new(FRAME, Seconds(0.1), &pose, Vec3::ZERO).with_pixels_per_metre(Some(0.0)));
    assert!(rt.asleep());
    assert_eq!(rt.dynamic_mesh(0).unwrap().revision, r1);
    assert_eq!(rt.cost().particles, 0);
}

#[test]
fn render_vertices_stay_within_1_5_cm_of_the_sheet() {
    let mut rt = skirt();
    rt.reset(&Pose::default());
    let mut surface = Vec::new();
    let mut worst = 0.0_f32;
    for frame in 1..=90 {
        let t = frame as f32 * FRAME;
        step(&mut rt, &turn(t), frame);
        rt.sheet_surface(&mut surface);
        worst = worst.max(max_gap(&positions(&rt), &surface));
    }
    println!("worst render vertex to sheet: {:.2} mm", worst * 1e3);
    assert!(worst < 0.015, "{worst} m");
}

#[test]
fn rows_that_follow_the_skin_never_lag_and_cloth_off_is_the_skinned_mesh() {
    let grid = skirt_grid();
    // Hybrid: the top 8 rows on the skin, then the sheet.
    let follow: Vec<f32> = (0..grid.rows).map(|r| if r < 8 { 0.0 } else { 1.0 }).collect();
    let spec = ClothSpec { follow, ..ClothSpec::default() };
    let mut rt = ClothRuntime::new(&grid, &spec, skirt::leg_colliders()).unwrap();
    // All rows on the skin: what a garment with its cloth off draws.
    let off = ClothSpec { follow: vec![0.0; grid.rows], ..ClothSpec::default() };
    let mut static_rt = ClothRuntime::new(&grid, &off, skirt::leg_colliders()).unwrap();
    rt.reset(&Pose::default());
    static_rt.reset(&Pose::default());
    for frame in 1..=60 {
        let pose = Pose::walk(frame as f32 * FRAME);
        step(&mut rt, &pose, frame);
        step(&mut static_rt, &pose, frame);
        let skin = skinned(&grid, &pose);
        let p = positions(&rt);
        let tight = (0..8 * grid.cols).map(|v| p[v].distance(skin[v])).fold(0.0, f32::max);
        assert!(tight < 1e-5, "frame {frame}: a skin row moved {tight} m off the skin");
        let all = max_gap(&positions(&static_rt), &skin);
        assert!(all < 1e-5, "frame {frame}: cloth off moved {all} m off the skinned mesh");
    }
}

#[test]
fn a_fork_starts_at_the_initial_state() {
    let mut rt = skirt();
    let fresh = positions(&rt);
    rt.reset(&Pose::walk(0.3));
    for frame in 1..=10 {
        step(&mut rt, &Pose::walk(frame as f32 * FRAME), frame);
    }
    let fork = rt.fork().unwrap();
    let mesh = fork.dynamic_mesh(0).unwrap();
    assert_eq!(mesh.positions, fresh.as_slice());
    assert_eq!(mesh.revision, 0);
    assert_eq!(fork.cost(), skirt().cost());
    assert!(fork.heap_bytes() > 100_000, "the runtime reports its real size, {}", fork.heap_bytes());
}

#[test]
fn cost_reports_particles_colliders_and_vertices() {
    let rt = skirt();
    let cost = rt.cost();
    assert_eq!((cost.particles, cost.colliders, cost.vertices), (24 * 16, 5, 48 * 32));
}

#[test]
fn bad_input_is_refused_or_held() {
    let grid = skirt_grid();
    let bad = |spec: ClothSpec| ClothRuntime::new(&grid, &spec, skirt::leg_colliders()).is_err();
    assert!(bad(ClothSpec { levels: vec![], ..ClothSpec::default() }));
    assert!(bad(ClothSpec { levels: vec![SheetSize::new(96, 16)], ..ClothSpec::default() }), "finer than the grid");
    assert!(bad(ClothSpec { pin_rows: 16, ..ClothSpec::default() }));
    assert!(bad(ClothSpec { density: f32::NAN, ..ClothSpec::default() }));
    assert!(bad(ClothSpec { follow: vec![2.0; grid.rows], ..ClothSpec::default() }));
    let mut nan = grid.clone();
    nan.positions[5].x = f32::NAN;
    assert!(ClothRuntime::new(&nan, &ClothSpec::default(), skirt::leg_colliders()).is_err());
    let mut weights = grid.clone();
    weights.skin[3].weights = [0.0; 4];
    assert!(ClothRuntime::new(&weights, &ClothSpec::default(), skirt::leg_colliders()).is_err());

    /// A pose whose matrices are NaN: the runtime keeps the last good ones.
    struct Broken;
    impl omuzaki_char_api::PoseSource for Broken {
        fn skin_matrix(&self, _: &omuzaki_char_api::JointRef) -> Option<omuzaki_core::glam::Mat4> {
            Some(omuzaki_core::glam::Mat4::NAN)
        }
        fn root(&self) -> omuzaki_core::glam::Mat4 {
            omuzaki_core::glam::Mat4::NAN
        }
    }
    let mut rt = skirt();
    for frame in 0..30 {
        rt.step(&StepCtx::new(FRAME, Seconds(0.0), &Broken, Vec3::new(f32::NAN, 0.0, 0.0)));
        rt.step(&StepCtx::new(f32::NAN, Seconds(0.0), &Pose::default(), Vec3::ZERO));
        rt.step(&StepCtx::new(FRAME, Seconds(0.0), &Pose::default(), Vec3::ZERO).with_pixels_per_metre(Some(f32::NAN)));
        assert!(positions(&rt).iter().all(|p| p.is_finite()), "frame {frame}");
    }
}

#[test]
fn a_frame_allocates_nothing_after_warm_up_through_switches_and_sleep() {
    let mut rt = skirt();
    rt.reset(&Pose::default());
    let frame = |rt: &mut ClothRuntime, i: u32| {
        let t = i as f32 * FRAME;
        // A camera that zooms through every level, out of the frame and back.
        let ppm = 900.0 * (0.5 + 0.5 * (t * 2.0).cos());
        let pose = Pose::walk(t);
        let ctx = StepCtx::new(FRAME, Seconds(f64::from(t)), &pose, Vec3::new(1.5, 0.0, 0.0));
        rt.step(&ctx.with_pixels_per_metre(Some(ppm)));
    };
    for i in 0..200 {
        frame(&mut rt, i);
    }
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    MEASURING.with(|m| m.set(true));
    let mut levels = [0_u32; 4];
    for i in 200..800 {
        frame(&mut rt, i);
        levels[if rt.asleep() { 3 } else { rt.level() }] += 1;
    }
    MEASURING.with(|m| m.set(false));
    let allocations = ALLOCATIONS.load(Ordering::Relaxed) - before;
    assert!(levels.iter().all(|&n| n > 0), "every level and sleep visited: {levels:?}");
    assert_eq!(allocations, 0, "600 cloth frames made {allocations} allocations");
}

/// #124: the few-iteration stretch bias next to the pins. A stiff skirt (10x the default tensile stiffness) settles
/// at the live budget and at a converged reference (16 sub-steps x 8 sweeps); the strain of the edges next to the pins
/// is compared. The solver's chain solves of warp and weft (on main since #124 was filed) already remove the bias, so
/// long-range attachments are not added: this test records the numbers and holds them.
#[test]
fn the_stretch_next_to_the_pins_is_unbiased_at_the_live_budget() {
    let (rows, cols) = (16, 24);
    let mut rest = Vec::new();
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        let radius = 0.15 + 0.19 * t;
        for c in 0..cols {
            let a = std::f32::consts::TAU * c as f32 / cols as f32;
            rest.push(Vec3::new(radius * a.sin(), -0.44 * t, radius * a.cos()));
        }
    }
    let pins = (0..rows * cols).map(|i| if i < cols { 1.0 } else { 0.0 }).collect();
    let sheet = ClothSheet::with_density(SheetShape { rows, cols, wrap: true }, rest.clone(), 0.2, pins).unwrap();
    let params = ClothParams { stretch: 1.0e-5, ..ClothParams::default() };
    let strain_at_pins = |config: SolverConfig| {
        let mut solver = ClothSolver::new(&sheet, &params, &config).unwrap();
        let mut state = solver.rest_state();
        let targets: Vec<Vec3> = rest[..cols].to_vec();
        for _ in 0..360 {
            solver.step(1.0 / 120.0, &mut state, &targets).unwrap();
        }
        let x = state.positions();
        (0..cols).map(|c| x[cols + c].distance(x[c]) / rest[cols + c].distance(rest[c]) - 1.0).fold(0.0, f32::max)
    };
    let live = strain_at_pins(SolverConfig::live());
    let converged = strain_at_pins(SolverConfig { substeps: 16, iterations: 8, ..SolverConfig::quality() });
    println!("max strain next to the pins: live {:.4}%, converged {:.4}%", live * 100.0, converged * 100.0);
    assert!(live < 0.01, "the live budget leaves {live} strain at the pins (the limiter is at 0.05)");
    assert!((live - converged).abs() < 0.002, "live {live} vs converged {converged}");
}

#[test]
fn offline_quality_builds_finer_sheets_with_the_same_pins() {
    let grid = skirt_grid();
    let spec = ClothSpec { quality: Quality::Offline, ..ClothSpec::default() };
    let rt = ClothRuntime::new(&grid, &spec, skirt::leg_colliders()).unwrap();
    assert_eq!(rt.binding(0).unwrap().size(), SheetSize::new(48, 31));
    assert_eq!(rt.binding(1).unwrap().size(), SheetSize::new(24, 15));
    // The same grid rows are pinned at both qualities.
    let live = skirt();
    for level in 0..3 {
        let (b_live, b_off) = (live.binding(level).unwrap(), rt.binding(level).unwrap());
        let pinned = |b: &omuzaki_char::cloth::runtime::ClothBinding| b.grid_rows()[b.pin_rows() - 1];
        assert_eq!(pinned(b_live), pinned(b_off), "level {level}");
    }
    assert_eq!((rt.solver_config().substeps, rt.solver_config().iterations), (4, 8));
}

/// Rasterises a drawn grid (`rows x cols`, wrapped) into `image` (`w x h`, RGB) at panel `x0`, orthographic from the
/// front with a 30° turn, lit from the upper left, depth-tested.
#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss, clippy::many_single_char_names)]
fn rasterise(
    image: &mut [u8],
    depth: &mut [f32],
    w: usize,
    h: usize,
    x0: usize,
    panel: usize,
    p: &[Vec3],
    n: &[Vec3],
    rows: usize,
    cols: usize,
) {
    let view = omuzaki_core::glam::Quat::from_rotation_y(0.5);
    let scale = panel as f32 / 0.9;
    let to_screen = |q: Vec3| {
        let v = view * (q - Vec3::new(0.0, 0.8, 0.0));
        (x0 as f32 + panel as f32 * 0.5 + v.x * scale, h as f32 * 0.5 - v.y * scale, v.z)
    };
    let light = Vec3::new(-0.4, 0.7, 0.6).normalize();
    for r in 0..rows - 1 {
        for c in 0..cols {
            let at = |r: usize, c: usize| r * cols + c % cols;
            for tri in [[at(r, c), at(r + 1, c), at(r, c + 1)], [at(r, c + 1), at(r + 1, c), at(r + 1, c + 1)]] {
                let s = tri.map(|i| to_screen(p[i]));
                let normal = (tri.iter().map(|&i| n[i]).sum::<Vec3>()).normalize_or(Vec3::Z);
                let lit = 0.35 + 0.65 * (view * normal).dot(light).abs();
                let colour = [(200.0 * lit) as u8, (90.0 * lit) as u8, (110.0 * lit) as u8];
                let (minx, maxx) =
                    (s.iter().map(|v| v.0).fold(f32::MAX, f32::min), s.iter().map(|v| v.0).fold(f32::MIN, f32::max));
                let (miny, maxy) =
                    (s.iter().map(|v| v.1).fold(f32::MAX, f32::min), s.iter().map(|v| v.1).fold(f32::MIN, f32::max));
                let edge = |a: (f32, f32, f32), b: (f32, f32, f32), x: f32, y: f32| {
                    (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0)
                };
                let area = edge(s[0], s[1], s[2].0, s[2].1);
                if area.abs() < 1e-9 {
                    continue;
                }
                for y in (miny.max(0.0) as usize)..=(maxy.min(h as f32 - 1.0) as usize) {
                    for x in (minx.max(x0 as f32) as usize)..=(maxx.min((x0 + panel) as f32 - 1.0) as usize) {
                        let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
                        let (w0, w1, w2) = (
                            edge(s[1], s[2], px, py) / area,
                            edge(s[2], s[0], px, py) / area,
                            edge(s[0], s[1], px, py) / area,
                        );
                        if w0 < 0.0 || w1 < 0.0 || w2 < 0.0 {
                            continue;
                        }
                        let z = w0 * s[0].2 + w1 * s[1].2 + w2 * s[2].2;
                        let i = y * w + x;
                        if z > depth[i] {
                            depth[i] = z;
                            image[3 * i..3 * i + 3].copy_from_slice(&colour);
                        }
                    }
                }
            }
        }
    }
}

/// Writes panels of drawn meshes side by side as a PPM.
fn write_panels(path: &std::path::Path, meshes: &[(Vec<Vec3>, Vec<Vec3>)], rows: usize, cols: usize) {
    let panel = 384;
    let (w, h) = (panel * meshes.len(), panel);
    let mut image = vec![0x24_u8; w * h * 3];
    let mut depth = vec![f32::MIN; w * h];
    for (k, (p, n)) in meshes.iter().enumerate() {
        rasterise(&mut image, &mut depth, w, h, k * panel, panel, p, n, rows, cols);
    }
    let mut out = format!("P6\n{w} {h}\n255\n").into_bytes();
    out.extend(image);
    std::fs::write(path, out).unwrap();
}

/// The `cloth-runtime` evidence case (`evidence.toml`): the synthetic skirt mid-way through a 90° hips turn, Live and
/// Offline side by side, and the three levels of detail side by side. Garments wire `ClothSpec` to the runtime in a
/// later change, so the skirt is this file's own on its small rig. Writes only when `OMUZAKI_CLOTH_RUNTIME_PPM` names
/// a folder.
#[test]
fn render_the_cloth_runtime_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_CLOTH_RUNTIME_PPM") else { return };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).unwrap();
    let grid = skirt_grid();
    let mid_turn = |mut rt: ClothRuntime| {
        rt.reset(&Pose::default());
        rt.settle(&Pose::default(), 1.5);
        for frame in 1..=15 {
            step(&mut rt, &turn(frame as f32 * FRAME), frame);
        }
        let m = rt.dynamic_mesh(0).unwrap();
        (m.positions.to_vec(), m.normals.to_vec())
    };
    let offline = ClothSpec { quality: Quality::Offline, ..ClothSpec::default() };
    let qualities = [mid_turn(skirt()), mid_turn(ClothRuntime::new(&grid, &offline, skirt::leg_colliders()).unwrap())];
    write_panels(&directory.join("live-offline.ppm"), &qualities, grid.rows, grid.cols);
    let levels: Vec<_> = (0..3)
        .map(|level| {
            let mut rt = skirt();
            rt.force_level(Some(level));
            mid_turn(rt)
        })
        .collect();
    write_panels(&directory.join("levels.ppm"), &levels, grid.rows, grid.cols);
}
