//! The XPBD particle-sheet solver (spec RG-04): robes, skirts, capes, sleeves and ribbons as grids of particles held
//! by stretch, shear and bend constraints, pinned to targets that follow the skeleton.
//!
//! # Formulation
//!
//! Extended position-based dynamics, XPBD (Macklin, Müller and Chentanez 2016, *XPBD: Position-Based Simulation of
//! Compliant Constrained Dynamics*, MIG '16), run as many short sub-steps with few iterations each (Macklin et al.
//! 2019, *Small Steps in Physics Simulation*, SCA '19). Each sub-step of length `h`:
//!
//! 1. moves the pins to their targets for that instant (hard pins exactly; soft pins through their constraint);
//! 2. lets a [`SubstepHook`] change velocities (wind), then integrates gravity: `v += h g`, `x += h v`;
//! 3. sweeps the constraints `iterations` times in a fixed block Gauss-Seidel order, alternating direction: every
//!    column and row of warp and weft edges solved as a chain (exact for its linearisation), then shear, bend and soft
//!    pins one by one, all with the XPBD update of the paper's eqs. 17 and 18 (`λ` restarts at zero each sub-step;
//!    [`constraints`](super::constraints) lists the constraints, their compliances and units);
//! 4. runs the stretch limiter, then the hook's contact projection;
//! 5. sets `v = (x - x_prev) / h`, clamps it to [`MAX_SPEED`] and decays it by `exp(-damping h)`.
//!
//! # Time
//!
//! A frame's `dt` is consumed in whole sub-steps of `1 / (120 · substeps)` seconds ([`PHYSICS_HZ`], decision 0009);
//! the remainder is owed to the next frame, so the result depends on elapsed time, not on the frame rate. A frame
//! longer than [`SolverConfig::max_frame_s`] runs only that much and reports the rest as dropped; a negative, NaN or
//! infinite `dt` runs nothing and is reported. [`ClothSolver::advance`] runs an exact number of sub-steps instead, for
//! offline renders clocked in sub-steps.
//!
//! Pin targets are given once per call, for the instant the frame ends, and after the call every hard pin sits
//! **exactly** on its target (no gap opens between a waistband and the body at any frame rate). Within the call each
//! sub-step's targets follow the parabola through the last three frames' targets (no look-ahead), so a pin's velocity
//! does not jump at every frame boundary: a cape on walking hips ends 2 s within 0.05 mm of the same run at 120 fps
//! whether stepped at 30 or 240 fps. A frame more than twice as long or short as
//! the one before, or the first after a reset, interpolates linearly. The parabola can overshoot by an eighth of a
//! teleport on the frame after it; a cut should [`reset`](ClothSolver::reset) instead.
//!
//! # Accuracy
//!
//! - **The material its [`ClothParams`] describe, at the live budget.** Warp and weft are solved as exact chains, so
//!   4 sub-steps x 2 iterations already hang a curtain to its closed-form stretch and a sheet to its exact elastic
//!   catenary within 0.02%. (Point Gauss-Seidel alone left the classic few-iteration bias: 9% extra stretch.)
//! - **Resolution independent at the live budget.** The per-constraint compliances keep the material and the chain
//!   solves keep the convergence, whatever the particle count: twice the rows and columns hang within 0.1% of the
//!   coarser sheet (a soft hammock -0.04%, -0.06% at four times; a curtain 0.00%), a skirt's hem within 0.3%. (Point
//!   Gauss-Seidel: +15% at twice, +64% at four times, growing with the edge count.)
//! - **Shear, bend and soft pins** are projected one by one, as in plain XPBD: the stiffest settings resolve slowest,
//!   and more iterations (offline quality) converge them.
//! - **The stretch limiter** never leaves an edge that leads toward a pin longer than `1 + max_stretch` (one pass,
//!   ordered from the pins out). Edges between particles equally far from the pins (a skirt's hoops) are shortened in
//!   the same pass but held mostly by their own stiffness: a stiff fabric spun at 2 turns a second stays within 2.2%; a
//!   rubber one opens its hoops much further. With the chain solves it is a safety net for violent motion.
//! - **Energy.** XPBD is an implicit Euler step, which only loses energy while every constraint's energy is convex: a
//!   kicked sheet swinging in tension never gains energy from one frame to the next, damped or not, whether the fabric
//!   is soft, the default or stiff. Edges in compression and bends about a curved rest are not convex: a buckling skirt
//!   can turn released compression into motion for a moment, and for a stiff fabric `C² / 2α` then mostly measures the
//!   residual of its one-by-one shear and bend. It settles all the same.
//!
//! - **Presets.** [`SolverConfig::quality`] (4 sub-steps x 2 sweeps, the default) is what the figures above are measured
//!   at. [`SolverConfig::live`] (3 x 2) costs 25% less and passes the whole solver suite (`tests/cloth_live.rs`): the
//!   closed forms, the energy tests of soft, default and stiff fabrics, frame-rate invariance, hostile input. It
//!   opens the hoops of absurd fabrics (rubber, a thousand g) further in a hard spin, so those cases get twice the
//!   room there. Two sub-steps fail the stiff-fabric energy test and one sweep fails it outright, so 3 x 2 is the
//!   cheapest that passes.
//!
//! # Robustness
//!
//! - Construction validates the sheet and the parameters ([`ClothError`]); `step` only fails on a state or target list
//!   of the wrong length.
//! - A NaN, infinite or absurdly distant ([`MAX_COORDINATE`]) pin target is replaced by the pin's previous target.
//! - A particle that goes non-finite or beyond [`MAX_COORDINATE`] in a sub-step (or arrives so in the state) is reset
//!   — to where it started the sub-step, or to its rest place relative to its nearest pin — with zero velocity. Both
//!   are counted in the [`StepReport`].
//! - The order of every loop is fixed, so the same inputs give bit-identical states on the same build (decision 0009).
//!
//! # Cost
//!
//! [`ClothSolver::step`] never allocates or locks: [`ClothSolver::new`] sizes every buffer. A sub-step costs
//! `iterations ×` (one chain solve per row and column, and one projection per shear, bend and soft-pin constraint),
//! plus one limiter pass. Particles are SIMD `Vec3A`s, chains are solved in step so their recurrences overlap, and the
//! one-by-one constraints are ordered so consecutive projections share no particle: a 24 x 16 skirt (384 particles)
//! takes about 0.08 ms per 1/120 s step (`cargo bench -p omuzaki-char --bench cloth_solver`).

use omuzaki_anim_api::PHYSICS_HZ;
use omuzaki_core::glam::{Vec3, Vec3A};
use serde::{Deserialize, Serialize};

pub use super::constraints::{ConstraintCounts, SheetShape};
use super::constraints::{Constraints, Inputs, particle_areas, smallest_features};

/// The fastest a particle may move, m/s. Far above any character motion (a sleeve on a fast spin moves under 20 m/s),
/// it only matters when a pin teleports: the sheet then follows over a few frames instead of being flung.
pub const MAX_SPEED: f32 = 100.0;

/// The largest coordinate a pin target or particle may have, metres. A target beyond it is refused like a NaN and a
/// particle beyond it is reset: squared lengths of such distances leave `f32`'s range, and no character is simulated
/// 100 km from its origin.
pub const MAX_COORDINATE: f32 = 1.0e5;

/// Rest edges shorter than this (metres) are refused: their constraints would divide by almost nothing.
pub const MIN_REST_LENGTH: f32 = 1.0e-5;

/// Rest cells smaller than this (square metres) are refused: masses and compliances divide by cell areas.
pub const MIN_CELL_AREA: f32 = 1.0e-10;

/// Most sub-steps per physics step and most iterations per sub-step a [`SolverConfig`] may ask for.
pub const MAX_SUBSTEPS: u32 = 64;

/// The live preset's sub-steps per physics step and sweeps per sub-step.
const LIVE_SUBSTEPS: u32 = 3;
const LIVE_ITERATIONS: u32 = 2;

/// A fraction of a sub-step forgiven when counting due sub-steps: a frame time like `1/60` is not exact in `f32`, and
/// without the slack a frame could owe `7.9999` sub-steps, run seven, and run nine the next.
const SUBSTEP_SLACK: f64 = 1e-4;

/// A particle with no pin to reset relative to.
const NO_ANCHOR: u32 = u32::MAX;

/// Why a sheet, its parameters or a step's inputs were refused.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ClothError {
    /// Too few rows or columns for a sheet (2 x 2 at least, 3 columns for a wrapped one).
    #[error("a sheet needs at least {need} {what}, got {got}")]
    TooSmall {
        /// Rows or columns.
        what: &'static str,
        /// The least accepted.
        need: usize,
        /// What was given.
        got: usize,
    },
    /// More particles than the solver can index.
    #[error("a sheet of {rows} x {cols} particles is too large")]
    TooLarge {
        /// Rows asked for.
        rows: usize,
        /// Columns asked for.
        cols: usize,
    },
    /// A list whose length does not match the sheet (rest positions, masses, pin weights, a state, pin targets).
    #[error("{what}: expected {expected}, got {got}")]
    Length {
        /// Which list.
        what: &'static str,
        /// Its required length.
        expected: usize,
        /// Its length.
        got: usize,
    },
    /// A per-particle value that is NaN, infinite or outside its range.
    #[error("{what} of particle {index} is {value}, expected {expected}")]
    Particle {
        /// Which value.
        what: &'static str,
        /// The particle.
        index: usize,
        /// The value (the first bad coordinate for a position).
        value: f32,
        /// The accepted range.
        expected: &'static str,
    },
    /// A rest edge or cell too small to simulate ([`MIN_REST_LENGTH`], [`MIN_CELL_AREA`]).
    #[error("rest {what} {index} measures {value}, too small to simulate")]
    Degenerate {
        /// Edge or cell.
        what: &'static str,
        /// Its index in row order.
        index: usize,
        /// Its length or area.
        value: f32,
    },
    /// A parameter that is NaN or out of range.
    #[error("{name} is {value}, expected {expected}")]
    Param {
        /// The field.
        name: &'static str,
        /// Its value.
        value: f32,
        /// The accepted range.
        expected: &'static str,
    },
}

/// A cloth sheet at rest: a grid of particles with masses and pins. Built once per garment; the solver copies what it
/// needs.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothSheet {
    shape: SheetShape,
    rest: Vec<Vec3>,
    inv_mass: Vec<f32>,
    pin_weights: Vec<f32>,
    pins: Vec<u32>,
}

/// Checks that a per-particle list has one value per particle.
fn check_len<T>(what: &'static str, list: &[T], expected: usize) -> Result<(), ClothError> {
    if list.len() == expected { Ok(()) } else { Err(ClothError::Length { what, expected, got: list.len() }) }
}

/// The first particle whose value fails `ok`, as a [`ClothError::Particle`].
fn check_each(
    what: &'static str,
    values: impl Iterator<Item = f32>,
    ok: impl Fn(f32) -> bool,
    expected: &'static str,
) -> Result<(), ClothError> {
    match values.enumerate().find(|&(_, v)| !ok(v)) {
        Some((index, value)) => Err(ClothError::Particle { what, index, value, expected }),
        None => Ok(()),
    }
}

/// Checks positions are finite and inside [`MAX_COORDINATE`]; reports the first bad coordinate.
fn check_positions(what: &'static str, positions: &[Vec3]) -> Result<(), ClothError> {
    let worst = positions.iter().map(|p| if p.is_finite() { p.abs().max_element() } else { f32::NAN });
    check_each(what, worst, |v| v <= MAX_COORDINATE, "finite, within 1e5 m of the origin")
}

impl ClothSheet {
    /// A sheet of `shape.rows x shape.cols` particles stored row by row, with explicit inverse masses (kg⁻¹; 0 makes
    /// a particle immovable) and pin weights (0 free, 1 a hard pin that follows its target exactly, in between a soft
    /// pin: see [`constraints`](super::constraints)).
    ///
    /// # Errors
    /// [`ClothError`] for a sheet smaller than 2 x 2 (3 columns when wrapped), too many particles, lists of the wrong
    /// length, a non-finite or distant rest position, a negative or non-finite inverse mass, a pin weight outside
    /// `[0, 1]`, or a rest edge or cell too small to simulate.
    pub fn new(
        shape: SheetShape,
        rest: Vec<Vec3>,
        inv_mass: Vec<f32>,
        pin_weights: Vec<f32>,
    ) -> Result<Self, ClothError> {
        let count = check_geometry(shape, &rest)?;
        check_len("inverse masses", &inv_mass, count)?;
        check_len("pin weights", &pin_weights, count)?;
        check_each("inverse mass", inv_mass.iter().copied(), |w| w.is_finite() && w >= 0.0, "finite and >= 0")?;
        check_each("pin weight", pin_weights.iter().copied(), |p| (0.0..=1.0).contains(&p), "in [0, 1]")?;
        #[allow(clippy::cast_possible_truncation)] // `particle_count` keeps every index below u32::MAX
        let pins = pin_weights.iter().enumerate().filter(|(_, w)| **w > 0.0).map(|(i, _)| i as u32).collect();
        Ok(Self { shape, rest, inv_mass, pin_weights, pins })
    }

    /// A sheet of uniform fabric: each particle's mass is `density` (kg/m²) times its rest area (a quarter of each
    /// cell around it), so the sheet weighs `density` times its area at any resolution.
    ///
    /// # Errors
    /// As [`ClothSheet::new`], and [`ClothError::Param`] for a density that is not finite and positive.
    pub fn with_density(
        shape: SheetShape,
        rest: Vec<Vec3>,
        density: f32,
        pin_weights: Vec<f32>,
    ) -> Result<Self, ClothError> {
        if !(density.is_finite() && density > 0.0) {
            return Err(ClothError::Param { name: "density", value: density, expected: "finite and > 0 (kg/m²)" });
        }
        check_geometry(shape, &rest)?;
        let inv_mass = particle_areas(shape, &rest).into_iter().map(|area| 1.0 / (density * area)).collect();
        Self::new(shape, rest, inv_mass, pin_weights)
    }

    /// The grid's rows, columns and seam.
    pub fn shape(&self) -> SheetShape {
        self.shape
    }

    /// Number of particles, `rows * cols`.
    pub fn particle_count(&self) -> usize {
        self.rest.len()
    }

    /// The particle at `row`, `col`, if inside the grid.
    pub fn index(&self, row: usize, col: usize) -> Option<usize> {
        (row < self.shape.rows && col < self.shape.cols).then_some(row * self.shape.cols + col)
    }

    /// Rest positions, row by row.
    pub fn rest(&self) -> &[Vec3] {
        &self.rest
    }

    /// Inverse masses, kg⁻¹, as given (hard pins included).
    pub fn inv_mass(&self) -> &[f32] {
        &self.inv_mass
    }

    /// Pin weights per particle.
    pub fn pin_weights(&self) -> &[f32] {
        &self.pin_weights
    }

    /// The pinned particles (weight above 0), ascending. A step's targets are given in this order.
    pub fn pins(&self) -> &[u32] {
        &self.pins
    }
}

/// Checks the shape and the rest positions (count, finite, no degenerate edge or cell); returns the particle count.
fn check_geometry(shape: SheetShape, rest: &[Vec3]) -> Result<usize, ClothError> {
    let count = particle_count(shape)?;
    check_len("rest positions", rest, count)?;
    check_positions("rest position", rest)?;
    let (shortest, smallest) = smallest_features(shape, rest);
    if let Some((index, value)) = shortest.filter(|&(_, length)| length < MIN_REST_LENGTH) {
        return Err(ClothError::Degenerate { what: "edge", index, value });
    }
    if let Some((index, value)) = smallest.filter(|&(_, area)| area < MIN_CELL_AREA) {
        return Err(ClothError::Degenerate { what: "cell", index, value });
    }
    Ok(count)
}

/// The particle count of a valid shape.
fn particle_count(shape: SheetShape) -> Result<usize, ClothError> {
    if shape.rows < 2 {
        return Err(ClothError::TooSmall { what: "rows", need: 2, got: shape.rows });
    }
    let need = if shape.wrap { 3 } else { 2 };
    if shape.cols < need {
        return Err(ClothError::TooSmall { what: "columns", need, got: shape.cols });
    }
    let too_large = ClothError::TooLarge { rows: shape.rows, cols: shape.cols };
    let count = shape.rows.checked_mul(shape.cols).ok_or(too_large.clone())?;
    if count >= u32::MAX as usize { Err(too_large) } else { Ok(count) }
}

/// The fabric: compliances, damping and gravity. Every field is a physical quantity of the material, independent of
/// the sheet's resolution ([`constraints`](super::constraints) shows how each becomes per-constraint compliances), so
/// a sheet subdivided for an offline render behaves like the live one: at the live budget, twice the rows and columns
/// hang within 0.3% of the coarser sheet (see Accuracy in the [module docs](self)). An infinite compliance turns its
/// family off.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ClothParams {
    /// Stretch compliance, m/N: `1 / (E·t)`, the inverse of the fabric's tensile stiffness along warp and weft.
    pub stretch: f32,
    /// Shear compliance, m/N: `1 / (G·t)`, the inverse of its in-plane shear stiffness (the diagonals).
    pub shear: f32,
    /// Bend compliance, 1/(N·m): `1 / D`, the inverse of its bending stiffness per unit width.
    pub bend: f32,
    /// How fast velocity decays, 1/s: each sub-step keeps `exp(-damping h)` of it (drag in still air; wind is added by
    /// a hook).
    pub damping: f32,
    /// Gravity in the solver's space, m/s².
    pub gravity: Vec3,
    /// Half the fabric's thickness, metres: how far contacts keep particles from colliders.
    pub thickness: f32,
    /// The stretch limiter: no warp or weft edge ends a sub-step longer than `1 + max_stretch` times its rest length
    /// (infinite: no limit).
    pub max_stretch: f32,
}

impl Default for ClothParams {
    /// A medium cotton, slightly stiff in bending as toon skirts are: tensile stiffness 1e4 N/m, shear 1e3 N/m,
    /// bending 5e-5 N·m (a 3 cm bending length at 0.2 kg/m²), damping 2/s (still air's drag on a light fabric moving
    /// at about 1 m/s, linearised), 5% stretch limit, 2 mm half-thickness.
    fn default() -> Self {
        Self {
            stretch: 1.0e-4,
            shear: 1.0e-3,
            bend: 2.0e4,
            damping: 2.0,
            gravity: Vec3::new(0.0, -9.81, 0.0),
            thickness: 0.002,
            max_stretch: 0.05,
        }
    }
}

impl ClothParams {
    /// Checks every field is in range.
    ///
    /// # Errors
    /// [`ClothError::Param`] naming the first field that is NaN or out of range: compliances must be `>= 0` (infinity
    /// allowed), damping and thickness finite and `>= 0`, gravity finite, `max_stretch >= 0` (infinity allowed).
    pub fn validate(&self) -> Result<(), ClothError> {
        let param = |name, value, expected| Err(ClothError::Param { name, value, expected });
        for (name, value) in [("stretch", self.stretch), ("shear", self.shear), ("bend", self.bend)] {
            if value.is_nan() || value < 0.0 {
                return param(name, value, ">= 0 (infinity turns it off)");
            }
        }
        for (name, value) in [("damping", self.damping), ("thickness", self.thickness)] {
            if !(value.is_finite() && value >= 0.0) {
                return param(name, value, "finite and >= 0");
            }
        }
        if let Some(bad) = self.gravity.to_array().into_iter().find(|g| !g.is_finite()) {
            return param("gravity", bad, "finite");
        }
        if self.max_stretch.is_nan() || self.max_stretch < 0.0 {
            return param("max_stretch", self.max_stretch, ">= 0 (infinity turns the limiter off)");
        }
        Ok(())
    }
}

/// How finely the solver steps: the quality knobs (`Quality::Offline` asks for more of each).
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SolverConfig {
    /// XPBD sub-steps per 1/120 s physics step, 1 to [`MAX_SUBSTEPS`] (default 4: a 480 Hz sub-step).
    pub substeps: u32,
    /// Gauss-Seidel sweeps over the constraints per sub-step, 1 to [`MAX_SUBSTEPS`] (default 2).
    pub iterations: u32,
    /// The longest frame simulated in full, seconds (default 1/15): a longer hitch runs this much and drops the rest,
    /// so one stalled frame cannot stall the next.
    pub max_frame_s: f32,
}

impl Default for SolverConfig {
    /// The quality preset ([`SolverConfig::quality`]).
    fn default() -> Self {
        Self::quality()
    }
}

impl SolverConfig {
    /// The quality preset: 4 sub-steps of 2 sweeps per physics step (a 480 Hz sub-step), the budget the accuracy
    /// section of the [module docs](self) is measured at. For stills, offline renders and stiff fabrics on a machine
    /// that can afford it.
    pub const fn quality() -> Self {
        Self { substeps: 4, iterations: 2, max_frame_s: 1.0 / 15.0 }
    }

    /// The live preset: the cheapest sub-steps x sweeps that still passes every fabric test (`tests/cloth_live.rs`
    /// runs the whole solver suite with it), chosen by the tests.
    pub const fn live() -> Self {
        Self { substeps: LIVE_SUBSTEPS, iterations: LIVE_ITERATIONS, max_frame_s: 1.0 / 15.0 }
    }

    /// Checks every field is in range.
    ///
    /// # Errors
    /// [`ClothError::Param`] for sub-steps or iterations outside `1..=MAX_SUBSTEPS`, or a `max_frame_s` shorter than
    /// one sub-step, NaN or above 1 s.
    pub fn validate(&self) -> Result<(), ClothError> {
        let count = |name, value: u32| {
            #[allow(clippy::cast_precision_loss)] // only reported
            let err = ClothError::Param { name, value: value as f32, expected: "1 to 64" };
            if (1..=MAX_SUBSTEPS).contains(&value) { Ok(()) } else { Err(err) }
        };
        count("substeps", self.substeps)?;
        count("iterations", self.iterations)?;
        let h = 1.0 / (f64::from(PHYSICS_HZ) * f64::from(self.substeps));
        if !(f64::from(self.max_frame_s) >= h && self.max_frame_s <= 1.0) {
            return Err(ClothError::Param {
                name: "max_frame_s",
                value: self.max_frame_s,
                expected: "one sub-step to 1 s",
            });
        }
        Ok(())
    }
}

/// Everything that evolves: particle positions and velocities (SIMD `Vec3A`s), the last two frames' pin targets, the
/// time owed to the next frame and the sub-step count. Cloning it is a snapshot; a snapshot restored and stepped with
/// the same inputs replays bit for bit.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothState {
    positions: Vec<Vec3A>,
    velocities: Vec<Vec3A>,
    targets: Vec<Vec3A>,
    /// The targets one frame before `targets`, and the time between the two (0 when there is no usable history).
    earlier: Vec<Vec3A>,
    span_s: f64,
    owed_s: f64,
    substep: u64,
}

impl ClothState {
    /// Particle positions in the solver's space, row by row.
    pub fn positions(&self) -> &[Vec3A] {
        &self.positions
    }

    /// Particle velocities, m/s.
    pub fn velocities(&self) -> &[Vec3A] {
        &self.velocities
    }

    /// Positions to edit in place (a drag, a test's displacement). A NaN written here is reset by the next step.
    pub fn positions_mut(&mut self) -> &mut [Vec3A] {
        &mut self.positions
    }

    /// Velocities to edit in place (an impulse).
    pub fn velocities_mut(&mut self) -> &mut [Vec3A] {
        &mut self.velocities
    }

    /// The pin targets the last step ended at, in [`ClothSheet::pins`] order.
    pub fn targets(&self) -> &[Vec3A] {
        &self.targets
    }

    /// Sub-steps run since the state was made or reset: the simulation clock in sub-steps.
    pub fn substep(&self) -> u64 {
        self.substep
    }

    /// Time owed to the next frame, seconds (less than one sub-step).
    pub fn owed_s(&self) -> f64 {
        self.owed_s
    }
}

/// Where a sub-step sits, for a hook that moves colliders or varies wind with time.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Substep {
    /// Sub-steps the state ran before this one ([`ClothState::substep`]).
    pub index: u64,
    /// Its length, seconds.
    pub h: f32,
    /// How far through the call's frame the sub-step ends, in `(0, 1]`: the pins are at
    /// `previous.lerp(current, fraction)`, and colliders that follow the pose should be too.
    pub fraction: f32,
}

/// Extra physics inside each sub-step (contacts and wind). Both methods default to doing nothing, and a hook is
/// a generic parameter, so an empty one costs nothing.
pub trait SubstepHook {
    /// Before integration: change velocities (wind drag and lift as `h · a`). Positions are where the sub-step starts.
    fn accelerate(&mut self, substep: &Substep, positions: &[Vec3A], velocities: &mut [Vec3A], inv_mass: &[f32]) {
        let _ = (substep, positions, velocities, inv_mass);
    }

    /// After the constraint iterations and the stretch limiter, before velocities are derived: move positions out of
    /// colliders. `previous` holds the positions the sub-step started from, for friction. Leave particles whose inverse
    /// mass is 0 where they are.
    fn project(&mut self, substep: &Substep, positions: &mut [Vec3A], previous: &[Vec3A], inv_mass: &[f32]) {
        let _ = (substep, positions, previous, inv_mass);
    }
}

/// The hook that adds nothing: [`ClothSolver::step`] uses it.
#[derive(Copy, Clone, Debug, Default)]
pub struct NoHook;

impl SubstepHook for NoHook {}

/// What a step did, beyond moving the sheet.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct StepReport {
    /// Sub-steps run.
    pub substeps: u32,
    /// Frame time not simulated because it exceeded [`SolverConfig::max_frame_s`], seconds.
    pub dropped_s: f32,
    /// The frame time was negative, NaN or infinite, so nothing ran.
    pub rejected_dt: bool,
    /// Pin targets refused (NaN, infinite or beyond [`MAX_COORDINATE`]) and replaced by the pin's previous target.
    pub rejected_targets: u32,
    /// Particles reset after going (or arriving) non-finite or beyond [`MAX_COORDINATE`].
    pub reset_particles: u32,
}

/// The sheet's energy, joules, for tests and diagnostics: kinetic, gravitational (zero at the origin) and elastic
/// (`Σ C² / 2α` over the compliant constraints). Hard pins and immovable particles carry none.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct ClothEnergy {
    /// `Σ ½ m v²`.
    pub kinetic: f64,
    /// `-Σ m g·x`.
    pub gravity: f64,
    /// `Σ C² / 2α`.
    pub elastic: f64,
}

impl ClothEnergy {
    /// Kinetic plus gravitational plus elastic.
    pub fn total(&self) -> f64 {
        self.kinetic + self.gravity + self.elastic
    }
}

/// How a call's sub-steps map onto its frame, for interpolating pin targets.
#[derive(Copy, Clone, Debug)]
enum Timing {
    /// Sub-step `k` of `n` ends at `k / n` of a frame `n h` long (an exact count, or a frame cut short by the cap).
    Even { h: f64 },
    /// By the clock: sub-step `k` ends `k h - owed` seconds into a frame of `dt` (`owed` from the previous frame).
    Clock { owed_s: f64, h: f64, dt: f64 },
}

impl Timing {
    /// The frame's length, seconds, for `n` sub-steps.
    fn duration(self, n: u32) -> f64 {
        match self {
            Timing::Even { h } => f64::from(n) * h,
            Timing::Clock { dt, .. } => dt,
        }
    }

    /// The fraction of the frame at which sub-step `k` (1-based) of `n` ends. A sub-step that ends within rounding of
    /// the frame's end (the slack that let it run) is the frame's end.
    #[allow(clippy::cast_possible_truncation)] // a fraction in [0, 1]
    fn fraction(self, k: u32, n: u32) -> f32 {
        let fraction = match self {
            Timing::Even { .. } => f64::from(k) / f64::from(n),
            Timing::Clock { owed_s, h, dt } => {
                let fraction = (f64::from(k) * h - owed_s) / dt;
                if (1.0 - fraction) * dt <= SUBSTEP_SLACK * h { 1.0 } else { fraction }
            }
        };
        fraction.clamp(0.0, 1.0) as f32
    }
}

/// The weights of the earlier, start and end targets at `fraction` of a frame. With `ratio` (the previous frame's
/// length over this one's) the path is the parabola through the three samples (Lagrange's form), so a pin's velocity
/// carries smoothly across frame boundaries; without, a straight line.
#[allow(clippy::cast_possible_truncation)] // weights of magnitude about 1
fn path_weights(fraction: f32, ratio: Option<f64>) -> [f32; 3] {
    let s = f64::from(fraction);
    let [w0, w1, w2] = match ratio {
        Some(r) => [s * (s - 1.0) / (r * (r + 1.0)), -(s + r) * (s - 1.0) / r, (s + r) * s / (1.0 + r)],
        None => [0.0, 1.0 - s, s],
    };
    [w0 as f32, w1 as f32, w2 as f32]
}

/// Is `p` usable as a position or pin target: finite, and near enough the origin that squared distances stay in range?
fn usable(p: Vec3A) -> bool {
    p.is_finite() && p.abs().max_element() <= MAX_COORDINATE
}

/// Steps a [`ClothSheet`] with XPBD (see the [module docs](self)). Holds the constraints and scratch buffers; the
/// evolving [`ClothState`] is passed in, so one solver can step snapshots.
#[derive(Clone, Debug)]
pub struct ClothSolver {
    shape: SheetShape,
    rest: Vec<Vec3A>,
    /// Inverse masses with hard pins set to 0.
    inv_mass: Vec<f32>,
    /// Each pin slot's particle.
    pins: Vec<u32>,
    /// The hard pins as `(particle, slot)`.
    hard: Vec<(u32, u32)>,
    /// Each particle's nearest pin slot at rest ([`NO_ANCHOR`] for an unpinned sheet), for resets.
    anchor: Vec<u32>,
    constraints: Constraints,
    /// Positions at the start of the sub-step (scratch).
    previous: Vec<Vec3A>,
    /// This call's pin targets after refusing bad ones (scratch).
    end: Vec<Vec3A>,
    /// This sub-step's pin targets (scratch).
    now: Vec<Vec3A>,
    /// Sub-step length, seconds, and as `f64` for the clock.
    h: f32,
    h_clock: f64,
    iterations: u32,
    /// Most sub-steps one frame may run.
    max_substeps: u32,
    /// `h g`: one sub-step of gravity.
    gravity_dv: Vec3A,
    gravity: Vec3A,
    /// `exp(-damping h)`.
    decay: f32,
}

impl ClothSolver {
    /// Builds the solver for `sheet`. Everything a step needs is allocated here.
    ///
    /// # Errors
    /// [`ClothError::Param`] if `params` or `config` fail their `validate`.
    pub fn new(sheet: &ClothSheet, params: &ClothParams, config: &SolverConfig) -> Result<Self, ClothError> {
        params.validate()?;
        config.validate()?;
        let h_clock = 1.0 / (f64::from(PHYSICS_HZ) * f64::from(config.substeps));
        #[allow(clippy::cast_possible_truncation)] // a sub-step of a few milliseconds
        let h = h_clock as f32;
        let pins: Vec<(usize, f32)> = sheet.pins.iter().map(|&p| (p as usize, sheet.pin_weights[p as usize])).collect();
        let mut inv_mass = sheet.inv_mass.clone();
        let mut hard = Vec::new();
        for (slot, &(particle, weight)) in pins.iter().enumerate() {
            if weight >= 1.0 {
                inv_mass[particle] = 0.0;
                // Particle and slot counts fit in u32 (checked by the sheet).
                #[allow(clippy::cast_possible_truncation)]
                hard.push((particle as u32, slot as u32));
            }
        }
        let constraints = Constraints::build(&Inputs {
            topology: sheet.shape,
            rest: &sheet.rest,
            inv_mass: &inv_mass,
            pins: &pins,
            stretch: params.stretch,
            shear: params.shear,
            bend: params.bend,
            max_stretch: params.max_stretch,
            h,
        });
        let frame_substeps = (f64::from(config.max_frame_s) / h_clock + SUBSTEP_SLACK).floor();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // at most 1 s of 7680 Hz sub-steps
        let max_substeps = frame_substeps as u32;
        let count = sheet.pins.len();
        Ok(Self {
            shape: sheet.shape,
            rest: sheet.rest.iter().map(|&p| Vec3A::from(p)).collect(),
            anchor: anchors(&sheet.rest, &sheet.pins),
            inv_mass,
            pins: sheet.pins.clone(),
            hard,
            constraints,
            previous: vec![Vec3A::ZERO; sheet.rest.len()],
            end: vec![Vec3A::ZERO; count],
            now: vec![Vec3A::ZERO; count],
            h,
            h_clock,
            iterations: config.iterations,
            max_substeps,
            gravity_dv: Vec3A::from(params.gravity * h),
            gravity: Vec3A::from(params.gravity),
            decay: libm::expf(-params.damping * h),
        })
    }

    /// A state with every particle at rest and no velocity.
    pub fn rest_state(&self) -> ClothState {
        let targets: Vec<Vec3A> = self.pins.iter().map(|&p| self.rest[p as usize]).collect();
        ClothState {
            positions: self.rest.clone(),
            velocities: vec![Vec3A::ZERO; self.rest.len()],
            earlier: targets.clone(),
            targets,
            span_s: 0.0,
            owed_s: 0.0,
            substep: 0,
        }
    }

    /// A state with the particles at `positions` (the rest shape placed under the current pose, say), no velocity,
    /// and the pins' targets where their particles are.
    ///
    /// # Errors
    /// [`ClothError::Length`] or [`ClothError::Particle`] for a wrong count or a non-finite or distant position.
    pub fn state_at(&self, positions: &[Vec3]) -> Result<ClothState, ClothError> {
        let mut state = self.rest_state();
        self.reset(&mut state, positions)?;
        Ok(state)
    }

    /// Puts `state` at `positions` with no velocity, as [`ClothSolver::state_at`], without allocating.
    ///
    /// # Errors
    /// As [`ClothSolver::state_at`], or a `state` of another sheet; `state` is unchanged on error.
    pub fn reset(&self, state: &mut ClothState, positions: &[Vec3]) -> Result<(), ClothError> {
        self.check_state(state)?;
        check_len("positions", positions, self.rest.len())?;
        check_positions("position", positions)?;
        for (x, &p) in state.positions.iter_mut().zip(positions) {
            *x = Vec3A::from(p);
        }
        state.velocities.fill(Vec3A::ZERO);
        for (target, &p) in state.targets.iter_mut().zip(&self.pins) {
            *target = Vec3A::from(positions[p as usize]);
        }
        state.earlier.copy_from_slice(&state.targets);
        state.span_s = 0.0;
        state.owed_s = 0.0;
        state.substep = 0;
        Ok(())
    }

    /// Advances `state` by a frame of `dt` seconds, with the pins ending at `targets` (one per [`ClothSheet::pins`]).
    /// Real-time: no allocation, no locks.
    ///
    /// # Errors
    /// [`ClothError::Length`] if `state` is another sheet's or `targets` has the wrong length; nothing moves.
    pub fn step(&mut self, dt: f32, state: &mut ClothState, targets: &[Vec3]) -> Result<StepReport, ClothError> {
        self.step_with(dt, state, targets, &mut NoHook)
    }

    /// [`ClothSolver::step`] with a hook that adds contacts or wind to every sub-step.
    ///
    /// # Errors
    /// As [`ClothSolver::step`].
    pub fn step_with<H: SubstepHook + ?Sized>(
        &mut self,
        dt: f32,
        state: &mut ClothState,
        targets: &[Vec3],
        hook: &mut H,
    ) -> Result<StepReport, ClothError> {
        self.check_state(state)?;
        check_len("pin targets", targets, self.pins.len())?;
        let mut report = StepReport::default();
        let owed_before = state.owed_s;
        if dt.is_finite() && dt >= 0.0 {
            state.owed_s += f64::from(dt);
        } else {
            report.rejected_dt = true;
        }
        let due = (state.owed_s / self.h_clock + SUBSTEP_SLACK).floor();
        let run = due.min(f64::from(self.max_substeps));
        // A frame cut short owes nothing more: the time it dropped is gone, and a fraction of it would only make the
        // next frame run an extra sub-step.
        state.owed_s = if due > run { 0.0 } else { (state.owed_s - due * self.h_clock).clamp(0.0, self.h_clock) };
        #[allow(clippy::cast_possible_truncation)] // clamped into f32's range first
        let dropped_s = ((due - run) * self.h_clock).min(f64::from(f32::MAX)) as f32;
        report.dropped_s = dropped_s;
        // A frame cut short, or one with no length (a sub-step owed from rounding), spreads the pins' motion evenly.
        let timing = if due > run || report.rejected_dt || dt <= 0.0 {
            Timing::Even { h: self.h_clock }
        } else {
            Timing::Clock { owed_s: owed_before, h: self.h_clock, dt: f64::from(dt) }
        };
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0 <= run <= max_substeps
        let substeps = run as u32;
        Ok(self.run(substeps, timing, state, targets, hook, report))
    }

    /// Runs exactly `substeps` sub-steps as one frame of `substeps · h` seconds, the pins moving from their last
    /// targets to `targets` as in a step, leaving the owed frame time alone: the clock of an offline render, which
    /// counts sub-steps rather than frame times. The cost is linear in `substeps`.
    ///
    /// # Errors
    /// As [`ClothSolver::step`].
    pub fn advance<H: SubstepHook + ?Sized>(
        &mut self,
        substeps: u32,
        state: &mut ClothState,
        targets: &[Vec3],
        hook: &mut H,
    ) -> Result<StepReport, ClothError> {
        self.check_state(state)?;
        check_len("pin targets", targets, self.pins.len())?;
        let timing = Timing::Even { h: self.h_clock };
        Ok(self.run(substeps, timing, state, targets, hook, StepReport::default()))
    }

    /// The sheet's energy in `state` (see [`ClothEnergy`]).
    pub fn energy(&self, state: &ClothState) -> ClothEnergy {
        let mut energy = ClothEnergy {
            elastic: self.constraints.elastic_energy(&state.positions, &state.targets, self.h),
            ..ClothEnergy::default()
        };
        let moving = self.inv_mass.iter().zip(&state.positions).zip(&state.velocities).filter(|((w, _), _)| **w > 0.0);
        for ((&w, x), v) in moving {
            let mass = 1.0 / f64::from(w);
            energy.kinetic += 0.5 * mass * f64::from(v.length_squared());
            energy.gravity -= mass * f64::from(self.gravity.dot(*x));
        }
        energy
    }

    /// The largest strain of any warp or weft edge in `state` (`0.05` is 5% longer than at rest).
    pub fn max_strain(&self, state: &ClothState) -> f32 {
        self.constraints.max_strain(&state.positions)
    }

    /// The constraints projected per iteration, by family.
    pub fn counts(&self) -> ConstraintCounts {
        self.constraints.counts()
    }

    /// The sheet's grid.
    pub fn shape(&self) -> SheetShape {
        self.shape
    }

    /// The sub-step length, seconds.
    pub fn substep_s(&self) -> f32 {
        self.h
    }

    /// Inverse masses as the solver uses them: the sheet's, with hard pins at 0.
    pub fn inv_mass(&self) -> &[f32] {
        &self.inv_mass
    }

    /// Refuses a state made for another sheet.
    fn check_state(&self, state: &ClothState) -> Result<(), ClothError> {
        check_len("state positions", &state.positions, self.rest.len())?;
        check_len("state velocities", &state.velocities, self.rest.len())?;
        check_len("state targets", &state.targets, self.pins.len())?;
        check_len("state earlier targets", &state.earlier, self.pins.len())
    }

    /// Runs `substeps` sub-steps of a checked call and leaves the hard pins exactly on their targets.
    fn run<H: SubstepHook + ?Sized>(
        &mut self,
        substeps: u32,
        timing: Timing,
        state: &mut ClothState,
        targets: &[Vec3],
        hook: &mut H,
        mut report: StepReport,
    ) -> StepReport {
        report.substeps = substeps;
        report.reset_particles = self.repair(state);
        for ((end, &target), &last) in self.end.iter_mut().zip(targets).zip(&state.targets) {
            let target = Vec3A::from(target);
            *end = if usable(target) {
                target
            } else {
                report.rejected_targets += 1;
                last
            };
        }
        // A parabola through the last three frames' targets when the last two frames are of a like length; a frame
        // twice as long as the one before (or half), or one after a reset or a stall, takes the straight line.
        let duration = timing.duration(substeps);
        let ratio = Some(state.span_s / duration).filter(|r| (0.5..=2.0).contains(r));
        for k in 1..=substeps {
            let fraction = timing.fraction(k, substeps);
            report.reset_particles += self.substep(state, fraction, path_weights(fraction, ratio), hook);
        }
        for &(particle, slot) in &self.hard {
            state.positions[particle as usize] = self.end[slot as usize];
        }
        state.earlier.copy_from_slice(&state.targets);
        state.targets.copy_from_slice(&self.end);
        state.span_s = duration;
        report
    }

    /// Resets particles that arrive unusable (a NaN or a far-off position written through `positions_mut`, say): a bad
    /// position goes back to its rest place relative to its nearest pin's target (or to rest, on an unpinned sheet), a
    /// bad velocity to zero. Returns how many particles it touched.
    fn repair(&self, state: &mut ClothState) -> u32 {
        let mut count = 0;
        for (i, (x, v)) in state.positions.iter_mut().zip(state.velocities.iter_mut()).enumerate() {
            if usable(*x) && v.is_finite() {
                continue;
            }
            count += 1;
            *v = Vec3A::ZERO;
            if !usable(*x) {
                *x = match self.anchor[i] {
                    NO_ANCHOR => self.rest[i],
                    slot => {
                        let pin = self.pins[slot as usize] as usize;
                        self.rest[i] - self.rest[pin] + state.targets[slot as usize]
                    }
                };
            }
        }
        count
    }

    /// One sub-step ending at `fraction` of the frame, the pins at `path` weights of their earlier, start and end
    /// targets. Returns how many particles it had to reset.
    fn substep<H: SubstepHook + ?Sized>(
        &mut self,
        state: &mut ClothState,
        fraction: f32,
        [w0, w1, w2]: [f32; 3],
        hook: &mut H,
    ) -> u32 {
        let Self { inv_mass: w, hard, constraints, previous, end, now, h, iterations, gravity_dv, decay, .. } = self;
        let (h, x, v) = (*h, &mut state.positions[..], &mut state.velocities[..]);
        let substep = Substep { index: state.substep, h, fraction };

        // Where the pins are at the end of this sub-step. The last sub-step of a frame lands exactly on the target.
        let history = state.earlier.iter().zip(&state.targets).zip(end.iter());
        for (now, ((&earlier, &start), &end)) in now.iter_mut().zip(history) {
            *now = if fraction >= 1.0 { end } else { earlier * w0 + start * w1 + end * w2 };
        }
        previous.copy_from_slice(x);
        hook.accelerate(&substep, x, v, w);
        for ((x, v), &w) in x.iter_mut().zip(v.iter_mut()).zip(w.iter()) {
            if w > 0.0 {
                *v += *gravity_dv;
                *x += *v * h;
            }
        }
        for &(particle, slot) in hard.iter() {
            x[particle as usize] = now[slot as usize];
        }

        constraints.clear_multipliers();
        for iteration in 0..*iterations {
            constraints.sweep(x, w, now, iteration % 2 == 0);
        }
        constraints.limit_stretch(x);
        hook.project(&substep, x, previous, w);

        let mut reset = 0;
        let inv_h = 1.0 / h;
        for (((x, v), &start), &w) in x.iter_mut().zip(v.iter_mut()).zip(previous.iter()).zip(w.iter()) {
            let mut velocity = ((*x - start) * inv_h).clamp_length_max(MAX_SPEED);
            if w > 0.0 {
                velocity *= *decay;
            }
            if usable(*x) && velocity.is_finite() {
                *v = velocity;
            } else {
                // A blow-up (overflow, a hook's NaN) is contained: back to where the sub-step started, at rest.
                *x = start;
                *v = Vec3A::ZERO;
                reset += 1;
            }
        }
        state.substep += 1;
        reset
    }
}

/// Each particle's nearest pin slot at rest (the lowest slot on a tie), or [`NO_ANCHOR`] when nothing is pinned.
fn anchors(rest: &[Vec3], pins: &[u32]) -> Vec<u32> {
    rest.iter()
        .map(|&x| {
            let distance = |slot: usize| x.distance_squared(rest[pins[slot] as usize]);
            #[allow(clippy::cast_possible_truncation)] // slot counts fit in u32 (checked by the sheet)
            (0..pins.len()).min_by(|&a, &b| distance(a).total_cmp(&distance(b))).map_or(NO_ANCHOR, |slot| slot as u32)
        })
        .collect()
}
