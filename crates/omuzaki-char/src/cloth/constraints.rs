//! Stretch, shear, bend and pin constraints in XPBD form, the stretch limiter, and the sheet's grid topology.
//!
//! Every constraint is a scalar `C(x)` projected with the XPBD update of Macklin, Müller and Chentanez 2016 (*XPBD:
//! Position-Based Simulation of Compliant Constrained Dynamics*, MIG '16), eqs. 17 and 18:
//!
//! `Δλ = (-C(x) - α̃ λ) / (Σ w_i |∇_i C|² + α̃)`, `Δx_i = w_i ∇_i C Δλ`, with `α̃ = α / h²`
//!
//! where `w_i` is particle `i`'s inverse mass, `h` the sub-step and `α` the constraint's compliance (the inverse of its
//! stiffness). `λ` accumulates over the iterations of one sub-step and restarts at zero on the next.
//!
//! The warp and weft edges are not projected one by one: each column and each row is a chain whose linearised system
//! is solved exactly per iteration ([`Chains`], line relaxation), so a long hanging column or a sagging row converges
//! as fast as a short one and a finer sheet hangs like a coarser one. Shear, bend and soft pins are projected one by
//! one; a sweep is block Gauss-Seidel.
//!
//! | Constraint | `C(x)` | Compliance `α` per constraint |
//! |---|---|---|
//! | stretch (warp and weft edges, in chains) | `\|x_a - x_b\| - L` | `stretch · L² / A_edge` (m/N) |
//! | shear (both diagonals of a cell) | `\|x_a - x_b\| - L` | `shear · 2 A_cell / L²` (m/N) |
//! | bend (three particles along a row or column, Kelager et al. 2010) | `\|x_v - (x_0 + x_v + x_1)/3\| - h_0` | `bend · (L_0 L_1)² / (9 A_v)` (m/N) |
//! | soft pin (`0 < weight < 1`) | `\|x - target\|` | `stretch · (1 - weight) / weight` (m/N) |
//!
//! `L` is a rest length, `A_edge` the rest area an edge stands for (half of each cell beside it), `A_cell` a cell's rest
//! area, `A_v` a particle's rest area (a quarter of each cell around it), `L_0` and `L_1` the rest lengths either side of
//! a bend's middle particle and `h_0` its rest distance from the centroid. The scaling turns the sheet-wide parameters of
//! [`ClothParams`](super::solver::ClothParams) into the compliances of a regular grid that discretise the same material
//! at any resolution:
//!
//! - **stretch** is `1 / (E·t)`, the inverse of the membrane's tensile stiffness (N/m): a square cell's edge then has
//!   exactly `α = stretch`, whatever its size.
//! - **shear** is `1 / (G·t)`, the inverse of its in-plane shear stiffness (N/m): a square cell's two diagonals together
//!   resist a shear strain `γ` with energy `½ G·t γ² A_cell`.
//! - **bend** is `1 / D`, the inverse of the bending stiffness per unit width (N·m), in 1/(N·m): for a flat strip
//!   `2 x_v - x_0 - x_1 ≈ -L² w''`, so the constraint's energy `C² / 2α` is the continuum `½ D w''²` over `A_v`.
//!   Kelager, Niebe and Erleben 2010 (*A Triangle Bending Constraint Model for Position-Based Dynamics*, VRIPHYS)
//!   chose this centroid form because it is cheap and, unlike a spring across two edges, resists small bends linearly.
//!
//! The stretch limiter (Provot 1995, *Deformation Constraints in a Mass-Spring Model to Describe Rigid Cloth
//! Behavior*, Graphics Interface) runs once after the XPBD iterations: an edge longer than `(1 + max_stretch) L` is
//! pulled back to that length. It is a safety net for violent motion (the chain solves already hold a hanging sheet
//! to its compliance), and it is hierarchical in the spirit of long-range attachments (Kim, Chentanez and Müller
//! 2012, *Long Range Attachments*, SCA): edges are visited in order of their distance (in edges) from the nearest pinned or
//! immovable particle, and an edge between two depths moves only its deeper end, as if the end nearer the pin were
//! held by it. One pass then leaves every edge that leads toward a pin within the limit, whatever the sheet's length;
//! edges between particles of equal depth (a skirt's hoops) share the correction by inverse mass. On a sheet with no
//! pins every edge shares.

use omuzaki_core::glam::{Vec3, Vec3A};
use serde::{Deserialize, Serialize};

/// Below this length a constraint has no direction to act along, so the iteration skips it. Particles that close
/// together only happen in a crumpled sheet, where the next iteration separates them through their other constraints.
const DEGENERATE: f32 = 1e-9;

/// The rows, columns and seam of a sheet's particle grid: particle `(r, c)` is `r * cols + c`, rows run "down" the
/// sheet (from the waist to the hem of a skirt) and columns "across". A wrapped sheet's last column joins its first, so
/// a skirt or sleeve is a closed tube with no duplicated seam column (the kit's wrapped grids repeat their first
/// column at the end; a binding maps that copy to column 0).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetShape {
    /// Rows of particles, at least 2.
    pub rows: usize,
    /// Particles per row, at least 2 (3 when wrapped).
    pub cols: usize,
    /// Whether the last column joins the first (a tube: skirt, sleeve).
    pub wrap: bool,
}

impl SheetShape {
    /// The particle at row `r`, column `c`, with the column taken around the seam of a wrapped sheet.
    fn at(self, r: usize, c: usize) -> usize {
        r * self.cols + c % self.cols
    }

    /// Columns of cells (and of edges along a row): one fewer than the columns, unless the seam closes the ring.
    fn cell_cols(self) -> usize {
        if self.wrap { self.cols } else { self.cols - 1 }
    }

    /// Each cell's corners `[top-left, top-right, bottom-left, bottom-right]`, row by row.
    fn cells(self) -> impl Iterator<Item = [usize; 4]> {
        (0..self.rows - 1).flat_map(move |r| {
            (0..self.cell_cols())
                .map(move |c| [self.at(r, c), self.at(r, c + 1), self.at(r + 1, c), self.at(r + 1, c + 1)])
        })
    }

    /// The cell at row `r`, column `c` of the cell grid, if there is one there.
    fn cell(self, r: Option<usize>, c: Option<usize>) -> Option<usize> {
        let (r, c) = (r?, c?);
        (r < self.rows - 1 && c < self.cell_cols()).then_some(r * self.cell_cols() + c)
    }

    /// The column left of `c` (across the seam of a wrapped sheet).
    fn left(self, c: usize) -> Option<usize> {
        match c {
            0 if self.wrap => Some(self.cols - 1),
            0 => None,
            _ => Some(c - 1),
        }
    }
}

/// A cell's rest area: half the length of the cross product of its diagonals, exact for a flat quad and the area of
/// its best-fit plane for a slightly warped one.
fn cell_area(rest: &[Vec3], [a, b, c, d]: [usize; 4]) -> f32 {
    0.5 * (rest[d] - rest[a]).cross(rest[c] - rest[b]).length()
}

/// Every cell's rest area, in the order of [`SheetShape::cells`].
pub(crate) fn cell_areas(topology: SheetShape, rest: &[Vec3]) -> Vec<f32> {
    topology.cells().map(|cell| cell_area(rest, cell)).collect()
}

/// Every particle's rest area: a quarter of each cell it is a corner of.
pub(crate) fn particle_areas(topology: SheetShape, rest: &[Vec3]) -> Vec<f32> {
    let mut areas = vec![0.0; rest.len()];
    for cell in topology.cells() {
        let quarter = 0.25 * cell_area(rest, cell);
        for corner in cell {
            areas[corner] += quarter;
        }
    }
    areas
}

/// The index and size of a sheet's smallest feature of one kind, if it has any.
pub(crate) type Smallest = Option<(usize, f32)>;

/// The shortest rest edge and the smallest rest cell area, with their indices (edges in [`SheetShape`] row order),
/// for validating a sheet before anything divides by them.
pub(crate) fn smallest_features(topology: SheetShape, rest: &[Vec3]) -> (Smallest, Smallest) {
    let shortest = structural_edges(topology)
        .map(|(a, b)| rest[a].distance(rest[b]))
        .enumerate()
        .min_by(|x, y| x.1.total_cmp(&y.1));
    let smallest = cell_areas(topology, rest).into_iter().enumerate().min_by(|x, y| x.1.total_cmp(&y.1));
    (shortest, smallest)
}

/// The warp and weft edges, row by row from the first: each row's edges along it (even columns, then odd), then the
/// edges down to the next row. A forward Gauss-Seidel sweep in this order carries a pinned top row's pull down the
/// sheet in one pass, and consecutive edges share no particle, so the processor overlaps their projections.
fn structural_edges(t: SheetShape) -> impl Iterator<Item = (usize, usize)> {
    (0..t.rows).flat_map(move |r| {
        let columns = (0..t.cell_cols()).step_by(2).chain((1..t.cell_cols()).step_by(2));
        let along = columns.map(move |c| (t.at(r, c), t.at(r, c + 1)));
        let down = (0..t.cols).filter(move |_| r + 1 < t.rows).map(move |c| (t.at(r, c), t.at(r + 1, c)));
        along.chain(down)
    })
}

/// Both diagonals of every cell, row by row: a row's falling diagonals, then its rising ones (so consecutive
/// diagonals share no particle). Each comes with its cell's index.
fn diagonals(t: SheetShape) -> impl Iterator<Item = (usize, (usize, usize))> {
    (0..t.rows - 1).flat_map(move |r| {
        let cell = move |c: usize| r * t.cell_cols() + c;
        let falling = (0..t.cell_cols()).map(move |c| (cell(c), (t.at(r, c), t.at(r + 1, c + 1))));
        let rising = (0..t.cell_cols()).map(move |c| (cell(c), (t.at(r, c + 1), t.at(r + 1, c))));
        falling.chain(rising)
    })
}

/// The rest area a structural edge stands for: half of each cell on either side of it.
fn edge_area(t: SheetShape, areas: &[f32], (a, b): (usize, usize)) -> f32 {
    let (ra, ca, rb) = (a / t.cols, a % t.cols, b / t.cols);
    let half = |cell: Option<usize>| cell.map_or(0.0, |i| 0.5 * areas[i]);
    if ra == rb {
        // Along a row: the cells above and below; `a` is the edge's left end.
        half(t.cell(ra.checked_sub(1), Some(ca))) + half(t.cell(Some(ra), Some(ca)))
    } else {
        // Down a column: the cells left and right.
        half(t.cell(Some(ra), t.left(ca))) + half(t.cell(Some(ra), Some(ca)))
    }
}

/// A distance constraint `|x_a - x_b| - rest` (stretch, shear) with its multiplier for the current sub-step.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Distance {
    a: u32,
    b: u32,
    rest: f32,
    /// `α̃ = α / h²`.
    alpha: f32,
    /// `1 / (w_a + w_b + α̃)`: the update's denominator, fixed because pins and masses are.
    inv_den: f32,
    lambda: f32,
}

/// A bend constraint `|x_v - centroid(x_0, x_v, x_1)| - rest` (Kelager et al. 2010) with its multiplier.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Bend {
    ends: [u32; 2],
    middle: u32,
    rest: f32,
    /// `α̃ = α / h²`.
    alpha: f32,
    /// `1 / ((4 w_middle + w_0 + w_1) / 9 + α̃)`.
    inv_den: f32,
    lambda: f32,
}

/// A soft pin: a zero-length spring from a particle to its pin target, with its multiplier.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Attach {
    particle: u32,
    /// The pin's slot in the target list.
    pub(crate) slot: u32,
    /// `α̃ = α / h²`.
    alpha: f32,
    lambda: f32,
}

/// A warp or weft edge as the stretch limiter sees it: its ends, rest length, and the share of a correction each end
/// takes (`0` for an end nearer the pins than the other, inverse-mass shares between equals).
#[derive(Copy, Clone, Debug)]
pub(crate) struct Limit {
    a: u32,
    b: u32,
    rest: f32,
    share: [f32; 2],
}

/// A third, multiplied rather than divided by in the bend kernel (a float division is not a multiplication, so the
/// compiler keeps a division by 3.0, and the kernel runs thousands of times a frame).
const THIRD: f32 = 1.0 / 3.0;

/// The `α̃` of an edge a chain carries but does not simulate (stretch is off, or both ends are immovable).
const OFF: f32 = -1.0;

/// How much a chain solve weakens its diagonal, relative. A rigid, taut chain between two pins is a singular system
/// (more constraints along the line than free particles can satisfy); this keeps the elimination finite, and the part
/// of `Δλ` it lets grow moves no particle. At the solution the step is zero, so the fixed point does not move.
const REGULARISE: f32 = 1e-4;

/// Equal-length chains of warp or weft edges, solved directly: line relaxation.
///
/// Gauss-Seidel moves a correction one edge per projection, so a few sweeps leave a long hanging chain stretched by
/// gravity's unresolved share, and the shortfall grows with the number of edges (a finer sheet sagged further). One
/// iteration here instead solves each chain's linearised XPBD system `(J W Jᵀ + α̃) Δλ = -C - α̃ λ` exactly. It is
/// tridiagonal, because consecutive edges share one particle, and the Thomas algorithm solves it in linear time (the
/// direct solver for chains of Deul, Kugelstadt, Weiler and Bender 2018, *Direct Position-Based Solver for Stiff
/// Rods*, CGF). The other families stay one-by-one, so a sweep is block Gauss-Seidel.
///
/// Chain `lane` runs through the particles `nodes[i * lanes + lane]`, `i` in `0..=len`; its edge `k` joins nodes `k`
/// and `k + 1`, and the edge's data sits at `k * lanes + lane`, the index of its first node. Every pass walks the
/// chains in step, so their independent recurrences overlap in the processor.
#[derive(Clone, Debug, Default)]
pub(crate) struct Chains {
    lanes: usize,
    len: usize,
    nodes: Vec<u32>,
    rest: Vec<f32>,
    /// `α̃ = α / h²`, or [`OFF`].
    alpha: Vec<f32>,
    lambda: Vec<f32>,
    // Scratch for one solve, per edge.
    /// The edge's unit direction, first node minus second.
    normal: Vec<Vec3A>,
    diag: Vec<f32>,
    rhs: Vec<f32>,
    /// The coupling `A[k][k + 1]` to the next edge.
    upper: Vec<f32>,
    /// The Thomas algorithm's modified upper coefficients.
    factor: Vec<f32>,
    /// Its modified right-hand side, then each edge's `Δλ`.
    step: Vec<f32>,
    /// Edges simulated.
    active: usize,
}

impl Chains {
    /// `lanes` chains of `len` edges whose node `i` of chain `lane` is particle `node(i, lane)`, each edge's `α̃` from
    /// `alpha` (`None`: not simulated). No chains at all when no edge is simulated.
    fn new(
        lanes: usize,
        len: usize,
        node: impl Fn(usize, usize) -> usize,
        rest: &[Vec3],
        alpha: impl Fn((usize, usize)) -> Option<f32>,
    ) -> Self {
        let edges: Vec<(usize, usize)> = (0..len)
            .flat_map(|k| (0..lanes).map(move |lane| (k, lane)))
            .map(|(k, lane)| (node(k, lane), node(k + 1, lane)))
            .collect();
        let alphas: Vec<f32> = edges.iter().map(|&e| alpha(e).unwrap_or(OFF)).collect();
        let active = alphas.iter().filter(|&&a| a >= 0.0).count();
        if active == 0 {
            return Self::default();
        }
        let count = edges.len();
        Self {
            lanes,
            len,
            nodes: (0..=len)
                .flat_map(|i| (0..lanes).map(move |lane| (i, lane)))
                .map(|(i, lane)| id(node(i, lane)))
                .collect(),
            rest: edges.iter().map(|&(a, b)| rest[a].distance(rest[b])).collect(),
            alpha: alphas,
            lambda: vec![0.0; count],
            normal: vec![Vec3A::ZERO; count],
            diag: vec![0.0; count],
            rhs: vec![0.0; count],
            upper: vec![0.0; count],
            factor: vec![0.0; count],
            step: vec![0.0; count],
            active,
        }
    }

    fn clear_multipliers(&mut self) {
        self.lambda.fill(0.0);
    }

    /// Each simulated edge's violation and `α̃`, for the energy.
    fn edges<'a>(&'a self, x: &'a [Vec3A]) -> impl Iterator<Item = (f32, f32)> + 'a {
        let lanes = self.lanes;
        (0..self.alpha.len()).filter(|&e| self.alpha[e] >= 0.0).map(move |e| {
            let (a, b) = (self.nodes[e] as usize, self.nodes[e + lanes] as usize);
            (x[a].distance(x[b]) - self.rest[e], self.alpha[e])
        })
    }

    /// One exact solve of every chain's linearised system, then the move it asks for.
    fn solve(&mut self, x: &mut [Vec3A], w: &[f32]) {
        let (lanes, len) = (self.lanes, self.len);
        let count = len * lanes;
        // Slices of their exact lengths, so the indexing below by `e < count` (and `e + lanes` into the nodes) needs
        // no bounds checks.
        let nodes = &self.nodes[..count + lanes];
        let (rest, alpha) = (&self.rest[..count], &self.alpha[..count]);
        let (lambda, normal) = (&mut self.lambda[..count], &mut self.normal[..count]);
        let (diag, rhs, upper) = (&mut self.diag[..count], &mut self.rhs[..count], &mut self.upper[..count]);
        let (factor, step) = (&mut self.factor[..count], &mut self.step[..count]);
        // Linearise each edge where the particles are now.
        for e in 0..count {
            let (first, second) = (nodes[e] as usize, nodes[e + lanes] as usize);
            let d = x[first] - x[second];
            let length = d.length();
            if alpha[e] < 0.0 || length <= DEGENERATE {
                // Not simulated, or no direction this time: a row of the identity, which solves to `Δλ = 0`.
                (normal[e], diag[e], rhs[e]) = (Vec3A::ZERO, 1.0, 0.0);
            } else {
                normal[e] = d * length.recip();
                diag[e] = (w[first] + w[second] + alpha[e]) * (1.0 + REGULARISE);
                rhs[e] = rest[e] - length - alpha[e] * lambda[e];
            }
        }
        // Forward elimination. Edges k and k + 1 share node k + 1, where their gradients are -n_k and n_(k+1):
        // A[k][k + 1] = -w(node k + 1) n_k · n_(k+1).
        for k in 0..len {
            let (first, last) = (k == 0, k + 1 == len);
            for e in k * lanes..(k + 1) * lanes {
                let up = if last { 0.0 } else { -w[nodes[e + lanes] as usize] * normal[e].dot(normal[e + lanes]) };
                let (sub, f, s) =
                    if first { (0.0, 0.0, 0.0) } else { (upper[e - lanes], factor[e - lanes], step[e - lanes]) };
                let inv = (diag[e] - sub * f).recip();
                upper[e] = up;
                factor[e] = up * inv;
                step[e] = (rhs[e] - sub * s) * inv;
            }
        }
        // Back substitution, last edge first, leaves each edge's `Δλ` in `step`; as soon as both edges at a node are
        // known, the node moves: it is edge k's second end (gradient -n_k) and edge k + 1's first (gradient n_(k+1)).
        for k in (0..len).rev() {
            let last = k + 1 == len;
            for e in k * lanes..(k + 1) * lanes {
                let mut dx = Vec3A::ZERO;
                if !last {
                    step[e] -= factor[e] * step[e + lanes];
                    dx = normal[e + lanes] * step[e + lanes];
                }
                lambda[e] += step[e];
                dx -= normal[e] * step[e];
                let p = nodes[e + lanes] as usize;
                x[p] += dx * w[p];
            }
        }
        // Each chain's first node is only its first edge's first end.
        for e in 0..lanes.min(count) {
            let p = nodes[e] as usize;
            x[p] += normal[e] * (step[e] * w[p]);
        }
    }

    /// The `α̃` of the simulated edge from particle `a` to `b`, for tests.
    #[cfg(test)]
    fn alpha_of(&self, a: usize, b: usize) -> Option<f32> {
        let lanes = self.lanes;
        (0..self.alpha.len())
            .find(|&e| (self.nodes[e] as usize, self.nodes[e + lanes] as usize) == (a, b))
            .map(|e| self.alpha[e])
            .filter(|&alpha| alpha >= 0.0)
    }
}

/// Particle indices fit in `u32` (checked when the sheet is built), which halves the constraints' index size.
#[allow(clippy::cast_possible_truncation)]
fn id(index: usize) -> u32 {
    index as u32
}

/// A compliance `α` in m/N as the sub-step's `α̃ = α / h²`, or `None` when that is infinite: the constraint could not
/// move anything (and `∞ · 0` would turn its first projection into NaN), so it is left out.
fn per_substep(alpha: f32, h: f32) -> Option<f32> {
    Some(alpha / (h * h)).filter(|a| a.is_finite())
}

/// What [`Constraints::build`] needs from the sheet and the parameters.
pub(crate) struct Inputs<'a> {
    pub(crate) topology: SheetShape,
    pub(crate) rest: &'a [Vec3],
    /// The solver's inverse masses (hard pins are zero).
    pub(crate) inv_mass: &'a [f32],
    /// Each pin slot's particle and weight.
    pub(crate) pins: &'a [(usize, f32)],
    /// `ClothParams` compliances (`f32::INFINITY` turns a family off).
    pub(crate) stretch: f32,
    pub(crate) shear: f32,
    pub(crate) bend: f32,
    pub(crate) max_stretch: f32,
    /// The sub-step, seconds.
    pub(crate) h: f32,
}

/// Every constraint of a sheet, in the fixed order the Gauss-Seidel sweeps visit them.
#[derive(Clone, Debug)]
pub(crate) struct Constraints {
    /// The warp edges, down each column, solved chain by chain.
    columns: Chains,
    /// The weft edges, along each row (a wrapped row as an open chain), solved chain by chain.
    rows: Chains,
    /// A wrapped sheet's seam edges (each row's last particle to its first), projected one by one.
    seams: Vec<Distance>,
    pub(crate) shear: Vec<Distance>,
    pub(crate) bend: Vec<Bend>,
    pub(crate) attach: Vec<Attach>,
    /// Every movable warp and weft edge, for the limiter and the strain report.
    limit: Vec<Limit>,
    /// `1 + max_stretch`: the longest an edge may be, as a multiple of its rest length (infinite: no limit).
    longest: f32,
}

impl Constraints {
    /// Builds the constraints of a validated sheet. A family whose compliance is infinite is left out, and so is any
    /// constraint all of whose particles are immovable: it could never move anything.
    pub(crate) fn build(input: &Inputs<'_>) -> Self {
        let t = input.topology;
        let (rest, w, h) = (input.rest, input.inv_mass, input.h);
        let areas = cell_areas(t, rest);
        let movable = |ids: &[usize]| ids.iter().any(|&i| w[i] > 0.0);
        let distance = |(a, b): (usize, usize), alpha: f32| {
            Some(Distance::new([a, b], rest[a].distance(rest[b]), per_substep(alpha, h)?, w))
        };

        let edges: Vec<(usize, usize)> = structural_edges(t).filter(|&(a, b)| movable(&[a, b])).collect();
        let edge_scale = |e: (usize, usize)| rest[e.0].distance_squared(rest[e.1]) / edge_area(t, &areas, e);
        let stretch_alpha =
            |e: (usize, usize)| per_substep(input.stretch * edge_scale(e), h).filter(|_| movable(&[e.0, e.1]));
        let columns = Chains::new(t.cols, t.rows - 1, |i, lane| t.at(i, lane), rest, stretch_alpha);
        let rows = Chains::new(t.rows, t.cols - 1, |i, lane| t.at(lane, i), rest, stretch_alpha);
        let seam = |r: usize| (t.at(r, t.cols - 1), t.at(r, t.cols));
        let seams = (0..t.rows)
            .filter(|_| t.wrap)
            .map(seam)
            .filter(|&(a, b)| movable(&[a, b]))
            .filter_map(|e| distance(e, input.stretch * edge_scale(e)))
            .collect();
        let limit = limits(t, rest, w, input.pins, &edges);

        let shear = diagonals(t)
            .filter(|&(_, (a, b))| movable(&[a, b]))
            .filter_map(|(cell, e)| {
                distance(e, input.shear * 2.0 * areas[cell] / rest[e.0].distance_squared(rest[e.1]))
            })
            .collect();

        let attach = input
            .pins
            .iter()
            .enumerate()
            // A soft pin on an immovable particle could not move it (and with `stretch = 0` its update would divide by
            // zero).
            .filter(|&(_, &(particle, weight))| weight < 1.0 && w[particle] > 0.0)
            .filter_map(|(slot, &(particle, weight))| {
                let alpha = per_substep(input.stretch * (1.0 - weight) / weight, h)?;
                Some(Attach { particle: id(particle), slot: id(slot), alpha, lambda: 0.0 })
            })
            .collect();

        Self {
            columns,
            rows,
            seams,
            shear,
            bend: bends(t, rest, w, input.bend, h),
            attach,
            limit,
            longest: 1.0 + input.max_stretch,
        }
    }

    /// Restarts every multiplier at zero, as each sub-step must.
    pub(crate) fn clear_multipliers(&mut self) {
        self.columns.clear_multipliers();
        self.rows.clear_multipliers();
        self.seams.iter_mut().chain(&mut self.shear).for_each(|c| c.lambda = 0.0);
        self.bend.iter_mut().for_each(|c| c.lambda = 0.0);
        self.attach.iter_mut().for_each(|c| c.lambda = 0.0);
    }

    /// One block Gauss-Seidel sweep over every constraint: the column chains, the row chains and seams, shear, bend,
    /// soft pins, or the reverse of that order (alternating directions keeps a fixed order from biasing the sheet toward
    /// one side). `targets` holds each soft pin's target for this sub-step, by slot.
    pub(crate) fn sweep(&mut self, x: &mut [Vec3A], w: &[f32], targets: &[Vec3A], forward: bool) {
        if forward {
            self.columns.solve(x, w);
            self.rows.solve(x, w);
            self.seams.iter_mut().for_each(|c| c.project(x, w));
            self.shear.iter_mut().for_each(|c| c.project(x, w));
            self.bend.iter_mut().for_each(|c| c.project(x, w));
            self.attach.iter_mut().for_each(|c| c.project(x, w, targets));
        } else {
            self.attach.iter_mut().rev().for_each(|c| c.project(x, w, targets));
            self.bend.iter_mut().rev().for_each(|c| c.project(x, w));
            self.shear.iter_mut().rev().for_each(|c| c.project(x, w));
            self.seams.iter_mut().rev().for_each(|c| c.project(x, w));
            self.rows.solve(x, w);
            self.columns.solve(x, w);
        }
    }

    /// One pass of the stretch limiter, from the pins outward. Nothing to do when `max_stretch` is infinite.
    pub(crate) fn limit_stretch(&self, x: &mut [Vec3A]) {
        let longest = self.longest;
        if longest.is_finite() {
            self.limit.iter().for_each(|c| c.apply(x, longest));
        }
    }

    /// The elastic energy `Σ C² / 2α` (joules) of every compliant constraint, for a sub-step of `h` seconds. Rigid
    /// constraints (`α = 0`) store no energy: they hold `C = 0`.
    pub(crate) fn elastic_energy(&self, x: &[Vec3A], targets: &[Vec3A], h: f32) -> f64 {
        let energy = |c: f32, alpha: f32| {
            if alpha > 0.0 { f64::from(c) * f64::from(c) / (2.0 * f64::from(alpha * h * h)) } else { 0.0 }
        };
        let chains = self.columns.edges(x).chain(self.rows.edges(x)).map(|(c, alpha)| energy(c, alpha));
        let distances = self.seams.iter().chain(&self.shear).map(|d| energy(d.violation(x), d.alpha)).chain(chains);
        let bends = self.bend.iter().map(|b| energy(b.violation(x), b.alpha));
        let pins =
            self.attach.iter().map(|p| energy(x[p.particle as usize].distance(targets[p.slot as usize]), p.alpha));
        distances.chain(bends).chain(pins).sum()
    }

    /// The largest strain `|x_a - x_b| / L - 1` over the movable warp and weft edges (0 when none is stretched).
    pub(crate) fn max_strain(&self, x: &[Vec3A]) -> f32 {
        self.limit.iter().map(|c| x[c.a as usize].distance(x[c.b as usize]) / c.rest - 1.0).fold(0.0, f32::max)
    }

    /// The constraints per family.
    pub(crate) fn counts(&self) -> ConstraintCounts {
        ConstraintCounts {
            stretch: self.columns.active + self.rows.active + self.seams.len(),
            shear: self.shear.len(),
            bend: self.bend.len(),
            soft_pins: self.attach.len(),
            limited_edges: if self.longest.is_finite() { self.limit.len() } else { 0 },
        }
    }
}

/// How many constraints of each family a solver projects per iteration (the cost model's inputs).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConstraintCounts {
    /// Warp and weft springs.
    pub stretch: usize,
    /// Diagonal springs, two per cell.
    pub shear: usize,
    /// Bend triples along rows and columns.
    pub bend: usize,
    /// Soft pins (weight below 1).
    pub soft_pins: usize,
    /// Edges the stretch limiter checks once per sub-step.
    pub limited_edges: usize,
}

impl ConstraintCounts {
    /// Every constraint an iteration projects (the limiter runs once per sub-step and is not included).
    pub fn per_iteration(&self) -> usize {
        self.stretch + self.shear + self.bend + self.soft_pins
    }
}

/// The bend triples: along each row (around the seam of a wrapped sheet), then down each column.
fn bends(t: SheetShape, rest: &[Vec3], w: &[f32], bend: f32, h: f32) -> Vec<Bend> {
    if !bend.is_finite() {
        return Vec::new();
    }
    let areas = particle_areas(t, rest);
    // Along a row, every third triple first, so consecutive triples share no particle.
    let along = (0..t.rows).flat_map(move |r| {
        let (first, end) = if t.wrap { (0, t.cols) } else { (1, t.cols - 1) };
        let middles = (0..3).flat_map(move |k| (first + k..end).step_by(3));
        middles.map(move |c| [t.at(r, c + t.cols - 1), t.at(r, c), t.at(r, c + 1)])
    });
    let down =
        (1..t.rows - 1).flat_map(move |r| (0..t.cols).map(move |c| [t.at(r - 1, c), t.at(r, c), t.at(r + 1, c)]));
    along
        .chain(down)
        .filter(|ids| ids.iter().any(|&i| w[i] > 0.0))
        .filter_map(|[e0, m, e1]| {
            let spans = rest[e0].distance(rest[m]) * rest[m].distance(rest[e1]);
            let centroid = (rest[e0] + rest[m] + rest[e1]) * THIRD;
            let alpha = per_substep(bend * spans * spans / (9.0 * areas[m]), h)?;
            Some(Bend::new([e0, m, e1], rest[m].distance(centroid), alpha, w))
        })
        .collect()
}

/// Each particle's distance in edges from the nearest pinned or immovable particle (a breadth-first search over the
/// warp and weft edges); `u32::MAX` for every particle when there is none.
fn depths(t: SheetShape, w: &[f32], pins: &[(usize, f32)]) -> Vec<u32> {
    let mut depth = vec![u32::MAX; w.len()];
    let mut queue: std::collections::VecDeque<usize> =
        w.iter().enumerate().filter(|(_, w)| **w == 0.0).map(|(i, _)| i).collect();
    queue.extend(pins.iter().map(|&(p, _)| p));
    for &root in &queue {
        depth[root] = 0;
    }
    let mut neighbours: Vec<Vec<usize>> = vec![Vec::new(); w.len()];
    for (a, b) in structural_edges(t) {
        neighbours[a].push(b);
        neighbours[b].push(a);
    }
    while let Some(i) = queue.pop_front() {
        for &n in &neighbours[i] {
            if depth[n] == u32::MAX {
                depth[n] = depth[i] + 1;
                queue.push_back(n);
            }
        }
    }
    depth
}

/// The limiter's edges, in the order one pass visits them: by the depth of their deeper end, and at each depth the
/// edges between equals before the edges that come down from the depth above, so those finish exact.
fn limits(t: SheetShape, rest: &[Vec3], w: &[f32], pins: &[(usize, f32)], edges: &[(usize, usize)]) -> Vec<Limit> {
    let depth = depths(t, w, pins);
    let mut limits: Vec<(u32, bool, Limit)> = edges
        .iter()
        .map(|&(a, b)| {
            let (da, db) = (depth[a], depth[b]);
            let share = match da.cmp(&db) {
                std::cmp::Ordering::Less => [0.0, 1.0],
                std::cmp::Ordering::Greater => [1.0, 0.0],
                std::cmp::Ordering::Equal => [w[a] / (w[a] + w[b]), w[b] / (w[a] + w[b])],
            };
            (da.max(db), da != db, Limit { a: id(a), b: id(b), rest: rest[a].distance(rest[b]), share })
        })
        .collect();
    // A stable sort keeps row order among equals, so the pass order is fixed.
    limits.sort_by_key(|&(deeper, descends, _)| (deeper, descends));
    limits.into_iter().map(|(_, _, limit)| limit).collect()
}

impl Distance {
    /// The constraint between particles `a` and `b`, given the solver's inverse masses `w`.
    fn new([a, b]: [usize; 2], rest: f32, alpha: f32, w: &[f32]) -> Self {
        Self { a: id(a), b: id(b), rest, alpha, inv_den: 1.0 / (w[a] + w[b] + alpha), lambda: 0.0 }
    }

    fn violation(&self, x: &[Vec3A]) -> f32 {
        x[self.a as usize].distance(x[self.b as usize]) - self.rest
    }

    /// XPBD projection; `∇_a C = n`, `∇_b C = -n` with `n` the unit vector from `b` to `a`.
    #[inline]
    fn project(&mut self, pos: &mut [Vec3A], w: &[f32]) {
        let (ia, ib) = (self.a as usize, self.b as usize);
        let (wa, wb) = (w[ia], w[ib]);
        let d = pos[ia] - pos[ib];
        let length = d.length();
        if length <= DEGENERATE {
            return;
        }
        let dl = (self.rest - length - self.alpha * self.lambda) * self.inv_den;
        self.lambda += dl;
        let step = d * (dl / length);
        pos[ia] += step * wa;
        pos[ib] -= step * wb;
    }
}

impl Bend {
    /// The constraint bending at `middle` between `e0` and `e1`, given the solver's inverse masses `w`.
    fn new([e0, middle, e1]: [usize; 3], rest: f32, alpha: f32, w: &[f32]) -> Self {
        let weight = (4.0 * w[middle] + w[e0] + w[e1]) / 9.0;
        Self { ends: [id(e0), id(e1)], middle: id(middle), rest, alpha, inv_den: 1.0 / (weight + alpha), lambda: 0.0 }
    }

    fn violation(&self, x: &[Vec3A]) -> f32 {
        let [e0, e1] = self.ends.map(|e| x[e as usize]);
        let m = x[self.middle as usize];
        (m - (e0 + m + e1) * THIRD).length() - self.rest
    }

    /// XPBD projection; with `n` the unit vector from the centroid to the middle particle, `∇_middle C = 2n/3` and
    /// `∇_end C = -n/3`, so `Σ w |∇C|² = (4 w_middle + w_0 + w_1) / 9`.
    #[inline]
    fn project(&mut self, x: &mut [Vec3A], w: &[f32]) {
        let ([e0, e1], m) = (self.ends.map(|e| e as usize), self.middle as usize);
        let (w0, wm, w1) = (w[e0], w[m], w[e1]);
        let d = x[m] - (x[e0] + x[m] + x[e1]) * THIRD;
        let length = d.length();
        if length <= DEGENERATE {
            return;
        }
        let dl = (self.rest - length - self.alpha * self.lambda) * self.inv_den;
        self.lambda += dl;
        let step = d * (dl * THIRD / length);
        x[m] += step * (2.0 * wm);
        x[e0] -= step * w0;
        x[e1] -= step * w1;
    }
}

impl Attach {
    /// XPBD projection toward the pin's target; `∇C` is the unit vector from the target to the particle.
    #[inline]
    fn project(&mut self, x: &mut [Vec3A], w: &[f32], targets: &[Vec3A]) {
        let p = self.particle as usize;
        let wp = w[p];
        let d = x[p] - targets[self.slot as usize];
        let length = d.length();
        if length <= DEGENERATE {
            return;
        }
        let dl = (-length - self.alpha * self.lambda) / (wp + self.alpha);
        self.lambda += dl;
        x[p] += d * (wp * dl / length);
    }
}

impl Limit {
    /// Pulls an edge longer than `longest` times its rest length back to that, each end moving by its share.
    #[inline]
    fn apply(&self, pos: &mut [Vec3A], longest: f32) {
        let (ia, ib) = (self.a as usize, self.b as usize);
        let d = pos[ia] - pos[ib];
        let squared = d.length_squared();
        let max = longest * self.rest;
        if squared <= max * max {
            return;
        }
        let length = squared.sqrt();
        let step = d * ((length - max) / length);
        pos[ia] -= step * self.share[0];
        pos[ib] += step * self.share[1];
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // small grid indices
mod tests {
    use super::*;

    fn flat(t: SheetShape, spacing: f32) -> Vec<Vec3> {
        (0..t.rows * t.cols)
            .map(|i| Vec3::new((i % t.cols) as f32 * spacing, 0.0, (i / t.cols) as f32 * spacing))
            .collect()
    }

    fn build(t: SheetShape, rest: &[Vec3], w: &[f32]) -> Constraints {
        Constraints::build(&Inputs {
            topology: t,
            rest,
            inv_mass: w,
            pins: &[],
            stretch: 1e-3,
            shear: 2e-3,
            bend: 3e-3,
            max_stretch: 0.05,
            h: 1.0,
        })
    }

    #[test]
    fn a_square_grid_has_the_documented_compliances() {
        let t = SheetShape { rows: 4, cols: 5, wrap: false };
        let rest = flat(t, 0.1);
        let c = build(t, &rest, &[1.0; 20]);
        // Interior warp/weft edges stand for one cell: α = stretch. Border edges stand for half a cell: 2·stretch.
        let interior = c.rows.alpha_of(6, 7);
        let border = c.rows.alpha_of(0, 1);
        assert_eq!(c.columns.alpha_of(1, 6), interior, "a column edge between interior cells: the same");
        assert!((interior.unwrap() - 1e-3).abs() < 1e-9);
        assert!((border.unwrap() - 2e-3).abs() < 1e-9);
        // Diagonals of a square cell: α = shear.
        assert!(c.shear.iter().all(|d| (d.alpha - 2e-3).abs() < 1e-9));
        // An interior bend triple: α = bend · L⁴ / (9 L²) = bend · L² / 9.
        let middle = c.bend.iter().find(|b| b.middle == 7).map(|b| b.alpha).unwrap();
        assert!((middle - 3e-3 * 0.01 / 9.0).abs() < 1e-9);
    }

    #[test]
    fn counts_follow_the_grid_and_the_seam() {
        let open = SheetShape { rows: 3, cols: 4, wrap: false };
        let ring = SheetShape { rows: 3, cols: 4, wrap: true };
        let rest = flat(open, 0.1);
        let w = vec![1.0; 12];
        let c = build(open, &rest, &w);
        // 3 rows of 3 edges along, 2 rows of 4 edges down; 6 cells of 2 diagonals; 3 x 2 along + 1 x 4 down bends.
        assert_eq!((c.counts().stretch, c.shear.len(), c.bend.len()), (17, 12, 10));
        // A ring (geometry is irrelevant to the counts): 4 edges along each row, 8 cells, every particle bends along.
        let c = build(ring, &rest, &w);
        assert_eq!((c.counts().stretch, c.shear.len(), c.bend.len()), (20, 16, 16));
        let seams: Vec<(u32, u32)> = c.seams.iter().map(|d| (d.a, d.b)).collect();
        assert_eq!(seams, vec![(3, 0), (7, 4), (11, 8)], "each row's seam, last particle to first");
    }

    #[test]
    fn immovable_constraints_and_infinite_families_are_dropped() {
        let t = SheetShape { rows: 2, cols: 3, wrap: false };
        let rest = flat(t, 0.1);
        // The whole top row is immovable: its two edges along the row go.
        let c = build(t, &rest, &[0.0, 0.0, 0.0, 1.0, 1.0, 1.0]);
        assert_eq!(c.counts().stretch, 7 - 2);
        let off = Constraints::build(&Inputs {
            topology: t,
            rest: &rest,
            inv_mass: &[1.0; 6],
            pins: &[],
            stretch: f32::INFINITY,
            shear: f32::INFINITY,
            bend: f32::INFINITY,
            max_stretch: f32::INFINITY,
            h: 1.0,
        });
        assert_eq!(off.counts(), ConstraintCounts::default());
    }

    #[test]
    fn a_single_projection_solves_a_lone_rigid_spring() {
        let w = [1.0, 3.0];
        let mut d = Distance::new([0, 1], 1.0, 0.0, &w);
        let mut x = [Vec3A::ZERO, Vec3A::new(3.0, 0.0, 0.0)];
        d.project(&mut x, &w);
        // The correction is shared by inverse mass: a moves 1/4 of the 2 m excess, b 3/4.
        assert!((x[0] - Vec3A::new(0.5, 0.0, 0.0)).length() < 1e-6);
        assert!((x[1] - Vec3A::new(1.5, 0.0, 0.0)).length() < 1e-6);
    }

    #[test]
    fn the_bend_projection_straightens_a_kink_and_conserves_momentum() {
        let w = [1.0, 1.0, 1.0];
        let mut b = Bend::new([0, 1, 2], 0.0, 0.0, &w);
        let mut x = [Vec3A::ZERO, Vec3A::new(1.0, 0.3, 0.0), Vec3A::new(2.0, 0.0, 0.0)];
        let before = x[0] + x[1] + x[2];
        b.project(&mut x, &w);
        assert!(b.violation(&x).abs() < 1e-6, "a rigid bend is solved in one projection");
        assert!((x[0] + x[1] + x[2] - before).length() < 1e-6, "equal masses: the centre of mass stays");
    }

    #[test]
    fn the_limiter_only_shortens() {
        let l = Limit { a: 0, b: 1, rest: 1.0, share: [0.0, 1.0] };
        let mut x = [Vec3A::ZERO, Vec3A::new(2.0, 0.0, 0.0)];
        l.apply(&mut x, 1.05);
        assert!((x[1].x - 1.05).abs() < 1e-6 && x[0] == Vec3A::ZERO);
        let mut short = [Vec3A::ZERO, Vec3A::new(0.5, 0.0, 0.0)];
        l.apply(&mut short, 1.05);
        assert_eq!(short[1].x, 0.5);
    }
}

#[cfg(test)]
#[allow(clippy::cast_precision_loss)] // small grid indices
mod limiter_order {
    use super::*;

    #[test]
    fn edges_run_from_the_pins_outward_and_descending_edges_move_only_their_lower_end() {
        // 3 rows of 2, the top row pinned: depths 0, 1, 2.
        let t = SheetShape { rows: 3, cols: 2, wrap: false };
        let rest: Vec<Vec3> = (0..6).map(|i| Vec3::new((i % 2) as f32, -((i / 2) as f32), 0.0)).collect();
        let w = [0.0, 0.0, 1.0, 1.0, 1.0, 1.0];
        let edges: Vec<(usize, usize)> = structural_edges(t).filter(|&(a, b)| w[a] > 0.0 || w[b] > 0.0).collect();
        let order: Vec<_> =
            limits(t, &rest, &w, &[(0, 1.0), (1, 1.0)], &edges).iter().map(|l| (l.a, l.b, l.share)).collect();
        assert_eq!(
            order,
            vec![
                (2, 3, [0.5, 0.5]), // the hoop at depth 1
                (0, 2, [0.0, 1.0]), // down into depth 1
                (1, 3, [0.0, 1.0]),
                (4, 5, [0.5, 0.5]), // depth 2
                (2, 4, [0.0, 1.0]),
                (3, 5, [0.0, 1.0]),
            ]
        );
    }

    #[test]
    fn one_pass_holds_a_long_column_within_the_limit() {
        let t = SheetShape { rows: 40, cols: 2, wrap: false };
        let rest: Vec<Vec3> = (0..80).map(|i| Vec3::new((i % 2) as f32, -((i / 2) as f32), 0.0)).collect();
        let mut w = vec![1.0; 80];
        w[0] = 0.0;
        w[1] = 0.0;
        let c = Constraints::build(&Inputs {
            topology: t,
            rest: &rest,
            inv_mass: &w,
            pins: &[(0, 1.0), (1, 1.0)],
            stretch: f32::INFINITY,
            shear: f32::INFINITY,
            bend: f32::INFINITY,
            max_stretch: 0.05,
            h: 1.0,
        });
        // Stretch the whole column to twice its length.
        let mut x: Vec<Vec3A> = rest.iter().map(|p| Vec3A::new(p.x, 2.0 * p.y, 0.0)).collect();
        c.limit_stretch(&mut x);
        assert!(c.max_strain(&x) <= 0.05 + 1e-5, "max strain {}", c.max_strain(&x));
    }
}
