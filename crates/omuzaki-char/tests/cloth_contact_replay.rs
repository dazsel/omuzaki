//! Contacts replayed against their recorded history. The contact code was reworked for speed (a chunked broad
//! phase and a cheaper pair rejection) and must give **bit-identical** results, so these scenes' states were hashed
//! with the code before the rework and the hashes are asserted here: four scenes of moving colliders,
//! friction, sweeps and wind, a hash of every particle and velocity each 20 frames. The wind scene also reaches the
//! contact code's `libm` `cbrtf` (a scaled rig collider) and the wind's `libm` `sin` (its gust). A change to the
//! contact maths that is meant to change results must record new hashes and say so in its PR; `CLOTH_PRINT_HASHES=1`
//! prints them.

// The scenes' own sines and cosines go through `libm` too: the platform's `sinf` is not bit-identical across
// platforms (#1585), and these hashes must hold on every one.
#![allow(clippy::expect_used, clippy::unwrap_used, clippy::cast_precision_loss)]

use omuzaki_anim_api::ColliderShape;
use omuzaki_char::cloth::contact::{ClothEnvironment, ContactParams, Shape, Wind, WindParams};
use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, ClothState, SheetShape, SolverConfig};
use omuzaki_core::glam::{Affine3A, Quat, Vec3};

const FRAME: f32 = 1.0 / 60.0;

/// FNV-1a over the bits of every position and velocity, folded into `hash`.
fn fold(mut hash: u64, state: &ClothState) -> u64 {
    for v in state.positions().iter().chain(state.velocities()) {
        for bits in v.to_array().map(f32::to_bits) {
            hash = (hash ^ u64::from(bits)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    hash
}

/// An `n` x `n` sheet `size` wide at height `y`, with the pin weights from `pin`.
fn sheet(n: usize, size: f32, y: f32, pin: impl Fn(usize, usize) -> f32) -> ClothSheet {
    let shape = SheetShape { rows: n, cols: n, wrap: false };
    let coord = |i: usize| (i as f32 / (n - 1) as f32 - 0.5) * size;
    let cells = (0..n).flat_map(|r| (0..n).map(move |c| (r, c)));
    let rest = cells.clone().map(|(r, c)| Vec3::new(coord(c), y, coord(r))).collect();
    let pins = cells.map(|(r, c)| pin(r, c)).collect();
    ClothSheet::with_density(shape, rest, 0.2, pins).unwrap()
}

/// Runs `frames` frames of a scene whose colliders at frame `f` are `shapes(f)`, in `wind` if it is set, hashing every
/// 20th frame.
fn replay(
    sheet: &ClothSheet,
    capacity: usize,
    frames: usize,
    wind: Option<WindParams>,
    shapes: impl Fn(usize) -> Vec<Shape>,
) -> u64 {
    let mut solver = ClothSolver::new(sheet, &ClothParams::default(), &SolverConfig::default()).unwrap();
    let mut state = solver.rest_state();
    let targets: Vec<Vec3> = state.targets().iter().map(|&t| Vec3::from(t)).collect();
    let mut env = ClothEnvironment::new(capacity, ContactParams::default()).unwrap();
    if let Some(params) = wind {
        env.set_wind(Some(Wind::new(sheet.shape(), params).unwrap()));
    }
    for (i, shape) in shapes(0).into_iter().enumerate() {
        env.place(i, shape).unwrap();
    }
    let mut hash = 0xcbf2_9ce4_8422_2325;
    for frame in 1..=frames {
        env.begin_frame();
        for (i, shape) in shapes(frame).into_iter().enumerate() {
            env.place(i, shape).unwrap();
        }
        solver.step_with(FRAME, &mut state, &targets, &mut env).unwrap();
        if frame % 20 == 0 {
            hash = fold(hash, &state);
        }
    }
    hash
}

/// A free sheet dropped onto a capsule and a sphere that slide and swing under it (friction, resting contact).
fn resting_and_dragged() -> u64 {
    let sheet = sheet(25, 0.5, 0.2, |_, _| 0.0);
    replay(&sheet, 2, 600, None, |f| {
        let t = f as f32 * FRAME;
        let slide = 0.08 * libm::sinf(t * 2.0);
        vec![
            Shape::capsule(Vec3::new(-0.3 + slide, 0.0, 0.0), Vec3::new(0.05 + slide, 0.0, 0.0), 0.05),
            Shape::sphere(Vec3::new(0.14, -0.03 + 0.02 * libm::cosf(t * 3.0), 0.0), 0.08),
        ]
    })
}

/// A pinned sheet hit by a ball at 20 m/s (the swept test) and then by a swinging capsule.
fn swept_and_swung() -> u64 {
    let n = 41;
    let sheet = sheet(n, 0.6, 0.0, |r, c| if r == 0 || c == 0 || r == n - 1 || c == n - 1 { 1.0 } else { 0.0 });
    replay(&sheet, 2, 240, None, |f| {
        let t = f as f32 * FRAME;
        let ball_y = (-0.2 + 20.0 * t).min(0.04);
        let angle = -1.2 * libm::cosf(std::f32::consts::TAU * 1.5 * t);
        let pivot = Vec3::new(0.0, -0.3, 0.0);
        let tip = pivot + Vec3::new(libm::sinf(angle), libm::cosf(angle), 0.0) * 0.4;
        vec![Shape::sphere(Vec3::new(0.2, ball_y, 0.1), 0.02), Shape::capsule(pivot, tip, 0.03)]
    })
}

/// A skirt-like wrapped sheet against eight colliders that move through it (the budget scene's shape).
fn skirt_against_legs() -> u64 {
    let (rows, cols) = (16, 24);
    let mut rest = Vec::new();
    for r in 0..rows {
        let t = r as f32 / (rows - 1) as f32;
        for c in 0..cols {
            let angle = std::f32::consts::TAU * c as f32 / cols as f32;
            let radius = 0.14 + 0.18 * t;
            rest.push(Vec3::new(radius * libm::sinf(angle), -0.42 * t, radius * libm::cosf(angle)));
        }
    }
    let pins = (0..rows * cols).map(|i| if i < cols { 1.0 } else { 0.0 }).collect();
    let skirt = ClothSheet::with_density(SheetShape { rows, cols, wrap: true }, rest, 0.2, pins).unwrap();
    replay(&skirt, 8, 300, None, |f| {
        let swing = 0.25 * libm::sinf(f as f32 * FRAME * std::f32::consts::TAU);
        let leg = |x: f32, s: f32| {
            let knee = Vec3::new(x, -0.28, s);
            [
                Shape::capsule(Vec3::new(x, -0.02, 0.0), knee, 0.075),
                Shape::capsule(knee, Vec3::new(x, -0.55, s * 1.8), 0.05),
            ]
        };
        let [a, b] = leg(0.07, swing);
        let [c, d] = leg(-0.07, -swing);
        vec![
            a,
            b,
            c,
            d,
            Shape::capsule(Vec3::new(0.1, 0.0, 0.0), Vec3::new(-0.1, 0.0, 0.0), 0.09),
            Shape::sphere(Vec3::new(0.0, -0.08, -0.05), 0.1),
            Shape::capsule(Vec3::new(0.2, -0.05, 0.2), Vec3::new(0.26, -0.3, 0.2 + swing), 0.04),
            Shape::capsule(Vec3::new(-0.2, -0.05, 0.2), Vec3::new(-0.26, -0.3, 0.2 + swing), 0.04),
        ]
    })
}

/// A 17 x 17 sheet pinned along one edge, in a steady wind across it with the default gust, and a sphere scaled by a
/// non-uniform transform whose determinant is not a cube, so `Shape::from_collider` takes a real `cbrtf` (#1696).
fn gusty_against_a_scaled_collider() -> u64 {
    let sheet = sheet(17, 0.4, 0.0, |r, _| if r == 0 { 1.0 } else { 0.0 });
    let wind = WindParams { velocity: Vec3::new(4.0, 0.0, 0.0), ..WindParams::default() };
    // 1.3 * 0.9 * 1.1 = 1.287, which is not a cube: the radius scales by its cube root.
    let to_solver = Affine3A::from_scale_rotation_translation(
        Vec3::new(1.3, 0.9, 1.1),
        Quat::IDENTITY,
        Vec3::new(0.08, -0.12, 0.0),
    );
    let sphere = ColliderShape::Sphere { offset: Vec3::ZERO, radius: 0.06 };
    replay(&sheet, 1, 300, Some(wind), |_| vec![Shape::from_collider(&sphere, &to_solver).unwrap()])
}

/// The hashes recorded from the portable (`libm`) maths, once, on Linux (#1585). The rework's original hashes came
/// from the platform's C library; the contact code itself is unchanged by the re-record.
const RECORDED: [u64; 4] = [0x23f1_466c_81ab_7ee2, 0x3468_8c20_80fb_5c65, 0x6638_ab0b_96ef_4b6c, 0x6c56_fcf4_9084_20a4];

#[test]
fn contacts_replay_bit_for_bit_against_the_recorded_history() {
    let hashes = [resting_and_dragged(), swept_and_swung(), skirt_against_legs(), gusty_against_a_scaled_collider()];
    if std::env::var("CLOTH_PRINT_HASHES").is_ok() {
        println!("RECORDED = {hashes:#018x?}");
    }
    assert_eq!(hashes, RECORDED, "contact results changed; see the module docs");
}
