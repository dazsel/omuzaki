//! The XPBD cloth solver against closed forms and under abuse:
//!
//! - **Physics:** a curtain's stretch and a hanging sheet's sag against their exact discrete closed forms for the
//!   chosen compliance (and the continuous elastic catenary), converged and at the live budget; the default fabric
//!   against the catenary; the bend energy of a sheet wrapped on a cylinder against `½ D κ² A`; a sheet with twice the
//!   rows and columns hanging like the coarser one; energy that never grows after an impulse (soft, default and stiff
//!   fabrics, damped or not).
//! - **Pins** land exactly on their targets after every step; soft pins pull by their weight.
//! - **Hostile input:** frame times of 0, negative, NaN, infinite and huge; extreme stiffness and parameters; NaN and
//!   far-off targets; a corrupted state; a hook that writes NaN; a 1e6-step fuzz of random sheets, pins and parameters.
//! - **Time:** byte-identical replays over 10,000 steps, the same motion at 30 to 240 fps, stable from 240 to 20 Hz.
//! - **Cost:** no allocation in a step (a counting allocator, which is shown to catch an injected one).
//!
//! The suite runs twice: `cloth_solver/main.rs` runs it with the quality preset and `cloth_live.rs` with the live one,
//! each defining `preset()` at its crate root.
//!
//! `dump_skirt_sequence` (ignored) writes the settling-skirt sequence the PR's contact sheet was rendered from.

// Test helpers unwrap: a fixture that fails to build should stop the test with its reason. A counting allocator is the
// only way to prove "no allocation"; it forwards to `System` unchanged.
#![allow(clippy::expect_used, clippy::unwrap_used, unsafe_code)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)] // small counts
#![allow(clippy::float_cmp)] // pins are asserted bit-exact on purpose

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::f32::consts::TAU;

use omuzaki_char::cloth::solver::{
    ClothError, ClothParams, ClothSheet, ClothSolver, ClothState, MAX_COORDINATE, MAX_SPEED, NoHook, SheetShape,
    SolverConfig, StepReport, Substep, SubstepHook,
};
use omuzaki_core::glam::{Quat, Vec3, Vec3A};
use proptest::prelude::*;

// ---------------------------------------------------------------------------------------------------------------------
// Counting allocator: counts, per thread, the allocations a thread makes while it is measuring. Per thread, because the
// harness's own threads allocate at unpredictable times and two measuring tests may run at once.

struct Counting;

thread_local! {
    // `const` init with no destructor: using them never allocates, so they are safe inside the allocator.
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

/// Areal density of the test fabric, kg/m² (a light cotton).
const DENSITY: f32 = 0.2;
const G: f64 = 9.81;
const STEP: f32 = 1.0 / 120.0;

/// A sheet of uniform fabric whose particle `(r, c)` rests at `place(r, c)` with pin weight `pin(r, c)`.
fn sheet(shape: SheetShape, place: impl Fn(usize, usize) -> Vec3, pin: impl Fn(usize, usize) -> f32) -> ClothSheet {
    let cells = (0..shape.rows).flat_map(|r| (0..shape.cols).map(move |c| (r, c)));
    let rest = cells.clone().map(|(r, c)| place(r, c)).collect();
    let pins = cells.map(|(r, c)| pin(r, c)).collect();
    ClothSheet::with_density(shape, rest, DENSITY, pins).unwrap()
}

fn pinned_if(yes: bool) -> f32 {
    if yes { 1.0 } else { 0.0 }
}

/// A vertical curtain hanging from its top row, `l` between particles.
fn curtain(rows: usize, cols: usize, l: f32) -> ClothSheet {
    let shape = SheetShape { rows, cols, wrap: false };
    sheet(shape, |r, c| Vec3::new(c as f32 * l, -(r as f32) * l, 0.0), |r, _| pinned_if(r == 0))
}

/// A flared skirt: `cols` around (wrapped), `rows` from a 14 cm waist to a 32 cm hem 42 cm below, waist row pinned.
fn skirt(rows: usize, cols: usize) -> ClothSheet {
    let shape = SheetShape { rows, cols, wrap: true };
    let place = |r: usize, c: usize| {
        let t = r as f32 / (rows - 1) as f32;
        let (radius, angle) = (0.14 + 0.18 * t, TAU * c as f32 / cols as f32);
        Vec3::new(radius * angle.sin(), -0.42 * t, radius * angle.cos())
    };
    sheet(shape, place, |r, _| pinned_if(r == 0))
}

fn solver(sheet: &ClothSheet, params: ClothParams, config: SolverConfig) -> ClothSolver {
    ClothSolver::new(sheet, &params, &config).unwrap()
}

/// The state's current pin targets, as a step takes them.
fn targets_of(state: &ClothState) -> Vec<Vec3> {
    state.targets().iter().map(|&t| Vec3::from(t)).collect()
}

/// Steps `seconds` at 120 Hz with the pins held where they are.
fn hold(solver: &mut ClothSolver, state: &mut ClothState, seconds: f32) {
    let targets = targets_of(state);
    for _ in 0..(seconds * 120.0).round() as usize {
        solver.step(STEP, state, &targets).unwrap();
    }
}

/// Hips that turn `turn` radians either way at 1 Hz and sway `sway` metres sideways at 0.7 Hz.
#[derive(Copy, Clone)]
struct Motion {
    turn: f32,
    sway: f32,
}

/// A vigorous dance: +-30 degrees and 5 cm.
const DANCE: Motion = Motion { turn: 0.52, sway: 0.05 };
/// A walk: +-10 degrees and 3 cm.
const WALK: Motion = Motion { turn: 0.17, sway: 0.03 };

/// The pin targets `rest` moved by `motion` at time `t`.
fn move_into(rest: &[Vec3], motion: Motion, t: f64, out: &mut [Vec3]) {
    let t = t as f32;
    let turn = Quat::from_rotation_y(motion.turn * (TAU * t).sin());
    let sway = Vec3::new(motion.sway * (TAU * 0.7 * t).sin(), 0.0, 0.0);
    for (o, &r) in out.iter_mut().zip(rest) {
        *o = turn * r + sway;
    }
}

/// The waist dancing at time `t`.
fn waist_into(rest: &[Vec3], t: f64, out: &mut [Vec3]) {
    move_into(rest, DANCE, t, out);
}

/// The largest strain of the edges running down a sheet's columns (toward a pinned top row).
fn column_strain(sheet: &ClothSheet, state: &ClothState) -> f32 {
    let (cols, rest, x) = (sheet.shape().cols, sheet.rest(), state.positions());
    (cols..rest.len()).map(|i| x[i].distance(x[i - cols]) / rest[i].distance(rest[i - cols]) - 1.0).fold(0.0, f32::max)
}

/// Every hard pin's particle sits bit-exactly on its target.
fn assert_pins_exact(sheet: &ClothSheet, state: &ClothState, targets: &[Vec3]) {
    for (slot, &p) in sheet.pins().iter().enumerate() {
        if sheet.pin_weights()[p as usize] >= 1.0 {
            assert_eq!(state.positions()[p as usize], Vec3A::from(targets[slot]), "pin {slot} (particle {p})");
        }
    }
}

fn assert_sane(state: &ClothState) {
    for (x, v) in state.positions().iter().zip(state.velocities()) {
        assert!(x.is_finite() && x.abs().max_element() <= MAX_COORDINATE, "position {x}");
        assert!(v.is_finite() && v.length() <= MAX_SPEED * 1.0001, "velocity {v}");
    }
}

/// Every state float as bits, for byte-identity checks.
fn bits(state: &ClothState) -> Vec<u32> {
    let vectors = state.positions().iter().chain(state.velocities()).chain(state.targets());
    vectors.flat_map(Vec3A::to_array).map(f32::to_bits).collect()
}

// ---------------------------------------------------------------------------------------------------------------------
// Physics against closed forms.

#[test]
fn a_curtain_stretches_by_the_closed_form() {
    // 12 rows of 6 hanging from the top row, 5 cm apart, soft enough to stretch 2% at the top. Shear and bend are off
    // and so is the limiter, so the columns are independent chains and the closed form is exact: the edge above row i
    // carries the weight of rows i.. and stretches by `α · load` (α = stretch for an interior edge of a square grid).
    let (rows, cols, l) = (12, 6, 0.05_f32);
    let sheet = curtain(rows, cols, l);
    let params = ClothParams {
        stretch: 0.02,
        shear: f32::INFINITY,
        bend: f32::INFINITY,
        max_stretch: f32::INFINITY,
        damping: 5.0,
        ..ClothParams::default()
    };
    let col = 2;
    let mass = |r: usize| 1.0 / f64::from(sheet.inv_mass()[r * cols + col]);
    let extension: f64 = (1..rows).map(|i| f64::from(params.stretch) * G * (i..rows).map(mass).sum::<f64>()).sum();
    let measure = |config: SolverConfig| {
        let mut solver = solver(&sheet, params, config);
        let mut state = solver.rest_state();
        hold(&mut solver, &mut state, 10.0);
        let drop = state.positions()[col].y - state.positions()[(rows - 1) * cols + col].y;
        f64::from(drop) - f64::from(l) * (rows - 1) as f64
    };
    // Converged (8 iterations) and at the live budget (4 x 2) alike: the columns are solved as chains.
    for (name, config) in [("converged", SolverConfig { iterations: 8, ..crate::preset() }), ("live", crate::preset())]
    {
        let measured = measure(config);
        println!("curtain: {name} extension / closed form = {:.5}", measured / extension);
        assert!((measured / extension - 1.0).abs() < 0.005, "{name}: {measured} vs closed form {extension}");
    }
}

/// The exact sag of a discrete elastic catenary: `n` segments of rest length `l` and compliance `alpha` (m/N) holding
/// `n - 1` equal masses `m`, ends level and `span` apart. The horizontal tension `H` is the same in every segment and
/// the vertical one drops by `m g` at each mass, so each segment's direction and stretched length follow from `H`;
/// bisection finds the `H` that gives the span. Returns the middle particle's drop.
fn discrete_catenary(n: usize, l: f64, alpha: f64, m: f64, span: f64) -> f64 {
    let shape = |h: f64| {
        let (mut v, mut width, mut sag) = ((n - 1) as f64 * m * G / 2.0, 0.0, 0.0);
        for j in 1..=n {
            let tension = h.hypot(v);
            let length = l + alpha * tension;
            width += length * h / tension;
            if j <= n / 2 {
                sag += length * v / tension;
            }
            v -= m * G;
        }
        (width, sag)
    };
    let (mut lo, mut hi) = (1e-9_f64, 1e6_f64);
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if shape(mid).0 > span { hi = mid } else { lo = mid }
    }
    shape(lo).1
}

/// The continuous elastic catenary (Irvine 1981, *Cable Structures*, eqs. 2.6 and 2.7) with level supports: unstretched
/// length `l0`, total weight `w`, axial stiffness `ea`, span `span`. Returns the mid-span sag.
fn elastic_catenary(l0: f64, w: f64, ea: f64, span: f64) -> f64 {
    let width = |h: f64| h * l0 / ea + 2.0 * h * l0 / w * (w / (2.0 * h)).asinh();
    let (mut lo, mut hi) = (1e-9_f64, 1e6_f64);
    for _ in 0..200 {
        let mid = (lo * hi).sqrt();
        if width(mid) > span { hi = mid } else { lo = mid }
    }
    w * l0 / (8.0 * ea) + lo * l0 / w * ((1.0 + (w / (2.0 * lo)).powi(2)).sqrt() - 1.0)
}

/// A 4-row sheet of 30 segments, its end columns pinned and brought in to 80% of its length over a second, then held:
/// every row hangs as a catenary. Returns the sheet, the solver and the settled state.
fn hammock(params: ClothParams, config: SolverConfig) -> (ClothSheet, ClothSolver, ClothState, f32) {
    let (rows, n, l) = (4, 30, 0.02_f32);
    let cols = n + 1;
    let shape = SheetShape { rows, cols, wrap: false };
    let sheet = sheet(shape, |r, c| Vec3::new(c as f32 * l, 0.0, r as f32 * l), |_, c| pinned_if(c == 0 || c == n));
    let mut solver = solver(&sheet, params, config);
    let mut state = solver.rest_state();
    let start = targets_of(&state);
    let shift = 0.2 * n as f32 * l;
    for frame in 1..=1200 {
        let s = (frame as f32 / 120.0).min(1.0);
        let targets: Vec<Vec3> = (sheet.pins().iter().zip(&start))
            .map(|(&p, &x)| if (p as usize).is_multiple_of(cols) { x } else { x - Vec3::X * shift * s })
            .collect();
        solver.step(STEP, &mut state, &targets).unwrap();
    }
    (sheet, solver, state, 0.8 * n as f32 * l)
}

#[test]
fn a_hanging_sheet_sags_as_the_elastic_catenary_of_its_compliance() {
    let (n, l, stretch) = (30, 0.02_f64, 0.05_f32);
    let params = ClothParams {
        stretch,
        shear: f32::INFINITY,
        bend: f32::INFINITY,
        max_stretch: f32::INFINITY,
        damping: 3.0,
        ..ClothParams::default()
    };
    // Converged and at the live 2 iterations alike (the rows are solved as chains), the sag is the closed form's.
    for (iterations, tolerance) in [(16, 0.001), (2, 0.001)] {
        let (sheet, _, state, span) = hammock(params, SolverConfig { iterations, ..crate::preset() });
        // An interior row: its masses are full cells and its edges have α = stretch (border rows have half of each).
        let row = n + 1; // row 1
        let sag = -f64::from(state.positions()[row + n / 2].y);
        let m = 1.0 / f64::from(sheet.inv_mass()[row + 3]);
        let exact = discrete_catenary(n, l, f64::from(stretch), m, f64::from(span));
        let continuum = elastic_catenary(n as f64 * l, (n - 1) as f64 * m * G, l / f64::from(stretch), f64::from(span));
        println!(
            "catenary, {iterations} iterations: sag / discrete closed form = {:.5}, / continuous = {:.5}",
            sag / exact,
            sag / continuum
        );
        assert!((sag / exact - 1.0).abs() < tolerance, "{iterations} iterations: sag {sag} vs closed form {exact}");
        // The continuum it discretises: EA = l / α, the weight of the n - 1 free masses.
        assert!((sag / continuum - 1.0).abs() < tolerance + 0.01, "sag {sag} vs continuous catenary {continuum}");
    }
}

#[test]
fn the_default_cloth_hangs_as_the_catenary() {
    // The acceptance asks for 5%; with the rows solved as chains the live budget is within 1%.
    let (sheet, solver, state, span) = hammock(ClothParams { damping: 3.0, ..ClothParams::default() }, crate::preset());
    let (n, row) = (30, 31);
    let sag = -f64::from(state.positions()[row + n / 2].y);
    let m = 1.0 / f64::from(sheet.inv_mass()[row + 3]);
    let catenary = discrete_catenary(n, 0.02, 0.0, m, f64::from(span));
    println!("default cloth: sag / inextensible catenary = {:.4}", sag / catenary);
    assert!((sag / catenary - 1.0).abs() < 0.01, "sag {sag} vs inextensible catenary {catenary}");
    assert!(solver.max_strain(&state) <= 0.05 + 1e-4);
}

#[test]
fn a_cylinder_wrap_stores_the_bending_energy_of_the_bend_compliance() {
    // A flat sheet rolled (without stretching) onto a cylinder of radius R stores ½ D κ² per unit area in the rows'
    // bend triples (κ = 1/R, D = 1/bend), and nothing in the straight columns.
    let (rows, cols, l, radius) = (6, 21, 0.02_f32, 0.1_f32);
    let sheet =
        sheet(SheetShape { rows, cols, wrap: false }, |r, c| Vec3::new(c as f32 * l, 0.0, r as f32 * l), |_, _| 0.0);
    let bend = 40.0;
    let params = ClothParams { stretch: f32::INFINITY, shear: f32::INFINITY, bend, ..ClothParams::default() };
    let solver = solver(&sheet, params, crate::preset());
    let rolled: Vec<Vec3> = (0..rows * cols)
        .map(|i| {
            let (r, c) = (i / cols, i % cols);
            let phi = c as f32 * l / radius;
            Vec3::new(radius * phi.sin(), radius * (1.0 - phi.cos()), r as f32 * l)
        })
        .collect();
    let state = solver.state_at(&rolled).unwrap();
    // The area the row triples stand for: every particle with a neighbour on both sides along its row.
    let areas: f64 = (0..rows * cols)
        .filter(|i| (1..cols - 1).contains(&(i % cols)))
        .map(|i| f64::from(DENSITY * sheet.inv_mass()[i]).recip())
        .sum();
    let expected = 0.5 / f64::from(bend) / f64::from(radius).powi(2) * areas;
    let stored = solver.energy(&state).elastic;
    println!("cylinder wrap: stored bend energy / ½ D κ² A = {:.5}", stored / expected);
    assert!((stored / expected - 1.0).abs() < 0.01, "stored {stored} J vs ½ D κ² A = {expected} J");
}

/// Kicks a settled sheet with `velocity` on every free particle and returns the total energy after each 1/120 s step
/// (index 0 is just after the kick) and the energy it settled at.
fn energy_after_kick(sheet: &ClothSheet, params: ClothParams, velocity: Vec3, seconds: f32) -> (Vec<f64>, f64) {
    let mut solver = solver(sheet, params, crate::preset());
    let mut state = solver.rest_state();
    hold(&mut solver, &mut state, 4.0);
    let rest = solver.energy(&state).total();
    for (v, &w) in state.velocities_mut().iter_mut().zip(solver.inv_mass()) {
        if w > 0.0 {
            *v = velocity.into();
        }
    }
    let targets = targets_of(&state);
    let mut energies = vec![solver.energy(&state).total()];
    for _ in 0..(seconds * 120.0) as usize {
        solver.step(STEP, &mut state, &targets).unwrap();
        energies.push(solver.energy(&state).total());
    }
    (energies, rest)
}

/// The fabrics the energy tests kick: one soft, the default, and one stiff (tensile 1e6 N/m, shear 1e5 N/m, bending
/// 0.01 N·m: a card).
fn fabrics() -> [(&'static str, ClothParams); 3] {
    [
        ("soft", ClothParams { stretch: 1e-2, shear: 1e-2, ..ClothParams::default() }),
        ("default", ClothParams::default()),
        ("stiff", ClothParams { stretch: 1e-6, shear: 1e-5, bend: 1e2, ..ClothParams::default() }),
    ]
}

/// No frame's energy is above the one before. A curtain kicked through its plane swings in tension, where every
/// constraint's energy is convex, so XPBD (an implicit Euler step) can only lose energy. Once nearly settled,
/// single-precision positions (3e-8 m apart at this size) leave about 1e-8 J of noise in the sum, so a rise below
/// 1e-5 of the kick is that floor.
fn assert_never_rises(name: &str, energies: &[f64], kick: f64) {
    let worst = energies.windows(2).map(|p| p[1] - p[0]).fold(0.0, f64::max);
    println!("energy, {name}: worst frame rise {:.1e} of the kick", worst / kick);
    for (frame, pair) in energies.windows(2).enumerate() {
        assert!(pair[1] <= pair[0] + 1e-5 * kick, "{name}, frame {frame}: energy rose from {} to {}", pair[0], pair[1]);
    }
}

#[test]
fn energy_decays_monotonically_after_an_impulse() {
    for (name, fabric) in fabrics() {
        let (energies, rest) = energy_after_kick(&curtain(12, 8, 0.04), fabric, Vec3::Z, 5.0);
        let kick = energies[0] - rest;
        assert_never_rises(&format!("{name}, damped"), &energies, kick);
        let left = energies.last().unwrap() - rest;
        assert!(left < 0.01 * kick, "{name}: {left} J of a {kick} J kick left after 5 s");
    }
}

#[test]
fn without_damping_energy_never_grows() {
    // No input and no damping: the integrator's own dissipation is all that changes the energy, and it only lowers it.
    for (name, fabric) in fabrics() {
        let undamped = ClothParams { damping: 0.0, ..fabric };
        let (energies, rest) = energy_after_kick(&curtain(12, 8, 0.04), undamped, Vec3::Z, 5.0);
        assert_never_rises(&format!("{name}, undamped"), &energies, energies[0] - rest);
    }
}

#[test]
fn a_kicked_skirt_settles() {
    // Kicked sideways and twisted, a skirt buckles: compressed hoops and bends about its curved rest are not convex, so
    // released compression may speed it up for a moment (solver docs, Accuracy). It still settles: kinetic energy falls
    // under 1% of the kick within 3 s, for the default and the stiff fabric.
    for (name, fabric) in fabrics().into_iter().skip(1) {
        let sheet = skirt(16, 24);
        let mut solver = solver(&sheet, fabric, crate::preset());
        let mut state = solver.rest_state();
        hold(&mut solver, &mut state, 2.0);
        for (i, v) in state.velocities_mut().iter_mut().enumerate().skip(24) {
            *v = Vec3::new(1.5, 0.5, (i % 24) as f32 * 0.1).into();
        }
        let kick = solver.energy(&state).kinetic;
        hold(&mut solver, &mut state, 3.0);
        let left = solver.energy(&state).kinetic;
        println!("{name} skirt kick: {:.2e} of the kinetic energy left after 3 s", left / kick);
        assert!(left < 0.01 * kick, "{name}: {left} J of {kick} J kinetic energy left after 3 s");
        assert!(solver.max_strain(&state) <= 0.05 + 1e-4);
    }
}

/// A hammock of `n` segments over 0.6 m and `rows` rows across, its end columns brought in to 80% of the length over a
/// second and held for nine: the sag of its middle row's middle.
fn hammock_sag(n: usize, rows: usize, params: ClothParams) -> f64 {
    let l = 0.6 / n as f32;
    let cols = n + 1;
    let shape = SheetShape { rows, cols, wrap: false };
    let sheet = sheet(shape, |r, c| Vec3::new(c as f32 * l, 0.0, r as f32 * l), |_, c| pinned_if(c == 0 || c == n));
    let mut solver = solver(&sheet, params, crate::preset());
    let mut state = solver.rest_state();
    let start = targets_of(&state);
    for frame in 1..=1200 {
        let s = (frame as f32 / 120.0).min(1.0);
        let targets: Vec<Vec3> = (sheet.pins().iter().zip(&start))
            .map(|(&p, &x)| if (p as usize).is_multiple_of(cols) { x } else { x - Vec3::X * 0.12 * s })
            .collect();
        solver.step(STEP, &mut state, &targets).unwrap();
    }
    -f64::from(state.positions()[rows / 2 * cols + n / 2].y)
}

/// A curtain 0.44 m tall and 0.28 m wide of `rows` rows, held 8 s: how far its middle column's bottom hangs below its
/// top.
fn curtain_drop(rows: usize) -> f64 {
    let l = 0.44 / (rows - 1) as f32;
    let cols = (0.28 / l).round() as usize + 1;
    let sheet = curtain(rows, cols, l);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    hold(&mut solver, &mut state, 8.0);
    f64::from(state.positions()[cols / 2].y - state.positions()[(rows - 1) * cols + cols / 2].y)
}

/// The default skirt with `k` times the rows and columns, held 4 s: its hem's mean depth below the waist.
fn skirt_hem(k: usize) -> f64 {
    let (rows, cols) = (16 * k, 24 * k);
    let sheet = skirt(rows, cols);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    hold(&mut solver, &mut state, 4.0);
    -f64::from(state.positions()[(rows - 1) * cols..].iter().map(|x| x.y).sum::<f32>() / cols as f32)
}

#[test]
fn a_finer_sheet_hangs_like_a_coarser_one() {
    // The same material at twice the rows and columns, at the live budget (4 sub-steps x 2 iterations): the
    // compliances follow the cell sizes and the chain solves converge whatever the edge count. Point Gauss-Seidel
    // missed this by 15% on the soft hammock, and by more the finer the sheet.
    let soft = ClothParams {
        stretch: 0.05,
        shear: f32::INFINITY,
        bend: f32::INFINITY,
        max_stretch: f32::INFINITY,
        damping: 3.0,
        ..ClothParams::default()
    };
    let default = ClothParams { damping: 3.0, ..ClothParams::default() };
    let pairs = [
        ("soft hammock", hammock_sag(30, 4, soft), hammock_sag(60, 7, soft), 0.005),
        ("hammock with shear and bend", hammock_sag(30, 4, default), hammock_sag(60, 7, default), 0.005),
        ("curtain", curtain_drop(12), curtain_drop(23), 0.005),
        ("skirt hem", skirt_hem(1), skirt_hem(2), 0.01),
    ];
    for (name, coarse, fine, tolerance) in pairs {
        println!("resolution: {name}: 2x / 1x = {:.5}", fine / coarse);
        assert!((fine / coarse - 1.0).abs() < tolerance, "{name}: {fine} at 2x vs {coarse} at 1x");
    }
}

#[test]
fn a_soft_pin_on_an_immovable_particle_is_harmless() {
    // Its spring could not move the particle, and with `stretch = 0` its update would divide by zero: it is left out.
    let shape = SheetShape { rows: 3, cols: 3, wrap: false };
    let rest: Vec<Vec3> = (0..9).map(|i| Vec3::new((i % 3) as f32 * 0.05, -((i / 3) as f32) * 0.05, 0.0)).collect();
    let inv_mass = (0..9).map(|i| if i == 1 { 0.0 } else { 2000.0 }).collect();
    let weights = (0..9)
        .map(|i| match i {
            0 => 1.0,
            1 => 0.5,
            _ => 0.0,
        })
        .collect();
    let sheet = ClothSheet::new(shape, rest, inv_mass, weights).unwrap();
    let rigid = ClothParams { stretch: 0.0, shear: 0.0, bend: 0.0, ..ClothParams::default() };
    let mut solver = solver(&sheet, rigid, crate::preset());
    assert_eq!(solver.counts().soft_pins, 0);
    let mut state = solver.rest_state();
    let home = targets_of(&state);
    let mut targets = home.clone();
    for frame in 0..240 {
        move_into(&home, DANCE, f64::from(frame) / 120.0, &mut targets);
        let report = solver.step(STEP, &mut state, &targets).unwrap();
        assert_eq!(report.reset_particles, 0, "frame {frame}");
        assert_sane(&state);
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Pins.

#[test]
fn hard_pins_land_exactly_on_their_targets_at_any_frame_time() {
    let sheet = skirt(16, 24);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let waist = targets_of(&state);
    let mut targets = waist.clone();
    let mut t = 0.0;
    for (frame, dt) in [1.0 / 30.0, 1.0 / 60.0, 1.0 / 144.0, 1.0 / 37.3, 1.0 / 240.0, 0.0, 1.0 / 1000.0, 1.0 / 120.0]
        .iter()
        .cycle()
        .take(400)
        .enumerate()
    {
        t += f64::from(*dt);
        waist_into(&waist, t, &mut targets);
        let report = solver.step(*dt, &mut state, &targets).unwrap();
        assert_eq!(report.reset_particles + report.rejected_targets, 0, "frame {frame}");
        assert_pins_exact(&sheet, &state, &targets);
    }
    assert_sane(&state);
}

#[test]
fn soft_pins_stretch_by_their_compliance() {
    // A short tube (2 rows of 8 around) hung from its top ring on soft pins: by symmetry each pin carries its column's
    // weight W and settles `α W` below its target, with α = stretch · (1 - weight) / weight.
    let (cols, radius, l) = (8, 0.1_f32, 0.05_f32);
    let shape = SheetShape { rows: 2, cols, wrap: true };
    let place = |r: usize, c: usize| {
        let angle = TAU * c as f32 / cols as f32;
        Vec3::new(radius * angle.sin(), -(r as f32) * l, radius * angle.cos())
    };
    let fabric = ClothParams { stretch: 0.1, damping: 5.0, ..ClothParams::default() };
    for weight in [0.2_f32, 0.5, 0.8] {
        let sheet = sheet(shape, place, |r, _| if r == 0 { weight } else { 0.0 });
        let mut solver = solver(&sheet, fabric, SolverConfig { iterations: 16, ..crate::preset() });
        assert_eq!(solver.counts().soft_pins, cols);
        let mut state = solver.rest_state();
        hold(&mut solver, &mut state, 6.0);
        let column = (1.0 / f64::from(sheet.inv_mass()[0]) + 1.0 / f64::from(sheet.inv_mass()[cols])) * G;
        let expected = f64::from(fabric.stretch * (1.0 - weight) / weight) * column;
        let sag = f64::from(state.targets()[0].y - state.positions()[0].y);
        println!("soft pin {weight}: sag / α W = {:.5}", sag / expected);
        assert!((sag / expected - 1.0).abs() < 0.01, "weight {weight}: sag {sag} vs α W = {expected}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Hostile input.

/// What a step's report must show.
type Expect = fn(&StepReport) -> bool;

/// Sub-steps in a 1/120 s physics step.
fn substeps_per_step() -> u32 {
    crate::preset().substeps
}

#[test]
fn hostile_frame_times_are_contained() {
    let sheet = skirt(8, 12);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let waist = targets_of(&state);
    let mut targets = waist.clone();
    let cases: [(f32, Expect); 10] = [
        (0.0, |r| r.substeps == 0 && !r.rejected_dt),
        (-1.0, |r| r.substeps == 0 && r.rejected_dt),
        (f32::NAN, |r| r.substeps == 0 && r.rejected_dt),
        (f32::INFINITY, |r| r.substeps == 0 && r.rejected_dt),
        (f32::NEG_INFINITY, |r| r.substeps == 0 && r.rejected_dt),
        (1.0e9, |r| r.substeps == 8 * substeps_per_step() && r.dropped_s > 9.0e8),
        (f32::MAX, |r| r.substeps == 8 * substeps_per_step() && r.dropped_s.is_finite()),
        (f32::MIN_POSITIVE / 4.0, |r| !r.rejected_dt), // subnormal
        (1.0e-30, |r| r.substeps == 0),
        (1.0 / 20.0, |r| r.substeps == 6 * substeps_per_step() && r.dropped_s == 0.0),
    ];
    for round in 0..20 {
        for (i, (dt, expect)) in cases.iter().enumerate() {
            waist_into(&waist, (round * 10 + i) as f64 * 0.01, &mut targets);
            let report = solver.step(*dt, &mut state, &targets).unwrap();
            assert!(expect(&report), "dt {dt}: {report:?}");
            assert_pins_exact(&sheet, &state, &targets);
            assert_sane(&state);
        }
    }
    assert!(solver.max_strain(&state) < 0.06);
}

/// The strain the hoops of an absurd fabric (rubber at 1e3 m/N, a thousand g, drag of 1e6/s) may reach in the spin: the
/// bounds are measured at the quality preset's 4 sub-steps. The centrifugal load opens the ring further the coarser the
/// sub-step (rubber: 21% at 4, 50% at 3, 57% at 2), so a live preset gets twice the room. What these cases hold the
/// same for every preset is the contract: finite state, exact pins, and edges toward the pins within the limit.
fn extreme(bound: f32) -> f32 {
    if crate::preset().substeps >= 4 { bound } else { 2.0 * bound }
}

#[test]
fn extreme_parameters_stay_finite_and_within_the_limiter() {
    let sheet = skirt(8, 12);
    let stiff = ClothParams { stretch: 0.0, shear: 0.0, bend: 0.0, ..ClothParams::default() };
    let live = crate::preset();
    // Each case with the strain its hoops (edges between particles equally far from the pins) may reach in the spin:
    // a fabric with any stiffness keeps them under 0.6%; the extremes stretch them with their extreme load (measured:
    // one 1/120 s sub-step 5%, drag that stops the sheet following the spin 7.5%, a thousand g 13.5%, rubber 21%).
    let cases = [
        ("rigid", stiff, live, 0.02),
        ("1e-12", ClothParams { stretch: 1e-12, shear: 1e-12, bend: 1e-12, ..ClothParams::default() }, live, 0.02),
        ("rubber", ClothParams { stretch: 1e3, shear: 1e3, bend: 1e9, ..ClothParams::default() }, live, extreme(0.3)),
        (
            "no limiter, rubber",
            ClothParams { stretch: 1.0, max_stretch: f32::INFINITY, ..ClothParams::default() },
            live,
            0.0,
        ),
        ("limit 0", ClothParams { max_stretch: 0.0, ..ClothParams::default() }, live, 0.01),
        ("1000 g", ClothParams { gravity: Vec3::new(0.0, -9810.0, 0.0), ..ClothParams::default() }, live, extreme(0.2)),
        ("no damping", ClothParams { damping: 0.0, ..ClothParams::default() }, live, 0.02),
        ("damping 1e6", ClothParams { damping: 1.0e6, ..ClothParams::default() }, live, extreme(0.1)),
        ("1 x 1", stiff, SolverConfig { substeps: 1, iterations: 1, ..live }, 0.07),
        ("64 x 3", stiff, SolverConfig { substeps: 64, iterations: 3, ..live }, 0.02),
    ];
    for (name, params, config, hoops) in cases {
        let mut solver = solver(&sheet, params, config);
        let mut state = solver.rest_state();
        let waist = targets_of(&state);
        let mut targets = waist.clone();
        // Two seconds of hips spinning at 2 turns a second at 60 fps, then a 3 m teleport.
        for frame in 0..121 {
            let spin = Quat::from_rotation_y(TAU * 2.0 * frame as f32 / 60.0);
            let jump = if frame == 120 { Vec3::new(3.0, 0.0, 0.0) } else { Vec3::ZERO };
            for (t, &w) in targets.iter_mut().zip(&waist) {
                *t = spin * w + jump;
            }
            let report = solver.step(1.0 / 60.0, &mut state, &targets).unwrap();
            assert_eq!(report.reset_particles, 0, "{name}: frame {frame}");
            assert_pins_exact(&sheet, &state, &targets);
            assert_sane(&state);
            if params.max_stretch.is_finite() && frame < 120 {
                // Edges toward the pins never exceed the limit; hoops are held by their own stiffness as well, so only
                // a rubber skirt spun this hard stretches them further (centrifugal load opens the ring).
                let down = column_strain(&sheet, &state);
                assert!(down <= params.max_stretch + 1e-5, "{name}: frame {frame} strain {down} toward the pins");
                let any = solver.max_strain(&state);
                assert!(any <= hoops, "{name}: frame {frame} strain {any}");
            }
        }
    }
}

/// A hook that writes NaN into particle 40 every sub-step.
struct Poison;

impl SubstepHook for Poison {
    fn project(&mut self, _: &Substep, positions: &mut [Vec3A], _: &[Vec3A], _: &[f32]) {
        positions[40] = Vec3A::NAN;
    }
}

#[test]
fn bad_targets_and_a_corrupted_state_are_reset_and_reported() {
    let sheet = skirt(8, 12);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    hold(&mut solver, &mut state, 0.5);
    let good = targets_of(&state);

    let mut bad = good.clone();
    bad[0] = Vec3::NAN;
    bad[1] = Vec3::new(f32::INFINITY, 0.0, 0.0);
    bad[2] = Vec3::splat(2.0 * MAX_COORDINATE);
    let report = solver.step(STEP, &mut state, &bad).unwrap();
    assert_eq!(report.rejected_targets, 3);
    assert_pins_exact(&sheet, &state, &good); // refused targets keep their last good value
    assert_sane(&state);

    let hem = 7 * 12 + 5;
    state.positions_mut()[hem] = Vec3A::NAN;
    state.positions_mut()[hem - 12] = Vec3A::splat(1.0e30);
    state.velocities_mut()[hem - 1] = Vec3A::new(f32::INFINITY, 0.0, 0.0);
    let report = solver.step(STEP, &mut state, &good).unwrap();
    assert_eq!(report.reset_particles, 3);
    assert_sane(&state);
    // The reset hem particle is back near the hem: within a few centimetres of its rest place under the pins.
    assert!(state.positions()[hem].distance(Vec3A::from(sheet.rest()[hem])) < 0.1);

    // A hook that writes NaN mid-step is contained the same way.
    let report = solver.step_with(STEP, &mut state, &good, &mut Poison).unwrap();
    assert_eq!((report.substeps, report.reset_particles), (crate::preset().substeps, crate::preset().substeps));
    assert_sane(&state);

    // Lists of the wrong length are errors, and nothing moves.
    let before = state.clone();
    assert!(matches!(solver.step(STEP, &mut state, &good[1..]), Err(ClothError::Length { .. })));
    let mut other = ClothSolver::new(&skirt(4, 12), &ClothParams::default(), &crate::preset()).unwrap().rest_state();
    assert!(matches!(solver.step(STEP, &mut other, &good), Err(ClothError::Length { .. })));
    assert_eq!(state, before);
}

#[test]
fn bad_sheets_and_parameters_are_refused() {
    let flat = |rows: usize, cols: usize| {
        (0..rows * cols).map(|i| Vec3::new((i % cols) as f32, 0.0, (i / cols) as f32)).collect::<Vec<_>>()
    };
    let open = |rows, cols| SheetShape { rows, cols, wrap: false };
    let new = |shape: SheetShape, rest: Vec<Vec3>| {
        ClothSheet::with_density(shape, rest, 0.2, vec![0.0; shape.rows * shape.cols])
    };
    assert!(matches!(new(open(1, 4), flat(1, 4)), Err(ClothError::TooSmall { what: "rows", .. })));
    assert!(matches!(
        new(SheetShape { rows: 2, cols: 2, wrap: true }, flat(2, 2)),
        Err(ClothError::TooSmall { need: 3, .. })
    ));
    assert!(matches!(
        ClothSheet::with_density(open(usize::MAX, 2), Vec::new(), 0.2, Vec::new()),
        Err(ClothError::TooLarge { .. })
    ));
    assert!(matches!(new(open(2, 3), flat(2, 2)), Err(ClothError::Length { .. })));
    let mut nan = flat(2, 3);
    nan[4].y = f32::NAN;
    assert!(matches!(new(open(2, 3), nan), Err(ClothError::Particle { index: 4, .. })));
    let mut collapsed = flat(2, 3);
    collapsed[1] = collapsed[0];
    assert!(matches!(new(open(2, 3), collapsed), Err(ClothError::Degenerate { what: "edge", .. })));
    let line: Vec<Vec3> = (0..6).map(|i| Vec3::new(i as f32, 0.0, 0.0)).collect();
    assert!(matches!(new(open(2, 3), line), Err(ClothError::Degenerate { what: "cell", .. })));
    assert!(matches!(
        ClothSheet::with_density(open(2, 2), flat(2, 2), 0.0, vec![0.0; 4]),
        Err(ClothError::Param { name: "density", .. })
    ));
    assert!(matches!(
        ClothSheet::new(open(2, 2), flat(2, 2), vec![1.0, -1.0, 1.0, 1.0], vec![0.0; 4]),
        Err(ClothError::Particle { index: 1, .. })
    ));
    assert!(matches!(
        ClothSheet::new(open(2, 2), flat(2, 2), vec![1.0; 4], vec![0.0, 1.5, 0.0, 0.0]),
        Err(ClothError::Particle { index: 1, .. })
    ));

    let sheet = new(open(2, 2), flat(2, 2)).unwrap();
    assert_eq!((sheet.index(1, 1), sheet.index(2, 0), sheet.particle_count()), (Some(3), None, 4));
    let refused = |params: ClothParams, config: SolverConfig| {
        matches!(ClothSolver::new(&sheet, &params, &config), Err(ClothError::Param { .. }))
    };
    let ok = crate::preset();
    for params in [
        ClothParams { stretch: -1.0, ..ClothParams::default() },
        ClothParams { shear: f32::NAN, ..ClothParams::default() },
        ClothParams { damping: f32::INFINITY, ..ClothParams::default() },
        ClothParams { thickness: -0.1, ..ClothParams::default() },
        ClothParams { gravity: Vec3::new(0.0, f32::NAN, 0.0), ..ClothParams::default() },
        ClothParams { max_stretch: -0.01, ..ClothParams::default() },
    ] {
        assert!(refused(params, ok), "{params:?}");
    }
    for config in [
        SolverConfig { substeps: 0, ..ok },
        SolverConfig { iterations: 65, ..ok },
        SolverConfig { max_frame_s: 0.0, ..ok },
        SolverConfig { max_frame_s: f32::NAN, ..ok },
    ] {
        assert!(refused(ClothParams::default(), config), "{config:?}");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Time.

/// Runs the swaying skirt for `frames` frames of `dt` and returns the final state.
fn sway(sheet: &ClothSheet, solver: &mut ClothSolver, dts: impl Iterator<Item = f32>) -> ClothState {
    let mut state = solver.rest_state();
    let waist = targets_of(&state);
    let mut targets = waist.clone();
    let mut t = 0.0_f64;
    for dt in dts {
        t += f64::from(dt);
        waist_into(&waist, t, &mut targets);
        solver.step(dt, &mut state, &targets).unwrap();
        assert_pins_exact(sheet, &state, &targets);
    }
    state
}

#[test]
fn ten_thousand_steps_replay_byte_for_byte() {
    // A varying frame time (with odd ones that leave time owed) and swaying pins, run twice from scratch, and again
    // from a snapshot taken half way.
    let sheet = skirt(8, 12);
    let dts = || [1.0 / 60.0, 1.0 / 144.0, 1.0 / 30.0, 1.0 / 97.0, 1.0 / 240.0].into_iter().cycle().take(10_000);
    let first = sway(&sheet, &mut solver(&sheet, ClothParams::default(), crate::preset()), dts());
    let second = sway(&sheet, &mut solver(&sheet, ClothParams::default(), crate::preset()), dts());
    assert_eq!(bits(&first), bits(&second));
    assert_eq!(first.substep(), second.substep());

    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let targets = targets_of(&state);
    for _ in 0..500 {
        solver.step(1.0 / 97.0, &mut state, &targets).unwrap();
    }
    let mut snapshot = state.clone();
    for state in [&mut state, &mut snapshot] {
        for _ in 0..500 {
            solver.step(1.0 / 97.0, state, &targets).unwrap();
        }
    }
    assert_eq!(bits(&state), bits(&snapshot));
}

#[test]
fn the_motion_is_the_same_at_any_frame_rate() {
    // A cape (a 12 x 8 curtain) on walking and dancing hips for 2 s at 30 to 240 fps, against 120 fps, particle by
    // particle. A skirt cannot be compared this way: its cone buckles into folds whose pattern hangs on round-off (a
    // 1 µm nudge grows a thousandfold in 2 s), whatever the frame rate.
    let sheet = curtain(12, 8, 0.04);
    let run = |fps: u32, motion: Motion| {
        let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
        let mut state = solver.rest_state();
        let waist = targets_of(&state);
        let mut targets = waist.clone();
        let dt = 1.0 / fps as f32;
        for frame in 1..=2 * fps {
            move_into(&waist, motion, f64::from(frame) * f64::from(dt), &mut targets);
            solver.step(dt, &mut state, &targets).unwrap();
        }
        state
    };
    // The dance turns the cape at 1 Hz, right on its own swing (about 0.9 Hz): at 30 fps the pins' path between
    // samples, rebuilt without look-ahead, is a fraction of a millimetre off, and the resonance amplifies that.
    for (motion, name, bounds) in [(WALK, "walk", [0.002; 4]), (DANCE, "dance", [0.01, 0.002, 0.002, 0.002])] {
        let reference = run(120, motion);
        for (fps, bound) in [30, 60, 144, 240].into_iter().zip(bounds) {
            let state = run(fps, motion);
            let worst =
                state.positions().iter().zip(reference.positions()).map(|(a, b)| a.distance(*b)).fold(0.0, f32::max);
            println!("frame rate: {name} at {fps} fps ends {:.3} mm from 120 fps", worst * 1000.0);
            assert!(worst < bound, "{name} at {fps} fps: {worst} m from the 120 fps run");
        }
    }
}

#[test]
fn stable_from_240_to_20_hz() {
    let sheet = skirt(16, 24);
    for fps in [240.0, 120.0, 60.0, 30.0, 20.0] {
        let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
        let state = sway(&sheet, &mut solver, std::iter::repeat_n(1.0 / fps, (2.0 * fps) as usize));
        assert_sane(&state);
        let strain = solver.max_strain(&state);
        assert!(strain <= 0.05 + 1e-4, "{fps} Hz: strain {strain}");
        assert!(solver.energy(&state).kinetic < 0.5, "{fps} Hz: the swaying skirt is not flying apart");
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Hooks.

/// Records every sub-step it sees.
#[derive(Default)]
struct Recorder {
    seen: Vec<Substep>,
}

impl SubstepHook for Recorder {
    fn accelerate(&mut self, substep: &Substep, _: &[Vec3A], _: &mut [Vec3A], _: &[f32]) {
        self.seen.push(*substep);
    }
}

/// A floor at y = 0 and a steady wind: the shape of contacts and wind.
struct FloorAndWind {
    wind: Vec3A,
}

impl SubstepHook for FloorAndWind {
    fn accelerate(&mut self, substep: &Substep, _: &[Vec3A], velocities: &mut [Vec3A], inv_mass: &[f32]) {
        for (v, &w) in velocities.iter_mut().zip(inv_mass) {
            if w > 0.0 {
                *v += self.wind * substep.h;
            }
        }
    }

    fn project(&mut self, _: &Substep, positions: &mut [Vec3A], _: &[Vec3A], inv_mass: &[f32]) {
        for (x, &w) in positions.iter_mut().zip(inv_mass) {
            if w > 0.0 {
                x.y = x.y.max(0.0);
            }
        }
    }
}

#[test]
fn hooks_see_every_substep_with_its_place_in_the_frame() {
    let sheet = curtain(4, 4, 0.05);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let targets = targets_of(&state);
    let mut recorder = Recorder::default();
    solver.step_with(1.0 / 60.0, &mut state, &targets, &mut recorder).unwrap();
    solver.advance(3, &mut state, &targets, &mut recorder).unwrap();
    let fractions: Vec<f32> = recorder.seen.iter().map(|s| s.fraction).collect();
    // A 1/60 s frame is two physics steps of `substeps` sub-steps each; `advance(3)` is a frame of its own.
    let n = 2 * crate::preset().substeps as usize;
    let expected: Vec<f32> = (1..=n).map(|k| k as f32 / n as f32).chain((1..=3).map(|k| k as f32 / 3.0)).collect();
    assert!(fractions.iter().zip(&expected).all(|(f, e)| (f - e).abs() < 1e-6), "{fractions:?}");
    assert!(fractions[n - 1] == 1.0 && fractions[n + 2] == 1.0, "a frame's last sub-step ends exactly at its end");
    assert!(recorder.seen.iter().enumerate().all(|(i, s)| s.index == i as u64 && s.h == solver.substep_s()));
    assert_eq!(state.substep(), n as u64 + 3);

    // Odd frame times: a sub-step ends where the clock says, and the frame's last pins land exactly.
    recorder.seen.clear();
    // A 1/300 s frame owes 0.4 sub-steps per sub-step in a physics step: one sub-step runs (so at least 3 per step).
    solver.step_with(1.0 / 300.0, &mut state, &targets, &mut recorder).unwrap();
    let f = recorder.seen[0].fraction;
    assert!((f - 2.5 / crate::preset().substeps as f32).abs() < 1e-5, "{f}");
}

#[test]
fn a_hook_adds_contacts_and_wind() {
    // An unpinned sheet dropped 20 cm onto a floor in a wind: it lands, never sinks below the floor, and drifts.
    let shape = SheetShape { rows: 6, cols: 6, wrap: false };
    let sheet = sheet(shape, |r, c| Vec3::new(c as f32 * 0.04, 0.2, r as f32 * 0.04), |_, _| 0.0);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let mut hook = FloorAndWind { wind: Vec3A::new(0.5, 0.0, 0.0) };
    for _ in 0..240 {
        solver.step_with(STEP, &mut state, &[], &mut hook).unwrap();
        assert!(state.positions().iter().all(|x| x.y >= 0.0));
    }
    assert!(state.positions().iter().all(|x| x.y < 0.01), "the sheet lies on the floor");
    assert!(state.positions()[0].x > 0.05, "the wind pushed it along");
}

// ---------------------------------------------------------------------------------------------------------------------
// Cost.

/// Exercises every path a real-time call can take: frame rates that owe time, a frame too short for a sub-step,
/// hostile frame times, refused targets, a corrupted particle, a hook, an exact advance, a reset, diagnostics.
fn every_path(
    solver: &mut ClothSolver,
    state: &mut ClothState,
    hook: &mut impl SubstepHook,
    waist: &[Vec3],
    targets: &mut [Vec3],
    rest: &[Vec3],
    frame: usize,
) {
    let dt = [1.0 / 60.0, 1.0 / 144.0, 1.0 / 30.0, 0.0, 1.0 / 1000.0, f32::NAN, 1.0e9, 1.0 / 240.0][frame % 8];
    waist_into(waist, frame as f64 / 60.0, targets);
    if frame.is_multiple_of(50) {
        targets[3] = Vec3::NAN;
    }
    if frame.is_multiple_of(100) {
        state.positions_mut()[60] = Vec3A::NAN;
    }
    match frame % 4 {
        0 => solver.step(dt, state, targets).map(|_| ()),
        1 => solver.step_with(dt, state, targets, hook).map(|_| ()),
        2 => solver.advance(3, state, targets, hook).map(|_| ()),
        _ if frame % 400 == 3 => solver.reset(state, rest),
        _ => solver.step(dt, state, targets).map(|_| ()),
    }
    .unwrap();
    std::hint::black_box((solver.energy(state), solver.max_strain(state), solver.counts()));
}

#[test]
fn a_step_allocates_nothing() {
    // Everything that allocates is built before the counter starts; one warm-up call first.
    let sheet = skirt(16, 24);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let (waist, rest) = (targets_of(&state), sheet.rest().to_vec());
    let mut targets = waist.clone();
    let mut hook = FloorAndWind { wind: Vec3A::X };
    every_path(&mut solver, &mut state, &mut hook, &waist, &mut targets, &rest, 0);
    let count = allocations_in(|| {
        for frame in 1..=1200 {
            every_path(&mut solver, &mut state, &mut hook, &waist, &mut targets, &rest, frame);
        }
    });
    assert_eq!(count, 0, "1200 calls made {count} allocations");
}

/// A hook that allocates once per sub-step.
struct Allocating;

impl SubstepHook for Allocating {
    fn project(&mut self, _: &Substep, _: &mut [Vec3A], _: &[Vec3A], _: &[f32]) {
        std::hint::black_box(Vec::<u8>::with_capacity(16));
    }
}

#[test]
fn the_counter_catches_an_injected_allocation() {
    // The proof above is only as good as the counter: a hook that allocates once per sub-step must be seen.
    let sheet = skirt(4, 6);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let targets = targets_of(&state);
    solver.step_with(STEP, &mut state, &targets, &mut NoHook).unwrap();
    let count = allocations_in(|| {
        for _ in 0..10 {
            solver.step_with(STEP, &mut state, &targets, &mut Allocating).unwrap();
        }
    });
    assert_eq!(count, 10 * crate::preset().substeps as usize, "10 steps, one allocation per sub-step");
}

// ---------------------------------------------------------------------------------------------------------------------
// Fuzz: 64 random sheets x 15,625 calls = 1e6 steps with random pins (hard, soft, immovable), parameters, sub-steps,
// iterations, frame times (hostile ones included) and pin motion (teleports and NaN included).

/// A tiny deterministic generator for the per-step randomness of a fuzz case.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

/// A compliance from a code: 0 (rigid), infinite (off), or 10^(-9..1).
fn compliance(code: u8, exponent: f32) -> f32 {
    match code {
        0 => 0.0,
        1 => f32::INFINITY,
        _ => 10f32.powf(exponent),
    }
}

const FUZZ_CASES: u32 = 64;
const FUZZ_STEPS: usize = 15_625;

/// Checks the limiter's promise where it holds exactly, right after its pass in a frame's last sub-step: each particle is within
/// `reach` of a root (a pinned or immovable particle). Every other particle hangs from a root by a chain of at most
/// `rows + cols` edges that the limiter, visiting them from the roots out, has just held to `1 + max_stretch` times
/// their rest length. (Later in a call only hard pins move, onto their targets.)
struct Reach {
    roots: Vec<usize>,
    limit: f32,
    breach: Option<String>,
}

impl SubstepHook for Reach {
    fn project(&mut self, substep: &Substep, positions: &mut [Vec3A], _: &[Vec3A], _: &[f32]) {
        // Once a frame (its last sub-step) keeps the million-step fuzz affordable in a debug build.
        if self.breach.is_some() || self.roots.is_empty() || substep.fraction < 1.0 {
            return;
        }
        for (i, x) in positions.iter().enumerate() {
            let nearest = self.roots.iter().map(|&r| positions[r].distance(*x)).fold(f32::MAX, f32::min);
            if nearest.is_nan() || nearest > self.limit {
                self.breach = Some(format!("sub-step {}: particle {i} is {nearest} m from any root", substep.index));
                return;
            }
        }
    }
}

proptest! {
    // A fixed seed: CI explores the same million steps every run, and a failure reproduces.
    #![proptest_config(ProptestConfig {
        cases: FUZZ_CASES,
        failure_persistence: None,
        rng_seed: proptest::test_runner::RngSeed::Fixed(544),
        ..ProptestConfig::default()
    })]

    #[test]
    fn random_sheets_pins_and_parameters_never_explode(
        rows in 2usize..=5,
        cols in 2usize..=5,
        wrap in any::<bool>(),
        kinds in prop::collection::vec(0u8..11, 25),
        codes in prop::array::uniform3(0u8..4),
        exponents in prop::array::uniform3(-9.0f32..1.0),
        damping in 0.0f32..50.0,
        gravity in prop::array::uniform3(-50.0f32..50.0),
        max_stretch in 0.0f32..1.0,
        substeps in 1u32..=8,
        iterations in 1u32..=4,
        seed in any::<u64>(),
    ) {
        let shape = SheetShape { rows, cols, wrap: wrap && cols >= 3 };
        let n = rows * cols;
        let rest: Vec<Vec3> = (0..n).map(|i| Vec3::new((i % cols) as f32 * 0.05, -((i / cols) as f32) * 0.05, 0.01 * (i % 3) as f32)).collect();
        // 0-5 free, 6 immovable, 7-8 hard pin, 9 soft pin, 10 a soft pin on an immovable particle.
        let inv_mass = kinds[..n].iter().map(|&k| if k == 6 || k == 10 { 0.0 } else { 2000.0 }).collect();
        let weights: Vec<f32> = kinds[..n].iter().enumerate().map(|(i, &k)| match k { 7 | 8 => 1.0, 9 | 10 => 0.1 + 0.8 * (i as f32 / n as f32), _ => 0.0 }).collect();
        let sheet = ClothSheet::new(shape, rest, inv_mass, weights).unwrap();
        let params = ClothParams {
            stretch: compliance(codes[0], exponents[0]).min(1.0),
            shear: compliance(codes[1], exponents[1]),
            bend: compliance(codes[2], exponents[2] + 6.0),
            damping,
            gravity: Vec3::from_array(gravity),
            max_stretch,
            ..ClothParams::default()
        };
        let mut solver = ClothSolver::new(&sheet, &params, &SolverConfig { substeps, iterations, ..crate::preset() }).unwrap();
        let mut state = solver.rest_state();
        let home = targets_of(&state);
        let mut targets = home.clone();
        let mut rng = Lcg(seed);
        let longest_edge = 0.05 * cols as f32 + 0.03; // a wrapped flat sheet's seam spans its width
        let mut reach = Reach {
            roots: (0..n).filter(|&i| solver.inv_mass()[i] == 0.0 || sheet.pin_weights()[i] > 0.0).collect(),
            limit: (1.0 + max_stretch) * (rows + cols) as f32 * longest_edge + 1e-4,
            breach: None,
        };
        for step in 0..FUZZ_STEPS {
            // Mostly 120 Hz frames; one in 64 a hostile or extreme one (the capped hitch, the costliest, one in 512).
            let dt = match (rng.next() * 512.0) as u32 {
                0 => 1.0e6,
                1..=2 => 1.0 / 20.0,
                3..=8 => 0.0,
                9..=14 => f32::NAN,
                15..=20 => -1.0,
                21..=28 => 1.0 / 240.0,
                _ => STEP,
            };
            for (t, &h) in targets.iter_mut().zip(&home) {
                *t = match (rng.next() * 400.0) as u32 {
                    0 => h + Vec3::new(10.0 * rng.next() - 5.0, 10.0 * rng.next() - 5.0, 0.0), // teleport
                    1 => Vec3::NAN,
                    _ => h + 0.1 * Vec3::new((step as f32 * 0.05).sin(), (step as f32 * 0.031).cos(), 0.0),
                };
            }
            let report = solver.step_with(dt, &mut state, &targets, &mut reach).unwrap();
            prop_assert!(reach.breach.is_none(), "step {}: {:?}", step, reach.breach);
            // Hostile targets are refused, never absorbed: nothing in a valid sheet may blow up and need a reset.
            prop_assert_eq!(report.reset_particles, 0, "step {}", step);
            for (x, v) in state.positions().iter().zip(state.velocities()) {
                prop_assert!(x.is_finite() && x.abs().max_element() <= MAX_COORDINATE, "step {}: position {}", step, x);
                prop_assert!(v.is_finite() && v.length() <= MAX_SPEED * 1.0001, "step {}: velocity {}", step, v);
            }
            for (slot, &p) in sheet.pins().iter().enumerate() {
                if sheet.pin_weights()[p as usize] >= 1.0 {
                    prop_assert_eq!(state.positions()[p as usize], state.targets()[slot]);
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------------------------------
// Evidence.

/// Writes the settling skirt for the PR's contact sheet: `CLOTH_SEQUENCE_OUT=<file> cargo test -p omuzaki-char --release
/// --test cloth_solver -- --ignored dump_skirt_sequence`. One line per frame at 60 fps: time, then every particle's x y z.
/// The skirt starts at its flared rest shape (as cut), falls and settles for 3 s, then the hips dance for 3 s.
#[test]
#[ignore = "writes evidence files; run by hand"]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn dump_skirt_sequence() {
    use std::fmt::Write as _;
    let path = std::env::var("CLOTH_SEQUENCE_OUT").expect("set CLOTH_SEQUENCE_OUT to the output file");
    let sheet = skirt(16, 24);
    let mut solver = solver(&sheet, ClothParams::default(), crate::preset());
    let mut state = solver.rest_state();
    let waist = targets_of(&state);
    let mut targets = waist.clone();
    let mut out = String::new();
    for frame in 0..=360 {
        let t = f64::from(frame) / 60.0;
        if t > 3.0 {
            waist_into(&waist, t - 3.0, &mut targets);
        }
        if frame > 0 {
            solver.step(1.0 / 60.0, &mut state, &targets).unwrap();
        }
        write!(out, "{t:.4}").unwrap();
        for x in state.positions() {
            write!(out, " {:.5} {:.5} {:.5}", x.x, x.y, x.z).unwrap();
        }
        writeln!(out).unwrap();
    }
    std::fs::write(path, out).unwrap();
}
