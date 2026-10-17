//! Cloth quality modes (#198): Offline replays frame-exactly, the settle cache scrubs to the sequential result, and
//! Offline adds detail without changing behaviour. On the synthetic skirt of `cloth_runtime/skirt.rs`.

#![allow(clippy::unwrap_used, clippy::cast_precision_loss, clippy::cast_possible_truncation)]

#[path = "cloth_runtime/skirt.rs"]
mod skirt;

use omuzaki_char::cloth::quality::{SettleCache, solver_config};
use omuzaki_char::cloth::runtime::{ClothRuntime, ClothSpec};
use omuzaki_char::cloth::solver::SolverConfig;
use omuzaki_char_api::{PartRuntime, Quality, StepCtx};
use omuzaki_core::glam::Vec3;
use omuzaki_core::time::Seconds;
use skirt::{Pose, leg_colliders, skirt_grid};

/// A render's frame rate.
const FPS: f64 = 24.0;

fn offline() -> ClothRuntime {
    let spec = ClothSpec { quality: Quality::Offline, ..ClothSpec::default() };
    ClothRuntime::new(&skirt_grid(), &spec, leg_colliders()).unwrap()
}

fn bytes(rt: &ClothRuntime) -> Vec<u32> {
    let mesh = rt.dynamic_mesh(0).unwrap();
    mesh.positions.iter().chain(mesh.normals).flat_map(|p| p.to_array().map(f32::to_bits)).collect()
}

/// Settles at the walk's first pose, then renders `frames` frames of it clocked in sub-steps, in a 2 m/s breeze;
/// calls `each` after every frame.
fn render(
    rt: &mut ClothRuntime,
    cache: &mut SettleCache,
    frames: u32,
    mut each: impl FnMut(u32, &ClothRuntime, &mut SettleCache),
) {
    cache.settle(rt, &Pose::walk(0.0), 1.0);
    for frame in 1..=frames {
        let t = f64::from(frame) / FPS;
        let substep = rt.substep_at(t);
        rt.advance_to(substep, &Pose::walk(t as f32), Vec3::new(2.0, 0.0, 0.0)).unwrap();
        each(frame, rt, cache);
    }
}

#[test]
fn two_offline_renders_give_identical_bytes() {
    let run = || {
        let mut rt = offline();
        let mut frames = Vec::new();
        render(&mut rt, &mut SettleCache::new(0), 48, |_, rt, _| frames.push(bytes(rt)));
        frames
    };
    let (a, b) = (run(), run());
    assert_eq!(a.len(), 48);
    assert!(a == b, "two offline renders differ");
    // The clock is in whole sub-steps: frame 48 at 24 fps is 2 s of 480 Hz sub-steps.
    let mut rt = offline();
    render(&mut rt, &mut SettleCache::new(0), 48, |_, _, _| {});
    assert_eq!(rt.clock(), 2 * 480);
}

#[test]
fn scrubbing_from_the_cache_equals_the_sequential_render() {
    let mut cache = SettleCache::new(8);
    let mut rt = offline();
    let mut sequential = Vec::new();
    render(&mut rt, &mut cache, 40, |frame, rt, cache| {
        sequential.push(rt.dynamic_mesh(0).unwrap().positions.to_vec());
        if frame % 10 == 0 {
            cache.checkpoint(rt);
        }
    });
    assert_eq!(cache.len(), 4);
    // Scrub to frame 37: restore the nearest checkpoint (frame 30), advance seven frames.
    let mut scrubbed = offline();
    let target = scrubbed.substep_at(37.0 / FPS);
    let from = cache.scrub_to(&mut scrubbed, target).unwrap();
    assert_eq!(from, scrubbed.substep_at(30.0 / FPS));
    for frame in 31..=37 {
        let t = f64::from(frame) / FPS;
        scrubbed.advance_to(scrubbed.substep_at(t), &Pose::walk(t as f32), Vec3::new(2.0, 0.0, 0.0)).unwrap();
    }
    let worst = scrubbed
        .dynamic_mesh(0)
        .unwrap()
        .positions
        .iter()
        .zip(&sequential[36])
        .map(|(p, q)| p.distance(*q))
        .fold(0.0, f32::max);
    println!("scrub to frame 37 vs sequential: {worst:e} m");
    assert!(worst <= 1e-5, "{worst} m");
    // Back to the start: the settled state is a hit, and frame 1 matches.
    let mut again = offline();
    assert!(cache.settle(&mut again, &Pose::walk(0.0), 1.0), "the settled state is cached");
    again.advance_to(again.substep_at(1.0 / FPS), &Pose::walk((1.0 / FPS) as f32), Vec3::new(2.0, 0.0, 0.0)).unwrap();
    assert_eq!(again.dynamic_mesh(0).unwrap().positions, sequential[0].as_slice());
    // The clock refuses to run backwards; a snapshot of another garment is refused.
    assert!(again.advance_to(0, &Pose::default(), Vec3::ZERO).is_err());
    let live = skirt::skirt();
    assert!(again.restore(&live.snapshot()).is_err());
}

/// The hem's centroid and its mean radius about it: where the hem is and how far it flares, the behaviour a quality
/// mode must keep (the folds' placement is detail and differs with resolution).
fn hem_shape(p: &[Vec3], hem: &[usize]) -> (Vec3, f32) {
    let centroid = hem.iter().map(|&v| p[v]).sum::<Vec3>() / hem.len() as f32;
    let radius = hem.iter().map(|&v| (p[v] - centroid).length()).sum::<f32>() / hem.len() as f32;
    (centroid, radius)
}

/// The walk at both qualities: Offline's finer sheet adds detail but must not change how the skirt moves. Contact
/// with swinging legs makes a walk chaotic (the folds fall at other places, and even two Live sheets one row apart
/// drift a centimetre apart frame by frame), so behaviour is compared over whole walk cycles: the hem's mean centroid
/// and mean flare (radius about the centroid) over four cycles stay within 1.5 cm. The frame-by-frame RMS is printed.
#[test]
fn offline_and_live_differ_by_at_most_1_5_cm_at_the_hem_in_a_walk() {
    let grid = skirt_grid();
    let hem: Vec<usize> = (0..grid.cols).map(|c| grid.index(grid.rows - 1, c)).collect();
    let mut live = skirt::skirt();
    let mut off = offline();
    for rt in [&mut live, &mut off] {
        rt.reset(&Pose::walk(0.0));
        rt.settle(&Pose::walk(0.0), 1.5);
    }
    let frames = 240;
    let (mut mean_a, mut mean_b) = ((Vec3::ZERO, 0.0_f32), (Vec3::ZERO, 0.0_f32));
    let mut rms = (0.0_f32, 0.0_f32);
    for frame in 1..=frames {
        let t = frame as f32 / 60.0;
        let pose = Pose::walk(t);
        let ctx = StepCtx::new(1.0 / 60.0, Seconds(f64::from(t)), &pose, Vec3::ZERO);
        live.step(&ctx);
        off.step(&ctx);
        let (a, b) = (live.dynamic_mesh(0).unwrap().positions, off.dynamic_mesh(0).unwrap().positions);
        let ((ca, ra), (cb, rb)) = (hem_shape(a, &hem), hem_shape(b, &hem));
        mean_a = (mean_a.0 + ca / frames as f32, mean_a.1 + ra / frames as f32);
        mean_b = (mean_b.0 + cb / frames as f32, mean_b.1 + rb / frames as f32);
        rms = (rms.0 + ca.distance_squared(cb) / frames as f32, rms.1 + (ra - rb).powi(2) / frames as f32);
    }
    let (centroid, radius) = (mean_a.0.distance(mean_b.0), (mean_a.1 - mean_b.1).abs());
    println!(
        "walk, Offline vs Live at the hem over four cycles: mean centroid {:.1} mm, mean flare {:.1} mm (frame by frame RMS: {:.1} mm, {:.1} mm)",
        centroid * 1e3,
        radius * 1e3,
        rms.0.sqrt() * 1e3,
        rms.1.sqrt() * 1e3
    );
    assert!(centroid <= 0.015, "the hems' mean centroids differ by {centroid} m");
    assert!(radius <= 0.015, "the hems' mean flares differ by {radius} m");
}

#[test]
fn live_cost_is_unchanged() {
    assert_eq!(solver_config(Quality::Live), SolverConfig::live());
    let rt = skirt::skirt();
    assert_eq!(rt.solver_config(), SolverConfig::live());
    assert_eq!(rt.cost().particles, 24 * 16, "Live steps the sheet as given");
    assert_eq!(offline().cost().particles, 48 * 31);
}
