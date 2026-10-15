//! Cloth particle LOD by on-screen size (#706): pre-built levels, replay under a scripted zoom, the silhouette at each
//! level, no pop at a switch, hysteresis, sleep. (No allocation at a switch: `cloth_runtime.rs` counts a zoom through
//! every level and sleep.) On the synthetic skirt of `cloth_runtime/skirt.rs`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "cloth_runtime/skirt.rs"]
mod skirt;

use omuzaki_char::cloth::runtime::{ClothRuntime, ClothSpec};
use omuzaki_char::cloth::solver::ClothParams;
use omuzaki_char_api::{PartRuntime, StepCtx};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use skirt::{Pose, skirt, skirt_grid};

const FRAME: f32 = 1.0 / 60.0;

/// A dance: hips sway ±23° at 0.8 Hz, legs swing in opposition, a small bounce.
fn dance(t: f32) -> Pose {
    let w = std::f32::consts::TAU * 0.8 * t;
    Pose {
        yaw: 0.4 * w.sin(),
        flex_l: 0.35 * (2.0 * w).sin(),
        flex_r: -0.35 * (2.0 * w).sin(),
        knee_l: 0.3 * (0.5 + 0.5 * (2.0 * w).cos()),
        knee_r: 0.3 * (0.5 - 0.5 * (2.0 * w).cos()),
        drop: 0.03 * (0.5 + 0.5 * (4.0 * w).sin()),
        ..Pose::default()
    }
}

/// A camera that starts close, pulls out past every threshold and off screen, and comes back (pixels per metre).
fn zoom(t: f32) -> f32 {
    1100.0 * (0.5 + 0.5 * (t * 1.3).cos()).powi(2)
}

fn frame_ctx<'a>(pose: &'a Pose, frame: u32, ppm: Option<f32>) -> StepCtx<'a> {
    StepCtx::new(FRAME, Seconds(f64::from(frame) * f64::from(FRAME)), pose, Vec3::new(1.0, 0.0, 0.5))
        .with_pixels_per_metre(ppm)
}

fn positions(rt: &ClothRuntime) -> Vec<Vec3> {
    rt.dynamic_mesh(0).unwrap().positions.to_vec()
}

#[test]
fn a_scripted_zoom_replays_bit_for_bit() {
    let run = || {
        let mut rt = skirt();
        rt.reset(&dance(0.0));
        let mut out = Vec::new();
        let mut seen = [false; 4];
        for frame in 1..=480 {
            let t = frame as f32 * FRAME;
            let pose = dance(t);
            rt.step(&frame_ctx(&pose, frame, Some(zoom(t))));
            seen[if rt.asleep() { 3 } else { rt.level() }] = true;
            out.extend(rt.dynamic_mesh(0).unwrap().positions.iter().flat_map(|p| p.to_array().map(f32::to_bits)));
            out.push(rt.dynamic_mesh(0).unwrap().revision as u32);
        }
        (out, seen)
    };
    let ((a, seen), (b, _)) = (run(), run());
    assert_eq!(seen, [true; 4], "the zoom visits every level and sleep");
    assert!(a == b, "two runs of the same zoom differ");
}

/// The silhouette of the drawn skirt: per grid row, its extent left and right (front view) and front and back (side
/// view), relative to the hips.
fn silhouette(p: &[Vec3], rows: usize, cols: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(rows * 4);
    for r in 0..rows {
        let row = &p[r * cols..(r + 1) * cols];
        let (lo, hi) = row.iter().fold((Vec3::MAX, Vec3::MIN), |(lo, hi), q| (lo.min(*q), hi.max(*q)));
        out.extend([lo.x, hi.x, lo.z, hi.z]);
    }
    out
}

/// The mean silhouette over a dance at each forced level against the finest. A dance makes the cloth chaotic (two fine
/// sheets one row apart differ by 16 mm RMS frame by frame, 3 mm in the mean), so the silhouette is averaged over the
/// dance; on the toon skirt fabric (bending 10x the default) whose A-line holds. The default, softer fabric folds as
/// it hangs and a coarse sheet folds differently: its numbers are printed.
fn dance_silhouettes(params: ClothParams) -> [f64; 3] {
    let grid = skirt_grid();
    let spec = ClothSpec { params, ..ClothSpec::default() };
    let mut runs: Vec<ClothRuntime> = (0..3)
        .map(|level| {
            let mut rt = ClothRuntime::new(&grid, &spec, skirt::leg_colliders()).unwrap();
            rt.force_level(Some(level));
            rt.reset(&dance(0.0));
            rt.settle(&dance(0.0), 1.0);
            rt
        })
        .collect();
    let mut means = vec![vec![0.0_f64; grid.rows * 4]; 3];
    let frames = 300;
    for frame in 1..=frames {
        let t = frame as f32 * FRAME;
        let pose = dance(t);
        for (rt, mean) in runs.iter_mut().zip(&mut means) {
            rt.step(&frame_ctx(&pose, frame, None));
            for (m, v) in mean.iter_mut().zip(silhouette(&positions(rt), grid.rows, grid.cols)) {
                *m += f64::from(v) / f64::from(frames);
            }
        }
    }
    let rms = |i: usize| {
        (means[i].iter().zip(&means[0]).map(|(a, b)| (a - b).powi(2)).sum::<f64>() / means[0].len() as f64).sqrt()
    };
    [0.0, rms(1), rms(2)]
}

#[test]
fn a_dance_at_three_levels_keeps_the_fine_silhouette_within_15_mm_rms() {
    let toon = dance_silhouettes(ClothParams { bend: 2.0e3, ..ClothParams::default() });
    let soft = dance_silhouettes(ClothParams::default());
    println!(
        "dance, mean silhouette RMS vs 24x16: toon 12x8 {:.1} mm, 8x6 {:.1} mm; default fabric {:.1} mm, {:.1} mm",
        toon[1] * 1e3,
        toon[2] * 1e3,
        soft[1] * 1e3,
        soft[2] * 1e3
    );
    assert!(toon[1] <= 0.015 && toon[2] <= 0.015, "{toon:?}");
}

#[test]
fn a_switch_does_not_pop_and_the_threshold_has_hysteresis() {
    let mut rt = skirt();
    rt.reset(&dance(0.0));
    let mut last = positions(&rt);
    let mut level = rt.level();
    let mut moves = Vec::new();
    let mut switches = Vec::new();
    for frame in 1..=360 {
        let t = frame as f32 * FRAME;
        // Pull out from 600 px/m to 150 px/m and back (the skirt is 0.44 m: 264 px to 66 px), dancing.
        let ppm = 375.0 + 225.0 * (t * std::f32::consts::TAU / 3.0).cos();
        let pose = dance(t);
        rt.step(&frame_ctx(&pose, frame, Some(ppm)));
        let p = positions(&rt);
        let moved = p.iter().zip(&last).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
        moves.push(moved);
        if rt.level() != level {
            switches.push((moves.len() - 1, ppm));
            level = rt.level();
        }
        last = p;
    }
    assert!(switches.len() >= 2, "the zoom crosses a threshold both ways: {switches:?}");
    for &(i, ppm) in &switches {
        // The frame of the switch moves no more than its neighbours do, plus a pixel.
        let neighbours = moves[i - 1].max(moves[i + 1]);
        let pixel = 1.0 / ppm;
        assert!(
            moves[i] <= neighbours + pixel,
            "a switch at {ppm} px/m moved {} m, neighbours {neighbours} m",
            moves[i]
        );
    }
    // Hysteresis: hovering ±10% around a threshold (240 px for the skirt's 0.44 m) never switches.
    let mut rt = skirt();
    rt.reset(&Pose::default());
    let height = rt.height();
    let mut seen = std::collections::BTreeSet::new();
    for frame in 1..=240 {
        let px = 240.0 * (1.0 + 0.1 * (frame as f32 * 0.7).sin());
        rt.step(&frame_ctx(&Pose::default(), frame, Some(px / height)));
        seen.insert(rt.level());
    }
    assert_eq!(seen.len(), 1, "flickered between levels {seen:?}");
}

#[test]
fn off_screen_the_sheet_sleeps_holds_its_pose_and_wakes_reset() {
    let mut rt = skirt();
    rt.reset(&dance(0.0));
    for frame in 1..=30 {
        let pose = dance(frame as f32 * FRAME);
        rt.step(&frame_ctx(&pose, frame, Some(800.0)));
    }
    let held = positions(&rt);
    for frame in 31..=60 {
        let pose = dance(frame as f32 * FRAME);
        rt.step(&frame_ctx(&pose, frame, Some(0.0)));
        assert!(rt.asleep());
        assert_eq!(positions(&rt), held, "asleep, the drawn pose is held");
        assert_eq!(rt.cost().particles, 0);
    }
    let pose = dance(61.0 * FRAME);
    rt.step(&frame_ctx(&pose, 61, Some(800.0)));
    assert!(!rt.asleep());
    // Woken: no pop (the fade starts from the held pose), and the sheet starts at rest under the pose.
    let jump = positions(&rt).iter().zip(&held).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    assert_eq!(jump, 0.0, "the frame of the wake draws the held pose");
    // The sheet itself restarted from rest under the pose: its drawn waist row is the skinned one again by the fade's end.
    for frame in 62..=80 {
        let pose = dance(frame as f32 * FRAME);
        rt.step(&frame_ctx(&pose, frame, Some(800.0)));
    }
    let skin = skirt::skinned(&skirt_grid(), &dance(80.0 * FRAME));
    let waist = (0..48).map(|c| positions(&rt)[c].distance(skin[c])).fold(0.0, f32::max);
    assert!(waist < 1e-5, "after the fade the waist is on the skin, {waist} m off");
}
