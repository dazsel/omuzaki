//! Cloth contacts and wind against closed forms and under abuse:
//!
//! - **Resting contact:** a sheet dropped on a sphere rests on it within 1 mm, with no jitter or drift over 10 s and
//!   energy that never grows.
//! - **Friction:** a sheet on a tilted capsule slides at a speed that falls with the coefficient, close to the
//!   analytic `g (sin θ - μ cos θ)`, and sticks when `μs > tan θ`.
//! - **Tunnelling:** a collider crossing the sheet at 5 to 40 m/s (and a swinging capsule) never ends up on its far
//!   side, with the particle spacing smaller than the collider.
//! - **Wind:** a pinned flag settles within 10% of the analytic drag balance; lift and gusts act as specified.
//! - **Rig colliders** map into the solver's space; bad shapes and parameters are refused.
//! - **Contact limits (#129):** a 3 mm capsule swung at 5 m/s through 1.9 cm cells crosses no triangle; a sheet
//!   pressed into the crease of two overlapping spheres stays outside both.
//! - **Hostile input and cost:** a 1e6-step fuzz with random colliders and wind finds no NaN; replays are bit-exact;
//!   a step with contacts and wind allocates nothing.
//!
//! `dump_contact_sequence` (ignored) writes the sequences the PR's contact sheet was rendered from.

// Test helpers unwrap: a fixture that fails to build should stop the test with its reason. A counting allocator is the
// only way to prove "no allocation"; it forwards to `System` unchanged.
#![allow(clippy::expect_used, clippy::unwrap_used, unsafe_code)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small counts
#![allow(clippy::many_single_char_names)] // vector geometry
#![allow(
    clippy::disallowed_methods,
    reason = "tests write evidence to a folder the developer names; the fence is for ops"
)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use omuzaki_anim_api::{BoneId, Collider, ColliderShape};
use omuzaki_char::cloth::contact::{ClothEnvironment, ContactError, ContactParams, Friction, Shape, Wind, WindParams};
use omuzaki_char::cloth::solver::{ClothParams, ClothSheet, ClothSolver, ClothState, SheetShape, SolverConfig};
use omuzaki_core::glam::{Affine3A, Quat, Vec3, Vec3A};

struct Counting;

thread_local! {
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method forwards to `System` unchanged; the counter is the only addition.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if MEASURING.try_with(Cell::get).unwrap_or(false) {
            let _ = ALLOCATIONS.try_with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

/// Allocations `f` makes on this thread.
fn allocations_in(f: impl FnOnce()) -> usize {
    let before = ALLOCATIONS.with(Cell::get);
    MEASURING.with(|m| m.set(true));
    f();
    MEASURING.with(|m| m.set(false));
    ALLOCATIONS.with(Cell::get) - before
}

// ---------------------------------------------------------------------------------------------------------------------
// Fixtures.

const DENSITY: f32 = 0.2;
const G: f32 = 9.81;
const FRAME: f32 = 1.0 / 60.0;
/// The skin: cloth half-thickness plus margin, as `ContactParams::default` has them.
const SKIN: f32 = 0.003;

/// A `size` x `size` metre sheet of `n` x `n` particles, rest positions from `place(u, v)` with `u`, `v` in
/// `[-size/2, size/2]`, the pin weight from `pin(row, col)`.
fn sheet(n: usize, size: f32, place: impl Fn(f32, f32) -> Vec3, pin: impl Fn(usize, usize) -> f32) -> ClothSheet {
    let shape = SheetShape { rows: n, cols: n, wrap: false };
    let coord = |i: usize| (i as f32 / (n - 1) as f32 - 0.5) * size;
    let cells = (0..n).flat_map(|r| (0..n).map(move |c| (r, c)));
    let rest = cells.clone().map(|(r, c)| place(coord(c), coord(r))).collect();
    let pins = cells.map(|(r, c)| pin(r, c)).collect();
    ClothSheet::with_density(shape, rest, DENSITY, pins).unwrap()
}

/// A horizontal sheet at height `y` over the origin, no pins.
fn flat(n: usize, size: f32, y: f32) -> ClothSheet {
    sheet(n, size, |u, v| Vec3::new(u, y, v), |_, _| 0.0)
}

fn pinned_border(n: usize) -> impl Fn(usize, usize) -> f32 {
    move |r, c| if r == 0 || c == 0 || r == n - 1 || c == n - 1 { 1.0 } else { 0.0 }
}

struct Rig {
    solver: ClothSolver,
    state: ClothState,
    env: ClothEnvironment,
    targets: Vec<Vec3>,
}

impl Rig {
    fn new(sheet: &ClothSheet, params: ContactParams, capacity: usize) -> Self {
        let cloth = ClothParams { thickness: params.thickness, ..ClothParams::default() };
        let solver = ClothSolver::new(sheet, &cloth, &SolverConfig::default()).unwrap();
        let state = solver.rest_state();
        let targets = state.targets().iter().map(|&t| Vec3::from(t)).collect();
        Self { solver, state, env: ClothEnvironment::new(capacity, params).unwrap(), targets }
    }

    /// Puts the colliders at their first poses without stepping, so they start there rather than appearing there.
    fn start(&mut self, shapes: &[Shape]) {
        self.env.begin_frame();
        for (i, &shape) in shapes.iter().enumerate() {
            self.env.place(i, shape).unwrap();
        }
    }

    /// One frame of `dt` with the colliders ending at `shapes`.
    fn frame(&mut self, dt: f32, shapes: &[Shape]) {
        self.env.begin_frame();
        for (i, &shape) in shapes.iter().enumerate() {
            self.env.place(i, shape).unwrap();
        }
        self.solver.step_with(dt, &mut self.state, &self.targets, &mut self.env).unwrap();
    }

    fn positions(&self) -> Vec<Vec3> {
        self.state.positions().iter().map(|&p| Vec3::from(p)).collect()
    }
}

/// How far inside the bare collider surface (negative: clear of it) the deepest particle is, metres.
fn deepest(positions: &[Vec3], shape: &Shape) -> f32 {
    let ((a, b), r) = (shape.axis(), shape.radius());
    positions.iter().map(|&p| r - distance_to_segment(p, a, b)).fold(f32::MIN, f32::max)
}

fn distance_to_segment(p: Vec3, a: Vec3, b: Vec3) -> f32 {
    let e = b - a;
    let t = if e.length_squared() > 0.0 { ((p - a).dot(e) / e.length_squared()).clamp(0.0, 1.0) } else { 0.0 };
    p.distance(a + e * t)
}

fn assert_finite(state: &ClothState) {
    assert!(state.positions().iter().chain(state.velocities()).all(|v| v.is_finite()), "non-finite state");
}

fn bits(state: &ClothState) -> Vec<u32> {
    state.positions().iter().chain(state.velocities()).flat_map(Vec3A::to_array).map(f32::to_bits).collect()
}

fn centroid(positions: &[Vec3]) -> Vec3 {
    positions.iter().copied().sum::<Vec3>() / positions.len() as f32
}

// ---------------------------------------------------------------------------------------------------------------------
// Resting contact.

#[test]
fn a_sheet_dropped_on_a_sphere_rests_on_it_without_jitter_or_drift() {
    let ball = Shape::sphere(Vec3::ZERO, 0.1);
    let mut rig = Rig::new(&flat(21, 0.4, 0.13), ContactParams::default(), 1);
    // Energy: the impact's transient (elastic energy stored as the sheet is stopped) is not bounded by the energy the
    // drop released, because a position-based contact does not conserve the solver's energy measure; what must hold is
    // that a sheet that has come to rest never gains energy again.
    let (mut worst, mut early_peak, mut late_peak) = (f32::MIN, f64::MIN, f64::MIN);
    let (mut centre_at_5s, mut drift, mut jitter) = (Vec3::ZERO, 0.0_f32, 0.0_f32);
    for frame in 0..600 {
        rig.frame(FRAME, &[ball]);
        assert_finite(&rig.state);
        let positions = rig.positions();
        if frame >= 30 {
            worst = worst.max(deepest(&positions, &ball));
        }
        let energy = rig.solver.energy(&rig.state).total();
        if (120..300).contains(&frame) {
            early_peak = early_peak.max(energy);
        } else if frame >= 300 {
            late_peak = late_peak.max(energy);
        }
        if frame == 300 {
            centre_at_5s = centroid(&positions);
        }
        if frame > 300 {
            drift = drift.max(centroid(&positions).distance(centre_at_5s));
            jitter = jitter.max(rig.state.velocities().iter().map(|v| v.length()).fold(0.0, f32::max));
        }
    }
    assert!(worst <= 0.001, "penetrates the sphere by {worst} m");
    let top = rig.positions().iter().map(|p| p.y).fold(f32::MIN, f32::max);
    assert!((top - (0.1 + SKIN)).abs() < 0.001, "the sheet's apex is at {top}, not on the skin at {}", 0.1 + SKIN);
    assert!(drift < 1e-4, "the settled sheet drifts {drift} m over 5 s");
    assert!(jitter < 0.005, "the settled sheet still moves at {jitter} m/s");
    assert!(
        late_peak <= early_peak + 1e-6,
        "a resting sheet gained energy: {early_peak} J at 2-5 s, {late_peak} J after"
    );
}

#[test]
fn a_sheet_rests_on_a_capsule_and_a_sphere_together() {
    let shapes = [
        Shape::capsule(Vec3::new(-0.3, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), 0.05),
        Shape::sphere(Vec3::new(0.12, -0.03, 0.0), 0.08),
    ];
    let mut rig = Rig::new(&flat(31, 0.5, 0.15), ContactParams::default(), 2);
    for _ in 0..600 {
        rig.frame(FRAME, &shapes);
        assert_finite(&rig.state);
        let positions = rig.positions();
        assert!(shapes.iter().all(|s| deepest(&positions, s) <= 0.001), "penetration");
    }
    let still = rig.state.velocities().iter().map(|v| v.length()).fold(0.0, f32::max);
    assert!(still < 0.005, "the sheet is still moving at {still} m/s after 10 s");
}

// ---------------------------------------------------------------------------------------------------------------------
// Friction.

const SLOPE: f32 = 30.0 * std::f32::consts::PI / 180.0;

/// How far (metres, along the slope) a small sheet laid on a capsule ramp of 30 degrees has slid after `seconds`.
fn slid(friction: Friction, seconds: f32) -> f32 {
    let (down, up) = (Vec3::new(SLOPE.cos(), -SLOPE.sin(), 0.0), Vec3::new(SLOPE.sin(), SLOPE.cos(), 0.0));
    let radius = 0.15;
    let ramp = Shape::capsule(Vec3::ZERO, down * 1.4, radius);
    let middle = down * 0.5 + up * (radius + SKIN + 0.0005);
    // The sheet lies in the ramp's tangent plane: spanned by `down` and the capsule's width (z).
    let sheet = sheet(5, 0.06, |u, v| middle + down * u + Vec3::Z * v, |_, _| 0.0);
    let params = ContactParams { friction, ..ContactParams::default() };
    let mut rig = Rig::new(&sheet, params, 1);
    let start = centroid(&rig.positions());
    for _ in 0..(seconds / FRAME).round() as usize {
        rig.frame(FRAME, &[ramp]);
    }
    assert_finite(&rig.state);
    (centroid(&rig.positions()) - start).dot(down)
}

#[test]
fn sliding_speed_falls_with_friction_and_follows_coulomb() {
    let slide = |mu: f32| slid(Friction { static_: mu, kinetic: mu }, 0.4);
    let (free, light, medium) = (slide(0.0), slide(0.2), slide(0.4));
    assert!(free > light && light > medium && medium > 0.02, "not ordered by friction: {free} {light} {medium}");
    for (mu, got) in [(0.0, free), (0.2, light), (0.4, medium)] {
        // Constant acceleration `a = g (sin θ - μ cos θ)` against the solver's still-air drag `γ` (2/s):
        // `s = a/γ (t - (1 - e^-γt)/γ)`; within 12% (the ramp is round and the sheet bends over it).
        let (a, gamma, t) = (G * (SLOPE.sin() - mu * SLOPE.cos()), 2.0_f32, 0.4_f32);
        let expected = a / gamma * (t - (1.0 - (-gamma * t).exp()) / gamma);
        assert!((got - expected).abs() < 0.12 * expected, "mu {mu}: slid {got} m, Coulomb says {expected} m");
    }
}

#[test]
fn static_friction_holds_a_slope_below_its_angle_and_lets_go_above() {
    // The landing transient may start a slide; with `μk` above `tan θ` it stops again, as Coulomb says.
    let sticks = slid(Friction { static_: 0.7, kinetic: 0.65 }, 1.0);
    assert!(sticks.abs() < 0.002, "tan 30 = 0.577 < 0.7 yet the sheet slid {sticks} m");
    let slips = slid(Friction { static_: 0.5, kinetic: 0.4 }, 1.0);
    assert!(slips > 0.2, "tan 30 = 0.577 > 0.5 yet the sheet only slid {slips} m");
}

#[test]
fn a_moving_collider_drags_the_cloth_by_friction() {
    // A sheet on a long capsule that slides along its own axis: with friction the sheet follows, without it stays.
    let travelled = |mu: f32| {
        let friction = Friction { static_: mu, kinetic: mu };
        let params = ContactParams { friction, ..ContactParams::default() };
        let mut rig = Rig::new(&flat(9, 0.1, 0.1 + SKIN + 0.001), params, 1);
        let beam = |x: f32| Shape::capsule(Vec3::new(x - 1.0, 0.0, 0.0), Vec3::new(x + 1.0, 0.0, 0.0), 0.1);
        rig.start(&[beam(0.0)]);
        for frame in 1..=60 {
            rig.frame(FRAME, &[beam(frame as f32 * FRAME * 0.2)]); // slides along its axis at 0.2 m/s
        }
        centroid(&rig.positions()).x
    };
    let (grippy, slick) = (travelled(1.0), travelled(0.0));
    assert!(grippy > 0.15, "the sheet only followed {grippy} m of 0.2 m");
    assert!(slick.abs() < 0.02, "a frictionless collider dragged the sheet {slick} m");
}

// ---------------------------------------------------------------------------------------------------------------------
// Tunnelling.

#[test]
fn a_fast_collider_never_tunnels_through_the_sheet() {
    for speed in [5.0_f32, 20.0, 40.0] {
        let n = 61;
        let mut rig =
            Rig::new(&sheet(n, 0.3, |u, v| Vec3::new(u, 0.0, v), pinned_border(n)), ContactParams::default(), 1);
        let middle = (n / 2) * n + n / 2;
        let (mut y, mut frames) = (-0.1_f32, 0);
        rig.start(&[Shape::sphere(Vec3::new(0.0, y, 0.0), 0.015)]);
        while y < 0.015 {
            y = (y + speed * FRAME).min(0.015);
            let ball = Shape::sphere(Vec3::new(0.0, y, 0.0), 0.015);
            rig.frame(FRAME, &[ball]);
            assert_finite(&rig.state);
            let positions = rig.positions();
            assert!(deepest(&positions, &ball) <= 0.001, "{speed} m/s: penetrates by {}", deepest(&positions, &ball));
            assert!(
                positions[middle].y > y,
                "{speed} m/s: the ball went through (sheet {} vs ball {y})",
                positions[middle].y
            );
            frames += 1;
        }
        assert!(frames >= 1);
    }
}

#[test]
fn a_ball_crossing_a_resting_sheet_in_one_sub_step_is_caught() {
    // 8 cm of travel per sub-step against a 3 cm ball: only the swept test can see it.
    let n = 41;
    let mut rig = Rig::new(&sheet(n, 0.3, |u, v| Vec3::new(u, 0.0, v), pinned_border(n)), ContactParams::default(), 1);
    let h = 1.0 / 480.0_f32;
    let ball = |y: f32| Shape::sphere(Vec3::new(0.0, y, 0.0), 0.015);
    rig.frame(0.0, &[ball(-0.04)]);
    rig.env.begin_frame();
    rig.env.place(0, ball(0.04)).unwrap();
    let report = rig.solver.advance(1, &mut rig.state, &rig.targets, &mut rig.env).unwrap();
    assert_eq!(report.substeps, 1);
    assert!((0.08 / h) > 30.0);
    let middle = (n / 2) * n + n / 2;
    assert!(Vec3::from(rig.state.positions()[middle]).y > 0.04, "the ball tunnelled");
}

#[test]
fn a_swinging_capsule_stays_out_of_the_sheet_at_arm_speeds() {
    let n = 61;
    let mut rig = Rig::new(&sheet(n, 0.8, |u, v| Vec3::new(u, 0.0, v), pinned_border(n)), ContactParams::default(), 1);
    let (pivot, length, amplitude, hz) = (Vec3::new(0.0, -0.2, 0.0), 0.35, 1.2_f32, 2.5_f32);
    let mut fastest = 0.0_f32;
    let rest_tip = pivot + Vec3::new((-amplitude).sin(), (-amplitude).cos(), 0.0) * length;
    rig.start(&[Shape::capsule(pivot, rest_tip, 0.03)]);
    let mut last: Option<Vec3> = Some(rest_tip);
    for frame in 1..=180 {
        let angle = -amplitude * (std::f32::consts::TAU * hz * frame as f32 * FRAME).cos();
        let tip = pivot + Vec3::new(angle.sin(), angle.cos(), 0.0) * length;
        if let Some(before) = last {
            fastest = fastest.max(tip.distance(before) / FRAME);
        }
        last = Some(tip);
        let bat = Shape::capsule(pivot, tip, 0.03);
        rig.frame(FRAME, &[bat]);
        assert_finite(&rig.state);
        let worst = deepest(&rig.positions(), &bat);
        assert!(worst <= 0.002, "frame {frame}: penetrates by {worst} m");
    }
    assert!(fastest > 5.0, "the swing only reached {fastest} m/s at the tip");
}

// ---------------------------------------------------------------------------------------------------------------------
// Wind.

/// A flag `size` square hanging from its top row in the plane z = 0.
fn flag(n: usize, size: f32) -> ClothSheet {
    sheet(n, size, |u, v| Vec3::new(u, -(v + size / 2.0), 0.0), |r, _| if r == 0 { 1.0 } else { 0.0 })
}

/// The flag's mean angle from the vertical (radians) over the last `tail` frames of `seconds`, and its spread.
fn flag_angle(wind: WindParams, seconds: f32, tail: usize) -> (f32, f32) {
    let sheet = flag(13, 0.5);
    let mut rig = Rig::new(&sheet, ContactParams::default(), 0);
    rig.env.set_wind(Some(Wind::new(sheet.shape(), wind).unwrap()));
    let frames = (seconds / FRAME) as usize;
    let mut angles = Vec::new();
    for frame in 0..frames {
        rig.frame(FRAME, &[]);
        assert_finite(&rig.state);
        if frame >= frames - tail {
            let p = rig.positions();
            let (top, hem) = (centroid(&p[..13]), centroid(&p[12 * 13..]));
            angles.push((hem.z - top.z).atan2(top.y - hem.y));
        }
    }
    let mean = angles.iter().sum::<f32>() / angles.len() as f32;
    let spread = angles.iter().map(|a| (a - mean).abs()).fold(0.0, f32::max);
    (mean, spread)
}

#[test]
fn a_flag_in_steady_wind_settles_at_the_analytic_drag_balance() {
    for (speed, drag) in [(5.0_f32, 0.12_f32), (4.0, 0.25), (8.0, 0.05)] {
        let wind =
            WindParams { velocity: Vec3::new(0.0, 0.0, speed), drag, lift: 0.0, gust: 0.0, ..WindParams::default() };
        let (angle, spread) = flag_angle(wind, 20.0, 120);
        // drag u² cos²θ = ρ g sin θ, so with K = drag u²: K s² + ρ g s - K = 0 for s = sin θ.
        let (k, rho_g) = (f64::from(drag * speed * speed), f64::from(DENSITY * G));
        let expected = ((-rho_g + (rho_g * rho_g + 4.0 * k * k).sqrt()) / (2.0 * k)).asin() as f32;
        assert!(
            (angle - expected).abs() < 0.1 * expected,
            "{speed} m/s, drag {drag}: settles at {:.1} deg, balance says {:.1}",
            angle.to_degrees(),
            expected.to_degrees()
        );
        assert!(spread < 0.02, "the flag flutters by {spread} rad in a steady wind");
    }
}

#[test]
fn lift_raises_the_flag_and_no_wind_leaves_it_hanging() {
    let base = WindParams { velocity: Vec3::new(0.0, 0.0, 5.0), drag: 0.12, gust: 0.0, ..WindParams::default() };
    let (plain, _) = flag_angle(WindParams { lift: 0.0, ..base }, 12.0, 60);
    let (lifted, _) = flag_angle(WindParams { lift: 0.3, ..base }, 12.0, 60);
    assert!(lifted > plain + 0.05, "lift moved the flag from {plain} to {lifted} rad");
    let (calm, _) = flag_angle(WindParams::default(), 6.0, 60);
    assert!(calm.abs() < 0.01, "still air tilts the flag by {calm} rad");
}

#[test]
fn gusts_vary_with_the_clock_and_replay_exactly() {
    let wind =
        WindParams { velocity: Vec3::new(0.0, 0.0, 5.0), drag: 0.12, gust: 0.5, gust_hz: 0.7, ..WindParams::default() };
    let (_, gusty) = flag_angle(wind, 12.0, 360);
    assert!(gusty > 0.03, "gusts move the flag by only {gusty} rad");
    let run = || {
        let sheet = flag(9, 0.4);
        let mut rig = Rig::new(&sheet, ContactParams::default(), 0);
        rig.env.set_wind(Some(Wind::new(sheet.shape(), wind).unwrap()));
        for _ in 0..600 {
            rig.frame(FRAME, &[]);
        }
        bits(&rig.state)
    };
    assert_eq!(run(), run(), "gusty wind is not a pure function of the clock");
}

/// The flag of `flag_angle` under gusty wind at `rate_hz`, as its particle positions every 10th frame of 8 s.
fn gusty_flag_at(rate_hz: f32) -> Vec<Vec<Vec3>> {
    let wind = WindParams {
        velocity: Vec3::new(0.0, 0.0, 5.0),
        drag: 0.12,
        lift: 0.0,
        gust: 0.5,
        gust_hz: 0.7,
        rate_hz,
        ..WindParams::default()
    };
    let sheet = flag(13, 0.5);
    let mut rig = Rig::new(&sheet, ContactParams::default(), 0);
    rig.env.set_wind(Some(Wind::new(sheet.shape(), wind).unwrap()));
    let mut out = Vec::new();
    for frame in 0..480 {
        rig.frame(FRAME, &[]);
        if frame % 10 == 0 {
            out.push(rig.positions());
        }
    }
    out
}

#[test]
fn wind_at_the_physics_rate_stays_close_to_wind_at_every_sub_step() {
    // `rate_hz` 120 (the default; no lift, which would make the flag flutter chaotically and no two runs comparable) applies the wind once per 4 sub-steps with the push of all four; 480 is every
    // sub-step, the cost the default avoids. The behaviour differs by the wind's change within 1/120 s: the gust here
    // moves at 0.7 Hz, so the flag's particles must stay within 10 mm (2%) of each other over 8 s of a 0.5 m flag (measured 6.9 mm), and a
    // steady wind's settled angle within 1%.
    let (every, physics) = (gusty_flag_at(480.0), gusty_flag_at(120.0));
    let worst =
        every.iter().zip(&physics).flat_map(|(a, b)| a.iter().zip(b)).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
    println!("max particle distance between rate 480 and 120: {worst} m");
    assert!(worst < 0.010, "wind at 120 Hz drifts {worst} m from wind at 480 Hz");
    let steady = |rate_hz| {
        let wind = WindParams {
            velocity: Vec3::new(0.0, 0.0, 5.0),
            drag: 0.12,
            lift: 0.0,
            gust: 0.0,
            rate_hz,
            ..WindParams::default()
        };
        flag_angle(wind, 20.0, 120).0
    };
    let (a, b) = (steady(480.0), steady(120.0));
    println!("steady flag angle {a} rad at 480 Hz, {b} at 120 Hz");
    assert!((a - b).abs() < 0.01 * a, "a steady wind settles at {a} rad every sub-step and {b} rad at 120 Hz");
}

// ---------------------------------------------------------------------------------------------------------------------
// Rig colliders, refusals, determinism, cost.

#[test]
fn rig_colliders_are_placed_in_the_solvers_space() {
    let to_solver = Affine3A::from_scale_rotation_translation(
        Vec3::splat(2.0),
        Quat::from_rotation_z(std::f32::consts::FRAC_PI_2),
        Vec3::new(1.0, 0.0, 0.0),
    );
    let sphere =
        Collider { bone: BoneId(3), shape: ColliderShape::Sphere { offset: Vec3::new(0.1, 0.0, 0.0), radius: 0.05 } };
    let placed = Shape::from_collider(&sphere.shape, &to_solver).unwrap();
    assert!((placed.axis().0 - Vec3::new(1.0, 0.2, 0.0)).length() < 1e-6 && (placed.radius() - 0.1).abs() < 1e-6);
    let capsule = ColliderShape::Capsule { offset: Vec3::ZERO, radius: 0.04, tail: Vec3::new(0.0, 0.3, 0.0) };
    let placed = Shape::from_collider(&capsule, &to_solver).unwrap();
    let (a, b) = placed.axis();
    assert!((a - Vec3::new(1.0, 0.0, 0.0)).length() < 1e-6 && (b - Vec3::new(0.4, 0.0, 0.0)).length() < 1e-6);
    assert!((placed.radius() - 0.08).abs() < 1e-6);
    // Spring-only shapes are not cloth contacts.
    let inside = ColliderShape::SphereInside { offset: Vec3::ZERO, radius: 0.5 };
    assert!(Shape::from_collider(&inside, &to_solver).is_none());
    let plane = ColliderShape::Plane { offset: Vec3::ZERO, normal: Vec3::Y };
    assert!(Shape::from_collider(&plane, &to_solver).is_none());
}

#[test]
fn bad_shapes_slots_and_parameters_are_refused() {
    let mut env = ClothEnvironment::new(2, ContactParams::default()).unwrap();
    assert_eq!(env.place(0, Shape::sphere(Vec3::NAN, 0.1)), Err(ContactError::BadShape { index: 0 }));
    assert_eq!(env.place(0, Shape::sphere(Vec3::ZERO, 0.0)), Err(ContactError::BadShape { index: 0 }));
    assert_eq!(env.place(1, Shape::sphere(Vec3::ZERO, 0.1)), Err(ContactError::Capacity { index: 1, capacity: 2 }));
    assert!(env.is_empty());
    env.place(0, Shape::sphere(Vec3::ZERO, 0.1)).unwrap();
    env.place(1, Shape::sphere(Vec3::X, 0.1)).unwrap();
    assert_eq!(env.place(2, Shape::sphere(Vec3::X, 0.1)), Err(ContactError::Capacity { index: 2, capacity: 2 }));
    env.truncate(1);
    assert_eq!(env.len(), 1);
    let bad = |f: fn(&mut ContactParams)| {
        let mut p = ContactParams::default();
        f(&mut p);
        ClothEnvironment::new(1, p).is_err()
    };
    assert!(bad(|p| p.thickness = -1.0));
    assert!(bad(|p| p.margin = f32::NAN));
    assert!(bad(|p| p.friction = Friction { static_: 0.2, kinetic: 0.5 }));
    let shape = SheetShape { rows: 2, cols: 2, wrap: false };
    assert!(Wind::new(shape, WindParams { drag: -1.0, ..WindParams::default() }).is_err());
    assert!(Wind::new(shape, WindParams { velocity: Vec3::NAN, ..WindParams::default() }).is_err());
}

/// A scene with a swinging capsule, a sphere and gusty wind over a pinned skirt-like sheet.
fn busy_scene(frames: usize) -> Rig {
    let n = 21;
    let sheet = sheet(n, 0.4, |u, v| Vec3::new(u, 0.0, v), |r, _| if r == 0 { 1.0 } else { 0.0 });
    let mut rig = Rig::new(&sheet, ContactParams::default(), 2);
    let wind = WindParams { velocity: Vec3::new(2.0, 0.0, 3.0), ..WindParams::default() };
    rig.env.set_wind(Some(Wind::new(sheet.shape(), wind).unwrap()));
    for frame in 0..frames {
        let t = frame as f32 * FRAME;
        let swing = Vec3::new((t * 6.0).sin() * 0.2, -0.05, (t * 4.0).cos() * 0.2);
        rig.frame(
            FRAME,
            &[
                Shape::capsule(swing, swing + Vec3::new(0.0, -0.2, 0.1), 0.04),
                Shape::sphere(Vec3::new(0.0, -0.25, 0.05), 0.08),
            ],
        );
    }
    rig
}

#[test]
fn replays_are_bit_exact() {
    assert_eq!(bits(&busy_scene(1200).state), bits(&busy_scene(1200).state));
}

#[test]
fn a_step_with_contacts_and_wind_allocates_nothing() {
    let mut rig = busy_scene(10);
    let shapes = [Shape::capsule(Vec3::ZERO, Vec3::Y * 0.2, 0.05), Shape::sphere(Vec3::new(0.0, -0.2, 0.0), 0.08)];
    let n = allocations_in(|| {
        for _ in 0..50 {
            rig.frame(FRAME, &shapes);
        }
    });
    assert_eq!(n, 0, "a step allocated {n} times");
}

/// A tiny deterministic generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }

    fn range(&mut self, lo: f32, hi: f32) -> f32 {
        lo + (hi - lo) * self.next()
    }

    fn point(&mut self, reach: f32) -> Vec3 {
        Vec3::new(self.range(-reach, reach), self.range(-reach, reach), self.range(-reach, reach))
    }
}

#[test]
fn a_million_random_steps_produce_no_nan() {
    let mut rng = Lcg(550);
    for case in 0..50 {
        let n = 3 + case % 3;
        let free = case % 2 == 0;
        let sheet = sheet(n, 0.2, |u, v| Vec3::new(u, 0.0, v), |r, c| if !free && r == 0 && c < 2 { 1.0 } else { 0.0 });
        let mut rig = Rig::new(&sheet, ContactParams::default(), 4);
        let params = WindParams {
            velocity: Vec3::ZERO,
            drag: 1.0,
            lift: 1.0,
            gust: 1.0,
            gust_hz: 5.0,
            gust_phase_per_m: 20.0,
            rate_hz: 120.0,
        };
        rig.env.set_wind(Some(Wind::new(sheet.shape(), params).unwrap()));
        for step in 0..20_000 {
            // Hostile colliders: teleporting, tiny, huge, long and thin.
            let count = (rng.next() * 5.0) as usize;
            rig.env.begin_frame();
            for i in 0..count {
                let a = rng.point(0.4);
                let b = if rng.next() < 0.5 { a } else { a + rng.point(0.6) };
                let shape = Shape::capsule(a, b, rng.range(0.001, 0.3));
                rig.env.place(i, shape).unwrap();
            }
            rig.env.truncate(count);
            if let Some(wind) = rig.env.wind_mut() {
                wind.params.velocity = rng.point(40.0);
            }
            let dt = if step % 97 == 0 { rng.range(0.0, 0.2) } else { rng.range(0.002, 0.03) };
            rig.solver.step_with(dt, &mut rig.state, &rig.targets, &mut rig.env).unwrap();
            assert!(
                rig.state.positions().iter().chain(rig.state.velocities()).all(|v| v.is_finite()),
                "case {case} step {step}: non-finite state"
            );
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Evidence.

/// Writes the sequences the contact sheet is rendered from. Run with `CLOTH_CONTACT_OUT=<dir> cargo test -p omuzaki-char
/// --test cloth_contact -- --ignored dump_contact_sequence`: `drape.txt` (a sheet dropped over a capsule and a sphere,
/// then a capsule swung in through its hanging side) with one line per frame at 60 fps (time, then every particle's x y z), and
/// `shapes.txt` with each frame's colliders (`a.xyz b.xyz radius` each).
/// The sheet's height under the point `(x, z)` of the horizontal plane: the triangle covering it seen from above,
/// interpolated; `None` where no triangle covers it.
fn height_under(x: f32, z: f32, tris: impl Iterator<Item = [Vec3; 3]>) -> Option<f32> {
    for [p, q, r] in tris {
        let det = (q.z - r.z) * (p.x - r.x) + (r.x - q.x) * (p.z - r.z);
        if det.abs() < 1e-12 {
            continue;
        }
        let l1 = ((q.z - r.z) * (x - r.x) + (r.x - q.x) * (z - r.z)) / det;
        let l2 = ((r.z - p.z) * (x - r.x) + (p.x - r.x) * (z - r.z)) / det;
        let l3 = 1.0 - l1 - l2;
        if l1 >= 0.0 && l2 >= 0.0 && l3 >= 0.0 {
            return Some(l1 * p.y + l2 * q.y + l3 * r.y);
        }
    }
    None
}

/// The triangles of an `n` x `n` open sheet.
fn triangles(n: usize, x: &[Vec3]) -> impl Iterator<Item = [Vec3; 3]> + '_ {
    (0..n - 1).flat_map(move |r| {
        (0..n - 1).flat_map(move |c| {
            let at = |r: usize, c: usize| x[r * n + c];
            [[at(r, c), at(r + 1, c), at(r, c + 1)], [at(r, c + 1), at(r + 1, c), at(r + 1, c + 1)]]
        })
    })
}

/// #129 limit 1: a 3 mm capsule lying along a row, in the middle of a row of 1.9 cm cells and longer than the sheet,
/// swung down at 5 m/s into a 0.6 m sheet pinned along the two edges parallel to it, and held 5 cm under its rest. Returns how many frames ended with some point of its axis under the
/// sheet (it started above): frames in which it had crossed a triangle.
fn thin_capsule_crossings(triangle_contacts: bool) -> usize {
    let n = 33;
    // Pinned along the two edges parallel to the capsule, free at the ends it passes out of.
    let pins = move |r: usize, _| if r == 0 || r == n - 1 { 1.0 } else { 0.0 };
    let mut rig = Rig::new(&sheet(n, 0.6, |u, v| Vec3::new(u, 0.0, v), pins), ContactParams::default(), 1);
    let cell = 0.6 / (n - 1) as f32;
    if triangle_contacts {
        rig.env.set_sheet(Some(SheetShape { rows: n, cols: n, wrap: false }), cell);
    }
    // Half a cell off a row and a column: between particles in both directions.
    let (z, x) = (0.5 * cell, 0.5 * cell);
    // Longer than the sheet is wide, like a blade swung through a curtain: no end to slip off.
    let at = |y: f32| Shape::capsule(Vec3::new(x - 0.4, y, z), Vec3::new(x + 0.4, y, z), 0.003);
    rig.start(&[at(0.05)]);
    let mut crossings = 0;
    for frame in 1..=60 {
        // 5 m/s down for 20 ms (0.1 m), then held.
        let y = (0.05 - 5.0 * frame as f32 * FRAME).max(-0.05);
        rig.frame(FRAME, &[at(y)]);
        let (a, b) = at(y).axis();
        let x = rig.positions();
        let under = (0..=8).any(|k| {
            let p = a.lerp(b, 0.25 + 0.5 * k as f32 / 8.0);
            height_under(p.x, p.z, triangles(n, &x)).is_some_and(|h| h > p.y)
        });
        if under {
            crossings += 1;
        }
    }
    crossings
}

#[test]
fn a_thin_capsule_swung_through_a_fine_sheet_crosses_no_triangle() {
    let without = thin_capsule_crossings(false);
    let with = thin_capsule_crossings(true);
    println!(
        "3 mm capsule at 5 m/s through 1.9 cm cells: crossed in {without} frames without triangle contacts, {with} with"
    );
    assert!(without > 0, "the particle sweep alone should miss a capsule between particles (the limit the fix closes)");
    assert_eq!(with, 0);
}

/// #129 limit 2: a sheet dropped onto two overlapping 5 cm spheres 6 cm apart sinks into the crease between them;
/// over 10 s no particle ends a frame inside either sphere.
#[test]
fn a_particle_squeezed_between_overlapping_spheres_stays_outside_both() {
    let spheres = [Shape::sphere(Vec3::new(-0.03, 0.0, 0.0), 0.05), Shape::sphere(Vec3::new(0.03, 0.0, 0.0), 0.05)];
    // A heavy 17 x 17 sheet 0.12 m wide, so gravity presses its middle row into the crease.
    let mut rig = Rig::new(&sheet(17, 0.12, |u, v| Vec3::new(u, 0.06, v), |_, _| 0.0), ContactParams::default(), 2);
    rig.start(&spheres);
    let mut deepest_in = f32::MIN;
    for _ in 0..600 {
        rig.frame(FRAME, &spheres);
        for p in rig.positions() {
            for s in &spheres {
                deepest_in = deepest_in.max(s.radius() - p.distance(s.axis().0));
            }
        }
    }
    println!("squeezed between two spheres for 10 s: deepest {:.3} mm inside (negative: outside)", deepest_in * 1e3);
    assert!(deepest_in <= 0.0, "a particle ended {deepest_in} m inside a sphere");
}

#[test]
#[ignore = "writes evidence files; run by hand"]
fn dump_contact_sequence() {
    use std::fmt::Write as _;
    let dir = std::env::var("CLOTH_CONTACT_OUT").expect("set CLOTH_CONTACT_OUT to the output directory");
    let mut rig = Rig::new(&flat(33, 0.6, 0.2), ContactParams::default(), 3);
    let rest = [
        Shape::capsule(Vec3::new(-0.3, 0.0, 0.0), Vec3::new(0.0, 0.0, 0.0), 0.05),
        Shape::sphere(Vec3::new(0.14, -0.03, 0.0), 0.09),
    ];
    let (mut sheet_lines, mut shape_lines) = (String::new(), String::new());
    for frame in 0..=420 {
        let t = frame as f32 * FRAME;
        // From 4 s a capsule, pivoting at one end outside the sheet, swings in through the curtain of cloth that hangs on
        // the +z side (in the plane x = -0.1125, a column of the sheet).
        let theta = -0.9 + 0.7 * (std::f32::consts::PI * 0.8 * (t - 4.0).max(0.0)).sin().powi(2);
        let pivot = Vec3::new(-0.1125, -0.1, 0.4);
        let bat_tip = pivot + Vec3::new(0.0, theta.sin(), -theta.cos()) * 0.4;
        let bat = Shape::capsule(pivot, bat_tip, 0.025);
        let mut shapes = rest.to_vec();
        if t > 4.0 {
            shapes.push(bat);
        }
        if frame > 0 {
            rig.frame(FRAME, &shapes);
        }
        write!(sheet_lines, "{t:.4}").unwrap();
        for x in rig.state.positions() {
            write!(sheet_lines, " {:.5} {:.5} {:.5}", x.x, x.y, x.z).unwrap();
        }
        writeln!(sheet_lines).unwrap();
        for s in &shapes {
            let ((a, b), r) = (s.axis(), s.radius());
            write!(shape_lines, "{:.4} {:.4} {:.4} {:.4} {:.4} {:.4} {:.4};", a.x, a.y, a.z, b.x, b.y, b.z, r).unwrap();
        }
        writeln!(shape_lines).unwrap();
    }
    std::fs::write(format!("{dir}/drape.txt"), sheet_lines).unwrap();
    std::fs::write(format!("{dir}/shapes.txt"), shape_lines).unwrap();
}
