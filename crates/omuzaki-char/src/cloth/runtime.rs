//! [`ClothRuntime`]: the [`PartRuntime`] that binds a garment's render mesh to a coarse XPBD sheet (spec RG-04).
//!
//! # The binding
//!
//! Every garment generator lofts its mesh as a parametric grid ([`ClothGrid`]: rows from the pinned edge out, columns
//! around or across, skinned to the skeleton). The sheet the solver steps is a **subsample** of that grid: each sheet
//! particle sits exactly on a grid vertex (rows and columns picked evenly by index), so its rest position and skin
//! weights are the grid's own. Each render vertex keeps the four sheet particles around it in parameter space and its
//! bilinear weights ([`ClothBinding`]).
//!
//! A frame then works in **displacements**, never in absolute positions:
//!
//! - `s_v` is the render vertex skinned as the static mesh would be (linear blend skinning through
//!   [`PoseSource::skin_matrix`]), and `s_p` each sheet particle skinned the same way;
//! - `d_p = x_p - s_p` is how far the simulation moved a particle from where the skin would put it;
//! - the vertex is drawn at `s_v + f · Σ w_k d_k`, where `f` is the row's **follow** weight: 0 keeps the row on the
//!   skin (a tight waistband never lags), 1 lets it follow the sheet, and a ramp in between is the hybrid skinning.
//!
//! So after [`reset`](PartRuntime::reset) (`x_p = s_p`) the output **is** the skinned static mesh to rounding, whatever
//! the pose, and the sheet only ever adds the motion it simulated. Normals are re-derived from the drawn grid (central
//! differences across rows and columns, oriented once at build so the rest normals face away from the grid's centre).
//!
//! **Pins** are the sheet's first `pin_rows` rows (at the finest level; a coarser level pins the rows at or above the
//! same grid row, so pins map by parameter). A pin's target is its rest position skinned by its own weights: exactly
//! where the skinned mesh puts that vertex, so no gap ever opens at the waistband.
//!
//! The simulation runs in **world** space (walking and turning swing the cloth); the output is in character space
//! (`root⁻¹`), as [`DynamicMesh`] asks.
//!
//! # State
//!
//! The runtime owns, all sized at build: the levels (each a sheet, its solver, its state and its wind), one
//! [`ClothEnvironment`] with the colliders ([`ColliderSet`], placed every frame from the pose), the joint matrices of
//! the last frame (a joint the pose cannot give, or gives as NaN, keeps its last matrix), the drawn positions and
//! normals with their revision, and a fade buffer for level switches. Its clock is the active state's sub-step count
//! plus an offset carried across level switches and settles, so wind gusts and offline renders see one continuous
//! shot clock ([`ClothRuntime::clock`]).
//!
//! # Levels of detail (#706)
//!
//! [`ClothSpec::levels`] are pre-built sheets of the same garment, finest first (for example 24 x 16, 12 x 8, 6 x 4),
//! with the same material (the solver's compliances are resolution independent) and pins mapped by parameter. The
//! level follows the garment's projected height in pixels ([`StepCtx::pixels_per_metre`] times the garment's bind
//! height) through [`LodPolicy`]'s thresholds with hysteresis, or is forced with [`ClothRuntime::force_level`]. A
//! switch **resamples** the state: every particle of the new level takes the old level's displacement and velocity at
//! its grid vertex (the bilinear embedding above), a pure function of the two states that allocates nothing. The drawn
//! mesh does not pop: the difference between the old and the new level at the switch is kept and faded out over
//! [`FADE_S`]. Below [`LodPolicy::sleep_px`] (or off screen) the sheet sleeps: it holds its last drawn pose, costs
//! nothing, and wakes with a [`reset`](PartRuntime::reset) to the current pose, faded in the same way.
//!
//! # Cost
//!
//! [`ClothRuntime::new`] allocates everything; [`PartRuntime::step`], level switches, sleeping and waking never
//! allocate (`tests/cloth_runtime.rs` counts). A frame costs the solver's sub-steps for the active level, the collider
//! sweep, and one linear-blend skin of the particles and the render vertices (`cargo bench -p omuzaki-char --bench
//! cloth_runtime`).

use omuzaki_char_api::{DynamicMesh, JointRef, PartRuntime, PoseSource, Quality, RuntimeCost, StepCtx};
use omuzaki_core::glam::{Affine3A, Mat4, Vec3, Vec3A};

use super::collide::ColliderSet;
use super::contact::{ClothEnvironment, ContactError, ContactParams, Wind, WindParams};
use super::quality;
use super::solver::{
    ClothError, ClothParams, ClothSheet, ClothSolver, ClothState, SheetShape, SolverConfig, StepReport, Substep,
    SubstepHook,
};

/// Joints one vertex may be skinned to (glTF's one set of four).
pub const MAX_INFLUENCES: usize = 4;

/// Most levels of detail a runtime keeps.
pub const MAX_LEVELS: usize = 4;

/// How long a level switch or a wake fades the drawn mesh from the old shape to the new, seconds.
pub const FADE_S: f32 = 0.25;

/// A vertex's skinning: up to [`MAX_INFLUENCES`] joints (indices into [`ClothGrid::joints`]) and their weights. Unused
/// slots have weight 0.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct SkinWeights {
    /// Joint indices into the grid's joint list.
    pub joints: [u16; MAX_INFLUENCES],
    /// Their weights, `>= 0`; normalised to sum 1 when the grid is bound.
    pub weights: [f32; MAX_INFLUENCES],
}

impl SkinWeights {
    /// A vertex skinned to one joint.
    pub const fn rigid(joint: u16) -> Self {
        Self { joints: [joint, 0, 0, 0], weights: [1.0, 0.0, 0.0, 0.0] }
    }

    /// The same weights summing to 1, or `None` if a weight is negative or not finite, the sum is 0, or a joint with
    /// weight is not below `joint_count`.
    pub fn normalised(&self, joint_count: usize) -> Option<Self> {
        let mut sum = 0.0;
        for (&joint, &weight) in self.joints.iter().zip(&self.weights) {
            if !(weight.is_finite() && weight >= 0.0) || (weight > 0.0 && usize::from(joint) >= joint_count) {
                return None;
            }
            sum += weight;
        }
        if !(sum.is_finite() && sum > 0.0) {
            return None;
        }
        let mut out = *self;
        for (w, j) in out.weights.iter_mut().zip(out.joints.iter_mut()) {
            *w /= sum;
            if *w == 0.0 {
                *j = 0;
            }
        }
        Some(out)
    }

    /// `p` skinned by these weights through `matrices` (one per joint).
    #[inline]
    pub(crate) fn skin(&self, matrices: &[Affine3A], p: Vec3A) -> Vec3A {
        let mut out = Vec3A::ZERO;
        for (&joint, &weight) in self.joints.iter().zip(&self.weights) {
            if weight > 0.0 {
                out += matrices[usize::from(joint)].transform_point3a(p) * weight;
            }
        }
        out
    }
}

/// A garment's render mesh as the generators loft it: `rows x cols` vertices stored row by row, row 0 the edge that is
/// pinned (a waistband, a collar), in bind space, each skinned to the skeleton.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothGrid {
    /// Rows of vertices, at least 2.
    pub rows: usize,
    /// Vertices per row, at least 2 (3 when wrapped).
    pub cols: usize,
    /// Whether the last column joins the first (a skirt, a sleeve).
    pub wrap: bool,
    /// Bind-space positions, row by row.
    pub positions: Vec<Vec3>,
    /// Each vertex's skinning.
    pub skin: Vec<SkinWeights>,
    /// The joints [`SkinWeights::joints`] index.
    pub joints: Vec<JointRef>,
}

impl ClothGrid {
    /// The vertex at `row`, `col`.
    pub fn index(&self, row: usize, col: usize) -> usize {
        row * self.cols + col
    }

    /// The grid's height in bind space (its extent along Y), metres: what the level of detail projects.
    pub fn height(&self) -> f32 {
        let (lo, hi) = self.positions.iter().fold((f32::MAX, f32::MIN), |(lo, hi), p| (lo.min(p.y), hi.max(p.y)));
        (hi - lo).max(0.0)
    }

    fn check(&self) -> Result<Vec<SkinWeights>, ClothRuntimeError> {
        let min_cols = if self.wrap { 3 } else { 2 };
        if self.rows < 2 || self.cols < min_cols {
            return Err(ClothRuntimeError::Grid("a cloth grid needs at least 2 rows and 2 columns (3 when wrapped)"));
        }
        let count = self.rows.checked_mul(self.cols).filter(|&n| n <= u32::MAX as usize / 2);
        if count != Some(self.positions.len()) || self.skin.len() != self.positions.len() {
            return Err(ClothRuntimeError::Grid("a cloth grid needs one position and one skin weight per vertex"));
        }
        if self.joints.is_empty() || self.joints.len() > usize::from(u16::MAX) {
            return Err(ClothRuntimeError::Grid("a cloth grid needs 1 to 65535 joints"));
        }
        if !self.positions.iter().all(|p| p.is_finite() && p.abs().max_element() <= super::solver::MAX_COORDINATE) {
            return Err(ClothRuntimeError::Grid("a cloth grid position is not finite or is too far from the origin"));
        }
        self.skin
            .iter()
            .map(|s| s.normalised(self.joints.len()))
            .collect::<Option<Vec<_>>>()
            .ok_or(ClothRuntimeError::Grid("a skin weight is negative, not finite, all zero or names no joint"))
    }
}

/// A sheet's size in particles.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct SheetSize {
    /// Particles per row (around a wrapped garment).
    pub cols: usize,
    /// Rows of particles, from the pinned edge out.
    pub rows: usize,
}

impl SheetSize {
    /// `cols x rows` particles.
    pub const fn new(cols: usize, rows: usize) -> Self {
        Self { cols, rows }
    }
}

/// When the levels of detail switch, by the garment's height on screen in pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct LodPolicy {
    /// `level_px[k]` is the height below which level `k + 1` replaces level `k` (one fewer than the levels, descending).
    pub level_px: Vec<f32>,
    /// Below this height (or off screen) the sheet sleeps.
    pub sleep_px: f32,
    /// A change needs this share more (finer) or less (coarser) than its threshold, so it never flickers.
    pub hysteresis: f32,
}

impl Default for LodPolicy {
    /// 24 x 16 above 240 px, 12 x 8 above 96 px, 6 x 4 above 24 px, asleep below, 15% hysteresis.
    fn default() -> Self {
        Self { level_px: vec![240.0, 96.0], sleep_px: 24.0, hysteresis: 0.15 }
    }
}

/// How a garment's cloth is simulated: the sheet levels, pins, fabric, contacts and quality. A garment's
/// `ClothSpec` maps onto this when cloth is turned on (C10); with cloth off the garment keeps its skinned mesh and has
/// no runtime.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothSpec {
    /// Sheet sizes, finest first: 1 to [`MAX_LEVELS`], each no finer than the grid, particle counts not increasing.
    pub levels: Vec<SheetSize>,
    /// Rows of the finest sheet pinned to the skeleton, from row 0 (at least 1, fewer than its rows).
    pub pin_rows: usize,
    /// The fabric's mass per area, kg/m².
    pub density: f32,
    /// Per grid row, how much the drawn vertex follows the sheet (0 the skin, 1 the sheet); empty for
    /// [`follow_ramp`] over the pinned rows.
    pub follow: Vec<f32>,
    /// The fabric.
    pub params: ClothParams,
    /// Contacts with the colliders.
    pub contact: ContactParams,
    /// Wind coefficients; the velocity comes from [`StepCtx::wind`] each frame.
    pub wind: WindParams,
    /// Live (the live solver preset, the sheets as given) or Offline (twice the rows and columns, 4 sub-steps of 8
    /// sweeps; see [`quality`](super::quality)).
    pub quality: Quality,
    /// Level-of-detail thresholds.
    pub lod: LodPolicy,
    /// The mesh's index in the part's mesh list ([`DynamicMesh::mesh`]).
    pub mesh: u16,
}

impl Default for ClothSpec {
    /// The skirt class: 24 x 16, 12 x 8 and 6 x 4 levels, the waist row pinned, 0.2 kg/m² cotton, live quality.
    fn default() -> Self {
        Self {
            levels: vec![SheetSize::new(24, 16), SheetSize::new(12, 8), SheetSize::new(6, 4)],
            pin_rows: 1,
            density: 0.2,
            follow: Vec::new(),
            params: ClothParams::default(),
            contact: ContactParams::default(),
            wind: WindParams::default(),
            quality: Quality::Live,
            lod: LodPolicy::default(),
            mesh: 0,
        }
    }
}

/// The default follow weights for `rows` grid rows when rows up to `pinned` (a grid row index) are pinned: 0 on the
/// pinned rows, rising smoothly to 1 over as many rows again, 1 below.
#[allow(clippy::cast_precision_loss)] // row counts
pub fn follow_ramp(rows: usize, pinned: usize) -> Vec<f32> {
    let span = (pinned + 1) as f32;
    (0..rows)
        .map(|r| {
            if r <= pinned {
                0.0
            } else {
                let t = ((r - pinned) as f32 / span).min(1.0);
                t * t * (3.0 - 2.0 * t)
            }
        })
        .collect()
}

/// Why a cloth runtime could not be built or driven.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum ClothRuntimeError {
    /// The grid is malformed.
    #[error("cloth grid: {0}")]
    Grid(&'static str),
    /// The spec is out of range.
    #[error("cloth spec: {0}")]
    Spec(&'static str),
    /// The solver refused a sheet, its parameters or a state.
    #[error(transparent)]
    Solver(#[from] ClothError),
    /// The contacts refused a parameter or a collider.
    #[error(transparent)]
    Contact(#[from] ContactError),
    /// An offline clock asked to go back in time; restore a snapshot instead.
    #[error("the cloth clock is at sub-step {now}; it cannot advance to {asked}")]
    Clock {
        /// The current sub-step.
        now: u64,
        /// The one asked for.
        asked: u64,
    },
    /// A snapshot of another runtime.
    #[error("the snapshot belongs to another cloth runtime")]
    Snapshot,
}

/// A render vertex's embedding in one level: the four particles around it and their bilinear weights.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
struct Embed {
    particles: [u32; 4],
    weights: [f32; 4],
}

/// The binding of a grid to one sheet: which grid vertex each particle sits on, and each vertex's embedding.
#[derive(Clone, Debug, PartialEq)]
pub struct ClothBinding {
    size: SheetSize,
    /// The grid row of each sheet row and the grid column of each sheet column.
    rows: Vec<usize>,
    cols: Vec<usize>,
    /// Per render vertex.
    embed: Vec<Embed>,
    /// Per particle: its grid vertex.
    vertex: Vec<u32>,
    pin_rows: usize,
}

/// The grid index of each of `n` sheet rows or columns picked evenly from `m` grid ones: both ends kept for an open
/// line, the seam's spacing kept for a wrapped one.
fn pick(n: usize, m: usize, wrap: bool) -> Vec<usize> {
    (0..n).map(|j| if wrap { j * m / n } else { (j * (m - 1) * 2 + (n - 1)) / (2 * (n - 1)) }).collect()
}

/// For grid index `g`, the interval `[picked[k], picked[k+1]]` holding it and the fraction along it. A wrapped line's
/// last interval runs from the last pick to the first, through the seam.
#[allow(clippy::cast_precision_loss)] // grid indices
fn locate(picked: &[usize], g: usize, m: usize, wrap: bool) -> (usize, usize, f32) {
    let k = picked.partition_point(|&p| p <= g).saturating_sub(1);
    let (lo, hi, hi_g) = if k + 1 < picked.len() {
        (k, k + 1, picked[k + 1])
    } else if wrap {
        (k, 0, m + picked[0])
    } else {
        (k.saturating_sub(1), k, picked[k])
    };
    let lo_g = picked[lo];
    if hi_g == lo_g {
        return (lo, hi, 0.0);
    }
    (lo, hi, (g as f32 - lo_g as f32) / (hi_g as f32 - lo_g as f32))
}

impl ClothBinding {
    /// Binds `grid` to a sheet of `size` particles whose first `pin_grid_row + 1` grid rows are pinned (pins mapped by
    /// parameter: every sheet row on a grid row at or above it, and always row 0).
    ///
    /// # Errors
    /// [`ClothRuntimeError::Spec`] for a sheet smaller than 2 x 2 (3 columns wrapped) or finer than the grid.
    pub fn from_grid(grid: &ClothGrid, size: SheetSize, pin_grid_row: usize) -> Result<Self, ClothRuntimeError> {
        let min_cols = if grid.wrap { 3 } else { 2 };
        if size.rows < 2 || size.cols < min_cols {
            return Err(ClothRuntimeError::Spec("a sheet needs at least 2 rows and 2 columns (3 when wrapped)"));
        }
        if size.rows > grid.rows || size.cols > grid.cols {
            return Err(ClothRuntimeError::Spec("a sheet may not be finer than its grid"));
        }
        let rows = pick(size.rows, grid.rows, false);
        let cols = pick(size.cols, grid.cols, grid.wrap);
        let pin_rows = rows.iter().take_while(|&&r| r <= pin_grid_row).count().max(1);
        if pin_rows >= size.rows {
            return Err(ClothRuntimeError::Spec("every row of a sheet is pinned"));
        }
        let mut embed = Vec::with_capacity(grid.rows * grid.cols);
        #[allow(clippy::cast_possible_truncation)] // the grid's vertex count fits in u32 (checked)
        let at = |r: usize, c: usize| (r * size.cols + c) as u32;
        for g_r in 0..grid.rows {
            let (r0, r1, tv) = locate(&rows, g_r, grid.rows, false);
            for g_c in 0..grid.cols {
                let (c0, c1, tu) = locate(&cols, g_c, grid.cols, grid.wrap);
                embed.push(Embed {
                    particles: [at(r0, c0), at(r0, c1), at(r1, c0), at(r1, c1)],
                    weights: [(1.0 - tv) * (1.0 - tu), (1.0 - tv) * tu, tv * (1.0 - tu), tv * tu],
                });
            }
        }
        #[allow(clippy::cast_possible_truncation)] // as above
        let vertex = rows.iter().flat_map(|&r| cols.iter().map(move |&c| (r * grid.cols + c) as u32)).collect();
        Ok(Self { size, rows, cols, embed, vertex, pin_rows })
    }

    /// The sheet's size.
    pub fn size(&self) -> SheetSize {
        self.size
    }

    /// The grid vertex each particle sits on, row by row.
    pub fn particle_vertices(&self) -> &[u32] {
        &self.vertex
    }

    /// Rows of the sheet pinned.
    pub fn pin_rows(&self) -> usize {
        self.pin_rows
    }

    /// The grid row of each sheet row.
    pub fn grid_rows(&self) -> &[usize] {
        &self.rows
    }

    /// The grid column of each sheet column.
    pub fn grid_cols(&self) -> &[usize] {
        &self.cols
    }

    /// `values` (one per particle) interpolated at grid vertex `v`.
    #[inline]
    fn sample(&self, values: &[Vec3A], v: usize) -> Vec3A {
        let e = &self.embed[v];
        let mut out = Vec3A::ZERO;
        for (&p, &w) in e.particles.iter().zip(&e.weights) {
            out += values[p as usize] * w;
        }
        out
    }
}

/// One level of detail: a binding, its sheet's solver and state, and its wind.
#[derive(Clone, Debug)]
struct Level {
    binding: ClothBinding,
    /// Rest positions and skin weights of the particles (copies of their grid vertices').
    rest: Vec<Vec3A>,
    skin: Vec<SkinWeights>,
    /// The pinned particles in the solver's order.
    pins: Vec<u32>,
    solver: ClothSolver,
    shape: SheetShape,
    /// The longest rest edge, metres: thinner colliders also get triangle contacts.
    spacing: f32,
}

/// Everything a frame changes: what a [`ClothSnapshot`] holds.
#[derive(Clone, Debug)]
struct Dynamic {
    states: Vec<ClothState>,
    env: ClothEnvironment,
    /// Each level's wind while another level is active; the active level's wind is in `env`, and its slot here holds
    /// whatever the last switch left (never read until a switch swaps it out again).
    winds: Vec<Wind>,
    matrices: Vec<Affine3A>,
    root_inv: Affine3A,
    active: usize,
    asleep: bool,
    clock_offset: i64,
    carry_s: f32,
    positions: Vec<Vec3>,
    normals: Vec<Vec3>,
    revision: u64,
    fade: Vec<Vec3>,
    fade_s: f32,
    report: StepReport,
}

/// A copy of everything a [`ClothRuntime`] evolves: restoring it and stepping with the same inputs replays bit for bit.
/// The settle cache keeps these ([`SettleCache`](super::quality::SettleCache)).
#[derive(Clone, Debug)]
pub struct ClothSnapshot {
    key: u64,
    dynamic: Dynamic,
}

impl ClothSnapshot {
    /// The key of the runtime it came from ([`ClothRuntime::key`]).
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The shot clock it was taken at, in sub-steps.
    pub fn clock(&self) -> u64 {
        clock_of(&self.dynamic)
    }
}

#[allow(clippy::cast_possible_wrap, clippy::cast_sign_loss)] // sub-step counts stay far below 2^63
fn clock_of(d: &Dynamic) -> u64 {
    (d.states[d.active].substep() as i64 + d.clock_offset).max(0) as u64
}

/// Offsets the sub-step index the hooks see by the runtime's clock offset, so wind gusts follow the shot clock across
/// level switches and settles.
struct Clocked<'a> {
    offset: i64,
    env: &'a mut ClothEnvironment,
}

impl Clocked<'_> {
    fn shift(&self, substep: &Substep) -> Substep {
        Substep { index: substep.index.wrapping_add_signed(self.offset), ..*substep }
    }
}

impl SubstepHook for Clocked<'_> {
    fn accelerate(&mut self, substep: &Substep, positions: &[Vec3A], velocities: &mut [Vec3A], inv_mass: &[f32]) {
        let s = self.shift(substep);
        self.env.accelerate(&s, positions, velocities, inv_mass);
    }

    fn project(&mut self, substep: &Substep, positions: &mut [Vec3A], previous: &[Vec3A], inv_mass: &[f32]) {
        let s = self.shift(substep);
        self.env.project(&s, positions, previous, inv_mass);
    }
}

/// A garment's live cloth: see the [module docs](self).
#[derive(Clone, Debug)]
pub struct ClothRuntime {
    grid_shape: (usize, usize, bool),
    grid_rest: Vec<Vec3A>,
    grid_skin: Vec<SkinWeights>,
    joints: Vec<JointRef>,
    follow: Vec<f32>,
    /// `+1` or `-1`: the cross product's sign that makes rest normals face out.
    orient: f32,
    height: f32,
    levels: Vec<Level>,
    colliders: ColliderSet,
    lod: LodPolicy,
    forced: Option<usize>,
    mesh: u16,
    config: SolverConfig,
    key: u64,
    /// Initial positions and normals (bind space; the generator's output), for `fork`.
    initial: Dynamic,
    d: Dynamic,
    // Scratch, sized at build.
    skinned_particles: Vec<Vec3A>,
    displacement: Vec<Vec3A>,
    targets: Vec<Vec3>,
    resampled: Vec<Vec3>,
    velocities: Vec<Vec3A>,
    previous_out: Vec<Vec3>,
}

impl ClothRuntime {
    /// Builds the runtime for `grid` with `spec`, colliding with `colliders` (body, hair proxies, the layer below:
    /// [`ColliderSet`]). Everything a frame needs is allocated here. The runtime starts at the bind pose.
    ///
    /// # Errors
    /// [`ClothRuntimeError`] for a malformed grid or spec, or parameters the solver or the contacts refuse.
    pub fn new(grid: &ClothGrid, spec: &ClothSpec, mut colliders: ColliderSet) -> Result<Self, ClothRuntimeError> {
        let skin = grid.check()?;
        let mut joints = grid.joints.clone();
        colliders.link(&mut joints)?;
        let levels = quality::levels(spec, grid)?;
        // Pins and the follow ramp come from the levels as given, so Offline's finer sheets pin the same grid rows.
        let base = spec.levels[0];
        if spec.pin_rows == 0 || spec.pin_rows >= base.rows {
            return Err(ClothRuntimeError::Spec("pin_rows must be at least 1 and fewer than the finest sheet's rows"));
        }
        let pin_grid_row = pick(base.rows, grid.rows, false)[spec.pin_rows - 1];
        if !(spec.density.is_finite() && spec.density > 0.0) {
            return Err(ClothRuntimeError::Spec("density must be finite and > 0 (kg/m²)"));
        }
        check_lod(&spec.lod, levels.len())?;
        let follow = if spec.follow.is_empty() { follow_ramp(grid.rows, pin_grid_row) } else { spec.follow.clone() };
        if follow.len() != grid.rows || !follow.iter().all(|f| (0.0..=1.0).contains(f)) {
            return Err(ClothRuntimeError::Spec("follow needs one weight in [0, 1] per grid row"));
        }
        let config = quality::solver_config(spec.quality);
        let grid_rest: Vec<Vec3A> = grid.positions.iter().map(|&p| p.into()).collect();
        let (mut built, mut states, mut winds) =
            (Vec::with_capacity(levels.len()), Vec::with_capacity(levels.len()), Vec::with_capacity(levels.len()));
        for &size in &levels {
            let (level, state, wind) = build_level(grid, &skin, size, pin_grid_row, spec, &config)?;
            built.push(level);
            states.push(state);
            winds.push(wind);
        }
        let mut env = ClothEnvironment::new(colliders.len(), spec.contact)?;
        env.set_wind(Some(winds[0].clone()));
        env.set_sheet(Some(built[0].shape), built[0].spacing);
        let max_particles = built.iter().map(|l| l.rest.len()).max().unwrap_or(0);
        let max_pins = built.iter().map(|l| l.pins.len()).max().unwrap_or(0);
        let vertices = grid.positions.len();
        let orient = orientation(grid, &grid_rest);
        let joint_count = joints.len();
        let mut runtime = Self {
            grid_shape: (grid.rows, grid.cols, grid.wrap),
            grid_rest,
            grid_skin: skin,
            joints,
            follow,
            orient,
            height: grid.height(),
            colliders,
            lod: spec.lod.clone(),
            forced: None,
            mesh: spec.mesh,
            config,
            key: 0,
            initial: Dynamic {
                states,
                env,
                winds,
                matrices: vec![Affine3A::IDENTITY; joint_count],
                root_inv: Affine3A::IDENTITY,
                active: 0,
                asleep: false,
                clock_offset: 0,
                carry_s: 0.0,
                positions: grid.positions.clone(),
                normals: vec![Vec3::Y; vertices],
                revision: 0,
                fade: vec![Vec3::ZERO; vertices],
                fade_s: 0.0,
                report: StepReport::default(),
            },
            levels: built,
            d: Dynamic {
                states: Vec::new(),
                env: ClothEnvironment::new(0, spec.contact)?,
                winds: Vec::new(),
                matrices: Vec::new(),
                root_inv: Affine3A::IDENTITY,
                active: 0,
                asleep: false,
                clock_offset: 0,
                carry_s: 0.0,
                positions: Vec::new(),
                normals: Vec::new(),
                revision: 0,
                fade: Vec::new(),
                fade_s: 0.0,
                report: StepReport::default(),
            },
            skinned_particles: vec![Vec3A::ZERO; max_particles],
            displacement: vec![Vec3A::ZERO; max_particles],
            targets: vec![Vec3::ZERO; max_pins],
            resampled: vec![Vec3::ZERO; max_particles],
            velocities: vec![Vec3A::ZERO; max_particles],
            previous_out: vec![Vec3::ZERO; vertices],
        };
        let mut normals = std::mem::take(&mut runtime.initial.normals);
        runtime.derive_normals_into(&runtime.initial.positions, &mut normals);
        runtime.initial.normals = normals;
        runtime.d = runtime.initial.clone();
        runtime.key = runtime.compute_key(spec, &levels);
        Ok(runtime)
    }

    /// The key a [`ClothSnapshot`] and the settle cache are stored under: a hash of the grid, the spec and the
    /// colliders, so a cache never serves another garment's state.
    pub fn key(&self) -> u64 {
        self.key
    }

    /// The shot clock in sub-steps of [`ClothRuntime::substep_s`]: continuous across level switches; settles do not
    /// advance it, and a reset sets it to 0.
    pub fn clock(&self) -> u64 {
        clock_of(&self.d)
    }

    /// The sub-step length, seconds (1/360 s live, 1/480 s offline).
    pub fn substep_s(&self) -> f32 {
        self.levels[0].solver.substep_s()
    }

    /// The solver configuration the spec's quality chose.
    pub fn solver_config(&self) -> SolverConfig {
        self.config
    }

    /// The active level of detail (0 the finest).
    pub fn level(&self) -> usize {
        self.d.active
    }

    /// The number of levels.
    pub fn level_count(&self) -> usize {
        self.levels.len()
    }

    /// Whether the sheet sleeps (too small on screen, or off it).
    pub fn asleep(&self) -> bool {
        self.d.asleep
    }

    /// Forces a level (`Some(k)`, clamped to the levels) instead of choosing it by screen size, or returns to choosing
    /// (`None`). Takes effect at the next step; a forced level never sleeps.
    pub fn force_level(&mut self, level: Option<usize>) {
        self.forced = level.map(|k| k.min(self.levels.len() - 1));
    }

    /// The binding of level `k`.
    pub fn binding(&self, level: usize) -> Option<&ClothBinding> {
        self.levels.get(level).map(|l| &l.binding)
    }

    /// The solver state of the active level.
    pub fn state(&self) -> &ClothState {
        &self.d.states[self.d.active]
    }

    /// The solver of the active level (for diagnostics: strain, energy).
    pub fn solver(&self) -> &ClothSolver {
        &self.levels[self.d.active].solver
    }

    /// The colliders.
    pub fn colliders(&self) -> &ColliderSet {
        &self.colliders
    }

    /// What the last step's solver did (sub-steps run, rejected inputs).
    pub fn last_report(&self) -> StepReport {
        self.d.report
    }

    /// The garment's bind height, metres.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// The sheet's particles of the active level interpolated at every grid vertex, in character space: the surface the
    /// drawn vertices are bound to (for tests and debug drawing).
    pub fn sheet_surface(&self, out: &mut Vec<Vec3>) {
        let level = &self.levels[self.d.active];
        let x = self.d.states[self.d.active].positions();
        out.clear();
        out.extend(
            (0..self.grid_rest.len())
                .map(|v| Vec3::from(self.d.root_inv.transform_point3a(level.binding.sample(x, v)))),
        );
    }

    /// A copy of everything the runtime evolves.
    pub fn snapshot(&self) -> ClothSnapshot {
        ClothSnapshot { key: self.key, dynamic: self.d.clone() }
    }

    /// Puts the runtime back where `snapshot` was taken. Reuses the buffers, so it allocates nothing once the runtime
    /// has held a snapshot of the same garment.
    ///
    /// # Errors
    /// [`ClothRuntimeError::Snapshot`] for a snapshot of another runtime; nothing changes.
    pub fn restore(&mut self, snapshot: &ClothSnapshot) -> Result<(), ClothRuntimeError> {
        if snapshot.key != self.key {
            return Err(ClothRuntimeError::Snapshot);
        }
        self.d.clone_from(&snapshot.dynamic);
        Ok(())
    }

    /// Runs `seconds` of simulation with the pose held, from wherever the sheet is (after a [`reset`](PartRuntime::reset),
    /// from rest): the sheet hangs under gravity and settles against the colliders. The shot clock does not advance.
    pub fn settle(&mut self, pose: &dyn PoseSource, seconds: f32) {
        self.read_pose(pose);
        self.place_targets();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped to an hour of physics steps
        let steps = (f64::from(seconds) * f64::from(omuzaki_anim_api::PHYSICS_HZ)).round().clamp(0.0, 432_000.0) as u32;
        let active = self.d.active;
        let n = self.levels[active].pins.len();
        let mut ran: u64 = 0;
        for _ in 0..steps {
            self.colliders.place(&self.d.matrices, &mut self.d.env);
            let mut hook = Clocked { offset: self.d.clock_offset, env: &mut self.d.env };
            let state = &mut self.d.states[active];
            if let Ok(report) =
                self.levels[active].solver.advance(self.config.substeps, state, &self.targets[..n], &mut hook)
            {
                ran += u64::from(report.substeps);
            }
        }
        #[allow(clippy::cast_possible_wrap)] // at most an hour of sub-steps
        {
            self.d.clock_offset -= ran as i64;
        }
        self.write_output();
    }

    /// Advances the sheet to sub-step `substep` of the shot clock with the pins at `pose`: the clock of an offline
    /// render, independent of frame times (frame `k` at `fps` is sub-step [`ClothRuntime::substep_at`]`(k / fps)`).
    /// The same poses at the same sub-steps from the same start give identical bytes.
    ///
    /// # Errors
    /// [`ClothRuntimeError::Clock`] for a sub-step before the clock (restore a snapshot to go back) or more than
    /// `u32::MAX` sub-steps ahead.
    pub fn advance_to(
        &mut self,
        substep: u64,
        pose: &dyn PoseSource,
        wind: Vec3,
    ) -> Result<StepReport, ClothRuntimeError> {
        let now = self.clock();
        let n = substep.checked_sub(now).and_then(|n| u32::try_from(n).ok());
        let Some(n) = n else {
            return Err(ClothRuntimeError::Clock { now, asked: substep });
        };
        self.read_pose(pose);
        if self.d.asleep {
            self.d.asleep = false;
        }
        self.place_targets();
        self.colliders.place(&self.d.matrices, &mut self.d.env);
        if let Some(w) = self.d.env.wind_mut() {
            w.params.velocity = wind;
        }
        let active = self.d.active;
        let count = self.levels[active].pins.len();
        let mut hook = Clocked { offset: self.d.clock_offset, env: &mut self.d.env };
        let report =
            self.levels[active].solver.advance(n, &mut self.d.states[active], &self.targets[..count], &mut hook)?;
        self.d.report = report;
        self.write_output();
        Ok(report)
    }

    /// The sub-step of the shot clock at `seconds` (rounded to the nearest).
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped
    pub fn substep_at(&self, seconds: f64) -> u64 {
        let per_s = f64::from(omuzaki_anim_api::PHYSICS_HZ) * f64::from(self.config.substeps);
        (seconds * per_s).round().clamp(0.0, 9.0e15) as u64
    }

    /// Reads the joints' skin matrices and the root from `pose`; a joint the pose lacks or gives non-finite keeps its
    /// last matrix, and so does a non-invertible root.
    fn read_pose(&mut self, pose: &dyn PoseSource) {
        for (m, joint) in self.d.matrices.iter_mut().zip(&self.joints) {
            if let Some(next) = pose.skin_matrix(joint).filter(Mat4::is_finite) {
                *m = Affine3A::from_mat4(next);
            }
        }
        let root = pose.root();
        let det = root.determinant();
        if root.is_finite() && det.is_finite() && det.abs() > 1e-12 {
            let inv = Affine3A::from_mat4(root).inverse();
            if inv.is_finite() {
                self.d.root_inv = inv;
            }
        }
    }

    /// Skins the active level's particles into `skinned_particles` and copies the pins' into `targets`.
    fn place_targets(&mut self) {
        let level = &self.levels[self.d.active];
        for ((s, skin), &p) in self.skinned_particles.iter_mut().zip(&level.skin).zip(&level.rest) {
            *s = skin.skin(&self.d.matrices, p);
        }
        for (t, &p) in self.targets.iter_mut().zip(&level.pins) {
            *t = self.skinned_particles[p as usize].into();
        }
    }

    /// Snaps the active level to the skinned rest under the current matrices.
    fn snap_to_rest(&mut self) {
        self.place_targets();
        let active = self.d.active;
        let n = self.levels[active].rest.len();
        for (r, s) in self.resampled.iter_mut().zip(&self.skinned_particles[..n]) {
            *r = (*s).into();
        }
        // Skinned rest positions are finite (finite matrices of finite rest), so the solver's checks pass.
        let _ = self.levels[active].solver.reset(&mut self.d.states[active], &self.resampled[..n]);
        self.d.clear_transients();
    }

    /// Switches the active level to `to`, resampling the state (see the module docs). The drawn mesh's old shape is
    /// kept in the fade.
    fn switch(&mut self, to: usize) {
        let from = self.d.active;
        if to == from {
            return;
        }
        self.place_targets(); // skinned particles of `from`
        let n_from = self.levels[from].rest.len();
        for ((d, s), x) in
            self.displacement.iter_mut().zip(&self.skinned_particles[..n_from]).zip(self.d.states[from].positions())
        {
            *d = *x - *s;
        }
        let substep = self.d.states[from].substep();
        self.d.active = to;
        self.place_targets(); // skinned particles of `to`
        let (from_level, to_level) = (&self.levels[from], &self.levels[to]);
        let n_to = to_level.rest.len();
        let from_v = self.d.states[from].velocities();
        for (k, &vertex) in to_level.binding.vertex.iter().enumerate() {
            let v = vertex as usize;
            let d = from_level.binding.sample(&self.displacement, v);
            self.resampled[k] = (self.skinned_particles[k] + d).into();
            self.velocities[k] = from_level.binding.sample(from_v, v);
        }
        for &p in &to_level.pins {
            self.resampled[p as usize] = self.skinned_particles[p as usize].into();
        }
        let _ = to_level.solver.reset(&mut self.d.states[to], &self.resampled[..n_to]);
        self.d.states[to].velocities_mut().copy_from_slice(&self.velocities[..n_to]);
        #[allow(clippy::cast_possible_wrap)] // sub-step counts stay far below 2^63
        {
            self.d.clock_offset += substep as i64;
        }
        // The old level's wind goes back to its slot; the new one's comes out.
        if let Some(w) = self.d.env.wind_mut() {
            std::mem::swap(w, &mut self.d.winds[from]);
            std::mem::swap(w, &mut self.d.winds[to]);
        }
        self.d.env.set_sheet(Some(self.levels[to].shape), self.levels[to].spacing);
    }

    /// Starts a fade from what is drawn now to the active level's current output.
    fn start_fade(&mut self) {
        self.previous_out.copy_from_slice(&self.d.positions);
        self.d.fade_s = 0.0;
        self.write_output();
        for ((f, &old), &new) in self.d.fade.iter_mut().zip(&self.previous_out).zip(&self.d.positions) {
            *f = old - new;
        }
        self.d.fade_s = FADE_S;
        self.d.positions.copy_from_slice(&self.previous_out);
    }

    /// The level the policy picks at `px` pixels from `current`, with hysteresis; `None` to sleep.
    fn choose(&self, px: f32, current: usize, asleep: bool) -> Option<usize> {
        let h = self.lod.hysteresis;
        let sleep_at = if asleep { self.lod.sleep_px * (1.0 + h) } else { self.lod.sleep_px * (1.0 - h) };
        if px.is_nan() || px < sleep_at {
            return None;
        }
        let mut level = current.min(self.levels.len() - 1);
        while level + 1 < self.levels.len() && px < self.lod.level_px[level] * (1.0 - h) {
            level += 1;
        }
        while level > 0 && px > self.lod.level_px[level - 1] * (1.0 + h) {
            level -= 1;
        }
        Some(level)
    }

    /// Writes the drawn positions and normals from the active level; bumps the revision if a position changed.
    fn write_output(&mut self) {
        let active = self.d.active;
        let level = &self.levels[active];
        let n = level.rest.len();
        for ((d, s), x) in
            self.displacement[..n].iter_mut().zip(&self.skinned_particles[..n]).zip(self.d.states[active].positions())
        {
            *d = *x - *s;
        }
        let cols = self.grid_shape.1;
        let fade = if self.d.fade_s > 0.0 { self.d.fade_s / FADE_S } else { 0.0 };
        let mut changed = false;
        for (v, ((out, &rest), skin)) in
            self.d.positions.iter_mut().zip(&self.grid_rest).zip(&self.grid_skin).enumerate()
        {
            let follow = self.follow[v / cols];
            let mut p = skin.skin(&self.d.matrices, rest);
            if follow > 0.0 {
                p += level.binding.sample(&self.displacement, v) * follow;
            }
            let mut p: Vec3 = self.d.root_inv.transform_point3a(p).into();
            if fade > 0.0 {
                p += self.d.fade[v] * fade;
            }
            if p.to_array().map(f32::to_bits) != out.to_array().map(f32::to_bits) {
                changed = true;
                *out = p;
            }
        }
        if changed {
            self.d.revision = self.d.revision.wrapping_add(1);
            let mut normals = std::mem::take(&mut self.d.normals);
            self.derive_normals_into(&self.d.positions, &mut normals);
            self.d.normals = normals;
        }
    }

    /// Normals of the grid `positions`, by central differences across rows and columns.
    fn derive_normals_into(&self, positions: &[Vec3], normals: &mut [Vec3]) {
        let (rows, cols, wrap) = self.grid_shape;
        for r in 0..rows {
            let (r0, r1) = (r.saturating_sub(1), (r + 1).min(rows - 1));
            for c in 0..cols {
                let (c0, c1) = if wrap {
                    ((c + cols - 1) % cols, (c + 1) % cols)
                } else {
                    (c.saturating_sub(1), (c + 1).min(cols - 1))
                };
                let du = positions[r * cols + c1] - positions[r * cols + c0];
                let dv = positions[r1 * cols + c] - positions[r0 * cols + c];
                let n = du.cross(dv) * self.orient;
                let i = r * cols + c;
                normals[i] = n.try_normalize().unwrap_or(normals[i]);
            }
        }
    }
}

impl Dynamic {
    /// Clears the fade and the time carried, after a reset.
    fn clear_transients(&mut self) {
        self.fade_s = 0.0;
        self.carry_s = 0.0;
    }
}

/// The grid's skin weights, normalised, after checking the grid.
pub(crate) fn checked_skin(grid: &ClothGrid) -> Result<Vec<SkinWeights>, ClothRuntimeError> {
    grid.check()
}

/// The grid's rest normals, oriented as the runtime orients its output.
pub(crate) fn rest_normals(grid: &ClothGrid) -> Vec<Vec3> {
    let rest: Vec<Vec3A> = grid.positions.iter().map(|&p| p.into()).collect();
    let orient = orientation(grid, &rest);
    let (rows, cols) = (grid.rows, grid.cols);
    let mut normals = vec![Vec3::Y; rest.len()];
    for r in 0..rows {
        let (r0, r1) = (r.saturating_sub(1), (r + 1).min(rows - 1));
        for c in 0..cols {
            let (c0, c1) = if grid.wrap {
                ((c + cols - 1) % cols, (c + 1) % cols)
            } else {
                (c.saturating_sub(1), (c + 1).min(cols - 1))
            };
            let du = rest[r * cols + c1] - rest[r * cols + c0];
            let dv = rest[r1 * cols + c] - rest[r0 * cols + c];
            normals[r * cols + c] = Vec3::from(du.cross(dv) * orient).normalize_or(Vec3::Y);
        }
    }
    normals
}

/// The sign that makes the grid's rest normals (`du x dv`) face away from its centre, on balance.
fn orientation(grid: &ClothGrid, rest: &[Vec3A]) -> f32 {
    #[allow(clippy::cast_precision_loss)] // vertex counts
    let centre = rest.iter().copied().sum::<Vec3A>() / rest.len().max(1) as f32;
    let (rows, cols) = (grid.rows, grid.cols);
    let mut balance = 0.0_f64;
    for r in 0..rows {
        let (r0, r1) = (r.saturating_sub(1), (r + 1).min(rows - 1));
        for c in 0..cols {
            let (c0, c1) = if grid.wrap {
                ((c + cols - 1) % cols, (c + 1) % cols)
            } else {
                (c.saturating_sub(1), (c + 1).min(cols - 1))
            };
            let du = rest[r * cols + c1] - rest[r * cols + c0];
            let dv = rest[r1 * cols + c] - rest[r0 * cols + c];
            balance += f64::from(du.cross(dv).dot(rest[r * cols + c] - centre));
        }
    }
    if balance < 0.0 { -1.0 } else { 1.0 }
}

/// Checks the level-of-detail thresholds.
fn check_lod(lod: &LodPolicy, levels: usize) -> Result<(), ClothRuntimeError> {
    if lod.level_px.len() + 1 != levels {
        return Err(ClothRuntimeError::Spec("lod.level_px needs one threshold fewer than the levels"));
    }
    let ok = |v: f32| v.is_finite() && v >= 0.0;
    if !lod.level_px.iter().all(|&v| ok(v)) || !lod.level_px.windows(2).all(|w| w[0] > w[1]) {
        return Err(ClothRuntimeError::Spec("lod.level_px must be finite, >= 0 and descending"));
    }
    if !ok(lod.sleep_px) || lod.level_px.last().is_some_and(|&last| lod.sleep_px >= last) {
        return Err(ClothRuntimeError::Spec("lod.sleep_px must be finite, >= 0 and below every level threshold"));
    }
    if !(lod.hysteresis.is_finite() && (0.0..0.5).contains(&lod.hysteresis)) {
        return Err(ClothRuntimeError::Spec("lod.hysteresis must be in [0, 0.5)"));
    }
    Ok(())
}

/// Builds one level: its binding, sheet, solver, rest state and wind.
fn build_level(
    grid: &ClothGrid,
    skin: &[SkinWeights],
    size: SheetSize,
    pin_grid_row: usize,
    spec: &ClothSpec,
    config: &SolverConfig,
) -> Result<(Level, ClothState, Wind), ClothRuntimeError> {
    let binding = ClothBinding::from_grid(grid, size, pin_grid_row)?;
    let rest: Vec<Vec3> = binding.vertex.iter().map(|&v| grid.positions[v as usize]).collect();
    let particle_skin: Vec<SkinWeights> = binding.vertex.iter().map(|&v| skin[v as usize]).collect();
    let pin_weights = (0..rest.len()).map(|i| if i / size.cols < binding.pin_rows { 1.0 } else { 0.0 }).collect();
    let shape = SheetShape { rows: size.rows, cols: size.cols, wrap: grid.wrap };
    let sheet = ClothSheet::with_density(shape, rest, spec.density, pin_weights)?;
    let solver = ClothSolver::new(&sheet, &spec.params, config)?;
    let state = solver.rest_state();
    let wind = Wind::new(shape, spec.wind)?;
    let spacing = longest_edge(shape, sheet.rest());
    let level = Level {
        rest: sheet.rest().iter().map(|&p| p.into()).collect(),
        skin: particle_skin,
        pins: sheet.pins().to_vec(),
        binding,
        solver,
        shape,
        spacing,
    };
    Ok((level, state, wind))
}

/// The longest warp or weft rest edge of a sheet.
fn longest_edge(shape: SheetShape, rest: &[Vec3]) -> f32 {
    let mut longest = 0.0_f32;
    for r in 0..shape.rows {
        for c in 0..shape.cols {
            let i = r * shape.cols + c;
            if c + 1 < shape.cols || shape.wrap {
                longest = longest.max(rest[i].distance(rest[r * shape.cols + (c + 1) % shape.cols]));
            }
            if r + 1 < shape.rows {
                longest = longest.max(rest[i].distance(rest[i + shape.cols]));
            }
        }
    }
    longest
}

impl ClothRuntime {
    /// The grid's bind positions (for the bake and tests).
    pub fn grid_rest(&self) -> impl ExactSizeIterator<Item = Vec3> + '_ {
        self.grid_rest.iter().map(|&p| p.into())
    }

    /// The grid's skin weights, normalised.
    pub fn grid_skin(&self) -> &[SkinWeights] {
        &self.grid_skin
    }

    /// The joints the grid is skinned to.
    pub fn joints(&self) -> &[JointRef] {
        &self.joints
    }

    /// The follow weight of each grid row.
    pub fn follow(&self) -> &[f32] {
        &self.follow
    }

    /// The grid's rows, columns and seam.
    pub fn grid_shape(&self) -> (usize, usize, bool) {
        self.grid_shape
    }

    /// The fabric of level `k`'s sheet as the solver holds it (rest positions, particle by particle).
    pub fn level_rest(&self, level: usize) -> Option<&[Vec3A]> {
        self.levels.get(level).map(|l| l.rest.as_slice())
    }

    /// Hashes everything that decides how the runtime evolves (see [`ClothRuntime::key`]).
    fn compute_key(&self, spec: &ClothSpec, levels: &[SheetSize]) -> u64 {
        let mut h = quality::KeyHasher::new();
        h.usize(self.grid_shape.0).usize(self.grid_shape.1).bool(self.grid_shape.2);
        for p in &self.grid_rest {
            h.vec3a(*p);
        }
        for s in &self.grid_skin {
            for (&j, &w) in s.joints.iter().zip(&s.weights) {
                h.usize(usize::from(j)).f32(w);
            }
        }
        for joint in &self.joints {
            h.str(&format!("{joint:?}"));
        }
        for &f in &self.follow {
            h.f32(f);
        }
        for size in levels {
            h.usize(size.cols).usize(size.rows);
        }
        h.usize(spec.pin_rows).f32(spec.density).str(&format!("{:?}", spec.params)).str(&format!("{:?}", spec.contact));
        h.str(&format!("{:?}", spec.wind)).str(&format!("{:?}", spec.quality)).str(&format!("{:?}", spec.lod));
        self.colliders.hash_into(&mut h);
        h.finish()
    }

    /// The bytes the runtime holds.
    fn bytes(&self) -> usize {
        use std::mem::size_of;
        let per_level: usize = self
            .levels
            .iter()
            .map(|l| {
                l.binding.embed.len() * size_of::<Embed>()
                    + l.rest.len() * (size_of::<Vec3A>() * 6 + size_of::<SkinWeights>() + size_of::<u32>() + 64)
            })
            .sum();
        let vertices = self.grid_rest.len();
        per_level
            + vertices * (size_of::<Vec3A>() + size_of::<SkinWeights>() + 2 * (4 * size_of::<Vec3>()))
            + self.colliders.len() * 128
    }
}

impl PartRuntime for ClothRuntime {
    fn step(&mut self, ctx: &StepCtx<'_>) {
        self.read_pose(ctx.pose);
        // The level: forced, or by the garment's height on screen.
        let wanted = match (self.forced, ctx.pixels_per_metre) {
            (Some(level), _) => Some(level),
            (None, Some(ppm)) => self.choose(ppm * self.height, self.d.active, self.d.asleep),
            (None, None) => Some(self.d.active),
        };
        let Some(level) = wanted else {
            self.d.asleep = true;
            self.d.report = StepReport::default();
            return;
        };
        if self.d.asleep {
            self.d.asleep = false;
            if level != self.d.active {
                self.switch(level);
            }
            self.snap_to_rest();
            self.start_fade();
        } else if level != self.d.active {
            self.previous_out.copy_from_slice(&self.d.positions);
            self.switch(level);
            self.start_fade();
        }
        self.place_targets();
        self.colliders.place(&self.d.matrices, &mut self.d.env);
        if let Some(w) = self.d.env.wind_mut() {
            w.params.velocity = ctx.wind;
        }
        let active = self.d.active;
        let count = self.levels[active].pins.len();
        let dt = if ctx.dt.is_finite() && ctx.dt >= 0.0 { ctx.dt + self.d.carry_s } else { ctx.dt };
        self.d.carry_s = 0.0;
        let mut hook = Clocked { offset: self.d.clock_offset, env: &mut self.d.env };
        let state = &mut self.d.states[active];
        self.d.report =
            self.levels[active].solver.step_with(dt, state, &self.targets[..count], &mut hook).unwrap_or_default();
        self.write_output();
        if self.d.fade_s > 0.0 {
            self.d.fade_s = if ctx.dt.is_finite() { (self.d.fade_s - ctx.dt.max(0.0)).max(0.0) } else { self.d.fade_s };
        }
    }

    fn reset(&mut self, pose: &dyn PoseSource) {
        self.read_pose(pose);
        self.d.asleep = false;
        self.d.clock_offset = 0;
        self.snap_to_rest();
        self.write_output();
    }

    fn dynamic_count(&self) -> usize {
        1
    }

    fn dynamic_mesh(&self, index: usize) -> Option<DynamicMesh<'_>> {
        (index == 0).then(|| DynamicMesh {
            mesh: self.mesh,
            positions: &self.d.positions,
            normals: &self.d.normals,
            revision: self.d.revision,
        })
    }

    #[allow(clippy::cast_possible_truncation)] // particle, collider and vertex counts fit in u32 (checked at build)
    fn cost(&self) -> RuntimeCost {
        if self.d.asleep {
            return RuntimeCost::default();
        }
        RuntimeCost {
            particles: self.levels[self.d.active].rest.len() as u32,
            colliders: self.colliders.len() as u32,
            vertices: self.grid_rest.len() as u32,
        }
    }

    fn fork(&self) -> Option<Box<dyn PartRuntime>> {
        let mut fork = self.clone();
        fork.d = self.initial.clone();
        fork.forced = None;
        Some(Box::new(fork))
    }

    fn heap_bytes(&self) -> usize {
        self.bytes()
    }
}

/// The skin matrices of `joints` from `pose`, for callers outside the runtime (the bake's comparison, tests).
pub fn skin_matrices(pose: &dyn PoseSource, joints: &[JointRef], out: &mut [Affine3A]) {
    for (m, joint) in out.iter_mut().zip(joints) {
        if let Some(next) = pose.skin_matrix(joint).filter(Mat4::is_finite) {
            *m = Affine3A::from_mat4(next);
        }
    }
}
