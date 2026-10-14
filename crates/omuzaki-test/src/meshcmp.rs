//! Mesh comparison: how far apart two triangle meshes are, with and without a vertex-for-vertex correspondence.
//!
//! This is the geometry half of the parity oracle for the character port: the JS prototype's meshes (see [`crate::jsref`])
//! against the Rust builders' output. Two views, because a port can match the prototype's *shape* without matching its
//! vertex *order*:
//!
//! - **In order** (needs equal vertex counts): the distance between vertex `i` of each mesh, and the angle between their
//!   normals.
//! - **Unordered**: the symmetric Chamfer and Hausdorff distances between the two vertex sets, found with a uniform grid
//!   so 100k vertices take milliseconds, not minutes.
//!
//! Distances are between *vertices*, not surfaces: two meshes of the same surface at different densities differ by about
//! half their edge length. Compare meshes built the same way.
//!
//! [`MeshTolerance::T_GEOM`] is the acceptance rule: in order within 2e-4 m and
//! 1 degree, else Chamfer within 3e-4 m and Hausdorff within 1.5e-3 m.

use std::fmt;
use std::io::{self, Write};
use std::ops::Deref;

use omuzaki_core::glam::Vec3;
use omuzaki_core::mesh::MeshData;

/// How many worst vertices a report lists per measure.
pub const WORST_LIMIT: usize = 10;

/// How far two meshes may drift and still count as the same geometry. Distances are in metres.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MeshTolerance {
    /// The largest allowed distance between vertex `i` of each mesh, when the vertex order matches.
    pub max_vertex_error: f32,
    /// The largest allowed angle between vertex `i`'s normals, in degrees, when the vertex order matches.
    pub max_normal_angle_deg: f32,
    /// The largest allowed Chamfer distance, when the vertex order does not match.
    pub max_chamfer: f32,
    /// The largest allowed Hausdorff distance, when the vertex order does not match.
    pub max_hausdorff: f32,
}

impl MeshTolerance {
    /// The wave-3 parity rule for geometry (starting values; the first parity PR measures the noise floor).
    pub const T_GEOM: Self =
        Self { max_vertex_error: 2e-4, max_normal_angle_deg: 1.0, max_chamfer: 3e-4, max_hausdorff: 1.5e-3 };
}

/// Which of the two compared meshes something is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The mesh under test.
    Actual,
    /// The reference.
    Expected,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Actual => "actual",
            Self::Expected => "expected",
        })
    }
}

/// A measure with a tolerance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Metric {
    /// Distance between corresponding vertices.
    VertexError,
    /// Angle between corresponding normals, in degrees.
    NormalAngle,
    /// Symmetric Chamfer distance.
    Chamfer,
    /// Symmetric Hausdorff distance.
    Hausdorff,
}

impl Metric {
    fn label(self) -> &'static str {
        match self {
            Self::VertexError => "vertex error",
            Self::NormalAngle => "normal angle",
            Self::Chamfer => "Chamfer",
            Self::Hausdorff => "Hausdorff",
        }
    }

    fn unit(self) -> &'static str {
        if self == Self::NormalAngle { "deg" } else { "m" }
    }
}

/// Something a reviewer should know about a comparison.
#[derive(Debug, Clone, PartialEq)]
pub enum Finding {
    /// A mesh breaks [`MeshData::validate`], so no distance means anything.
    Invalid {
        /// The offending mesh.
        side: Side,
        /// The rule it breaks.
        reason: String,
    },
    /// The vertex counts differ, so only the unordered measures apply. Not a failure by itself: a port may weld or split
    /// vertices differently and still match the shape.
    VertexCount {
        /// Vertices in the mesh under test.
        actual: usize,
        /// Vertices in the reference.
        expected: usize,
    },
    /// The triangle counts differ. Not a failure by itself, for the same reason.
    TriangleCount {
        /// Triangles in the mesh under test.
        actual: usize,
        /// Triangles in the reference.
        expected: usize,
    },
    /// A measure is over its limit.
    Exceeded {
        /// Which measure.
        metric: Metric,
        /// What was measured.
        value: f32,
        /// What was allowed.
        limit: f32,
    },
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invalid { side, reason } => write!(f, "{side} mesh is invalid: {reason}"),
            Self::VertexCount { actual, expected } => {
                write!(f, "vertex count differs: {actual} actual vs {expected} expected")
            }
            Self::TriangleCount { actual, expected } => {
                write!(f, "triangle count differs: {actual} actual vs {expected} expected")
            }
            Self::Exceeded { metric, value, limit } => {
                write!(f, "{} {value:.3e} {unit} exceeds {limit:.3e} {unit}", metric.label(), unit = metric.unit())
            }
        }
    }
}

/// How the comparison came out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    /// Same vertex order: every vertex and normal is within tolerance.
    InOrder,
    /// The vertices are not in the same place index for index, but the vertex sets match within the Chamfer and
    /// Hausdorff tolerances.
    Unordered,
    /// Outside tolerance, or not comparable. See [`MeshReport::findings`].
    Failed,
}

/// Per-vertex numbers. Debug-prints a summary, because 100k floats in an assertion message help nobody.
#[derive(Clone, PartialEq, Default)]
pub struct VertexErrors(Vec<f32>);

impl Deref for VertexErrors {
    type Target = [f32];
    fn deref(&self) -> &[f32] {
        &self.0
    }
}

impl fmt::Debug for VertexErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VertexErrors({} values, max {:.3e})", self.0.len(), max_of(&self.0))
    }
}

/// One entry of a worst-vertices list.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorstVertex {
    /// The vertex index (in the mesh under test for in-order measures).
    pub index: usize,
    /// Its error: metres, or degrees for normals.
    pub value: f32,
}

/// Vertex `i` against vertex `i`. Present only when the vertex counts match.
#[derive(Debug, Clone, PartialEq)]
pub struct InOrderStats {
    /// Distance between corresponding vertices, in metres, vertex order.
    pub position_errors: VertexErrors,
    /// The largest of `position_errors`.
    pub max_error: f32,
    /// The mean of `position_errors`.
    pub mean_error: f32,
    /// Angle between corresponding normals, in degrees, vertex order.
    pub normal_angles_deg: VertexErrors,
    /// The largest of `normal_angles_deg`.
    pub max_normal_angle_deg: f32,
    /// The [`WORST_LIMIT`] vertices with the largest position error, worst first (zero errors are left out).
    pub worst_positions: Vec<WorstVertex>,
    /// The [`WORST_LIMIT`] vertices with the largest normal angle, worst first (zero angles are left out).
    pub worst_normals: Vec<WorstVertex>,
}

/// The two vertex sets against each other, ignoring order.
#[derive(Debug, Clone, PartialEq)]
pub struct UnorderedStats {
    /// For each vertex of the mesh under test, the distance to the nearest expected vertex.
    pub actual_to_expected: VertexErrors,
    /// For each expected vertex, the distance to the nearest vertex of the mesh under test.
    pub expected_to_actual: VertexErrors,
    /// Half the sum of the two directions' mean distances, in metres. Two single-vertex meshes `d` apart give `d`.
    pub chamfer: f32,
    /// The largest nearest-vertex distance in either direction, in metres.
    pub hausdorff: f32,
}

/// How far the bounding boxes differ: actual minus expected, per corner.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BboxDelta {
    /// Minimum corner difference.
    pub min: Vec3,
    /// Maximum corner difference.
    pub max: Vec3,
}

impl BboxDelta {
    /// The largest absolute coordinate difference of either corner.
    pub fn max_abs(&self) -> f32 {
        self.min.abs().max(self.max.abs()).max_element()
    }
}

/// A count in each mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Counts {
    /// In the mesh under test.
    pub actual: usize,
    /// In the reference.
    pub expected: usize,
}

/// Everything [`compare_meshes`] found.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshReport {
    /// The tolerance the verdict was judged against.
    pub tolerance: MeshTolerance,
    /// Vertex counts.
    pub vertices: Counts,
    /// Triangle counts.
    pub triangles: Counts,
    /// The in-order measures; `None` when the vertex counts differ or a mesh is invalid.
    pub in_order: Option<InOrderStats>,
    /// The unordered measures; `None` when a mesh is invalid.
    pub unordered: Option<UnorderedStats>,
    /// Bounding-box difference; `None` when a mesh is invalid. Informational: the distances above already bound it.
    pub bbox_delta: Option<BboxDelta>,
    /// The verdict.
    pub verdict: Verdict,
    /// Count mismatches always, plus every violated limit when the verdict is [`Verdict::Failed`].
    pub findings: Vec<Finding>,
}

impl MeshReport {
    /// True unless the verdict is [`Verdict::Failed`].
    pub fn passed(&self) -> bool {
        self.verdict != Verdict::Failed
    }
}

impl fmt::Display for MeshReport {
    /// A short multi-line summary with fixed rounding, so repeated runs print identical text.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let verdict = match self.verdict {
            Verdict::InOrder => "PASS (vertex order)",
            Verdict::Unordered => "PASS (unordered)",
            Verdict::Failed => "FAIL",
        };
        writeln!(
            f,
            "{verdict}: {} vs {} vertices, {} vs {} triangles",
            self.vertices.actual, self.vertices.expected, self.triangles.actual, self.triangles.expected
        )?;
        if let Some(s) = &self.in_order {
            write!(
                f,
                "  in order: max {:.3e} m, mean {:.3e} m, max normal {:.3} deg",
                s.max_error, s.mean_error, s.max_normal_angle_deg
            )?;
            if let Some(w) = s.worst_positions.first() {
                write!(f, ", worst vertex {} ({:.3e} m)", w.index, w.value)?;
            }
            writeln!(f)?;
        }
        if let Some(s) = &self.unordered {
            writeln!(f, "  unordered: Chamfer {:.3e} m, Hausdorff {:.3e} m", s.chamfer, s.hausdorff)?;
        }
        if let Some(b) = &self.bbox_delta {
            writeln!(f, "  bbox delta: max {:.3e} m", b.max_abs())?;
        }
        self.findings.iter().try_for_each(|finding| writeln!(f, "  finding: {finding}"))
    }
}

/// Compares `actual` against the reference `expected` under `tolerance`.
///
/// **The rule**, applied in this order:
/// 1. Invalid meshes (anything [`MeshData::validate`] rejects) fail with a [`Finding::Invalid`] and no numbers.
/// 2. With equal vertex counts and every vertex within `max_vertex_error`, the mesh passes in order when the largest
///    normal angle is within `max_normal_angle_deg`, and fails otherwise. Bad normals never hide behind rule 3, because
///    the vertices demonstrably correspond.
/// 3. Otherwise it passes unordered when Chamfer and Hausdorff are within their limits. Normals are not judged here:
///    without a correspondence they have nothing to be compared with.
///
/// Count mismatches are listed in [`MeshReport::findings`] but do not fail the comparison alone.
pub fn compare_meshes(actual: &MeshData, expected: &MeshData, tolerance: MeshTolerance) -> MeshReport {
    let vertices = Counts { actual: actual.positions.len(), expected: expected.positions.len() };
    let triangles = Counts { actual: actual.triangle_count(), expected: expected.triangle_count() };
    let mut report = MeshReport {
        tolerance,
        vertices,
        triangles,
        in_order: None,
        unordered: None,
        bbox_delta: None,
        verdict: Verdict::Failed,
        findings: invalid_findings(actual, expected),
    };
    if !report.findings.is_empty() {
        return report;
    }
    if vertices.actual != vertices.expected {
        report.findings.push(Finding::VertexCount { actual: vertices.actual, expected: vertices.expected });
    }
    if triangles.actual != triangles.expected {
        report.findings.push(Finding::TriangleCount { actual: triangles.actual, expected: triangles.expected });
    }
    report.in_order = (vertices.actual == vertices.expected).then(|| in_order_stats(actual, expected));
    let unordered = unordered_stats(&actual.positions, &expected.positions);
    report.bbox_delta = Some(bbox_delta(&actual.positions, &expected.positions));

    let (verdict, failures) = judge(&tolerance, report.in_order.as_ref(), &unordered);
    report.verdict = verdict;
    report.findings.extend(failures);
    report.unordered = Some(unordered);
    report
}

/// Applies the rule documented on [`compare_meshes`]: the verdict, and the violated limits when it is a failure.
fn judge(
    tolerance: &MeshTolerance,
    in_order: Option<&InOrderStats>,
    unordered: &UnorderedStats,
) -> (Verdict, Vec<Finding>) {
    // `is_nan` first, so a NaN measure fails instead of slipping past `>`.
    let over = |metric, value: f32, limit: f32| {
        (value.is_nan() || value > limit).then_some(Finding::Exceeded { metric, value, limit })
    };
    if let Some(s) = in_order.filter(|s| s.max_error <= tolerance.max_vertex_error) {
        return match over(Metric::NormalAngle, s.max_normal_angle_deg, tolerance.max_normal_angle_deg) {
            None => (Verdict::InOrder, Vec::new()),
            Some(finding) => (Verdict::Failed, vec![finding]),
        };
    }
    let unordered_over: Vec<Finding> = [
        over(Metric::Chamfer, unordered.chamfer, tolerance.max_chamfer),
        over(Metric::Hausdorff, unordered.hausdorff, tolerance.max_hausdorff),
    ]
    .into_iter()
    .flatten()
    .collect();
    if unordered_over.is_empty() {
        return (Verdict::Unordered, Vec::new());
    }
    // The in-order limit was missed too (that is why the unordered rule applied): list it first, it is the first thing
    // to look at when the counts match.
    let order = in_order.and_then(|s| over(Metric::VertexError, s.max_error, tolerance.max_vertex_error));
    (Verdict::Failed, order.into_iter().chain(unordered_over).collect())
}

/// The [`Finding::Invalid`] entries for both meshes.
fn invalid_findings(actual: &MeshData, expected: &MeshData) -> Vec<Finding> {
    [(Side::Actual, actual), (Side::Expected, expected)]
        .into_iter()
        .filter_map(|(side, mesh)| mesh.validate().err().map(|e| Finding::Invalid { side, reason: e.0 }))
        .collect()
}

fn in_order_stats(actual: &MeshData, expected: &MeshData) -> InOrderStats {
    let positions = actual.positions.iter().zip(&expected.positions).map(|(a, e)| a.distance(*e)).collect::<Vec<_>>();
    let angles = actual.normals.iter().zip(&expected.normals).map(|(a, e)| angle_deg(*a, *e)).collect::<Vec<_>>();
    InOrderStats {
        max_error: max_of(&positions),
        mean_error: mean_of(&positions),
        max_normal_angle_deg: max_of(&angles),
        worst_positions: worst(&positions),
        worst_normals: worst(&angles),
        position_errors: VertexErrors(positions),
        normal_angles_deg: VertexErrors(angles),
    }
}

/// The angle between two normals in degrees. `atan2(|a x b|, a . b)` rather than `acos(a . b)`: near parallel, the
/// dot product rounds to 1 and `acos` loses most of its digits (0.03 degrees of noise), which would eat the 1 degree
/// tolerance's headroom. A zero normal against a real one is a total mismatch (180), two zero normals agree.
fn angle_deg(a: Vec3, b: Vec3) -> f32 {
    let (a_zero, b_zero) = (a.length_squared() == 0.0, b.length_squared() == 0.0);
    if a_zero || b_zero {
        return if a_zero && b_zero { 0.0 } else { 180.0 };
    }
    a.cross(b).length().atan2(a.dot(b)).to_degrees()
}

fn unordered_stats(actual: &[Vec3], expected: &[Vec3]) -> UnorderedStats {
    let actual_to_expected = nearest_distances(actual, &Grid::new(expected));
    let expected_to_actual = nearest_distances(expected, &Grid::new(actual));
    UnorderedStats {
        chamfer: mean_of(&actual_to_expected).midpoint(mean_of(&expected_to_actual)),
        hausdorff: max_of(&actual_to_expected).max(max_of(&expected_to_actual)),
        actual_to_expected: VertexErrors(actual_to_expected),
        expected_to_actual: VertexErrors(expected_to_actual),
    }
}

fn nearest_distances(from: &[Vec3], to: &Grid) -> Vec<f32> {
    from.iter().map(|&p| to.nearest_distance(p)).collect()
}

fn bbox_delta(actual: &[Vec3], expected: &[Vec3]) -> BboxDelta {
    let ((a_min, a_max), (e_min, e_max)) = (bounds(actual), bounds(expected));
    BboxDelta { min: a_min - e_min, max: a_max - e_max }
}

fn bounds(points: &[Vec3]) -> (Vec3, Vec3) {
    points
        .iter()
        .fold((Vec3::splat(f32::INFINITY), Vec3::splat(f32::NEG_INFINITY)), |(lo, hi), &p| (lo.min(p), hi.max(p)))
}

fn max_of(values: &[f32]) -> f32 {
    values.iter().copied().fold(0.0, f32::max)
}

#[allow(clippy::cast_precision_loss)] // vertex counts stay far below 2^52; the sum is kept in f64 so 100k terms don't drift
fn mean_of(values: &[f32]) -> f32 {
    if values.is_empty() {
        return 0.0;
    }
    #[allow(clippy::cast_possible_truncation)] // the mean of f32 values fits f32
    let mean = (values.iter().map(|&v| f64::from(v)).sum::<f64>() / values.len() as f64) as f32;
    mean
}

/// The [`WORST_LIMIT`] largest non-zero values with their indices, largest first (ties by lower index).
fn worst(values: &[f32]) -> Vec<WorstVertex> {
    let mut all: Vec<WorstVertex> = values
        .iter()
        .enumerate()
        .filter(|&(_, &v)| v > 0.0)
        .map(|(index, &value)| WorstVertex { index, value })
        .collect();
    let by_value = |a: &WorstVertex, b: &WorstVertex| b.value.total_cmp(&a.value).then(a.index.cmp(&b.index));
    if all.len() > WORST_LIMIT {
        all.select_nth_unstable_by(WORST_LIMIT, by_value);
        all.truncate(WORST_LIMIT);
    }
    all.sort_unstable_by(by_value);
    all
}

/// Points binned into a uniform grid, so a nearest-vertex query looks at a few cells instead of every point. Built from
/// a non-empty set of finite points ([`compare_meshes`] validates both meshes first).
struct Grid {
    min: Vec3,
    cell: f32,
    dims: [usize; 3],
    /// `starts[c]..starts[c + 1]` is the range of `points` lying in cell `c`.
    starts: Vec<u32>,
    /// The points, grouped by cell, so a cell's points are contiguous in memory.
    points: Vec<Vec3>,
}

impl Grid {
    /// About four cells per point: surfaces fill only a thin shell of a bounding box, and this keeps a few points per
    /// occupied cell for them while still costing only a few bytes per point.
    const CELLS_PER_POINT: usize = 4;

    fn new(points: &[Vec3]) -> Self {
        let (min, max) = bounds(points);
        let extent = max - min;
        let longest = extent.max_element();
        // An axis thinner than this counts as this thick, so a flat mesh doesn't shrink the cells to nothing.
        let thin = longest * 1e-3;
        let volume = extent.max(Vec3::splat(thin)).element_product();
        #[allow(clippy::cast_precision_loss)] // see `mean_of`
        let target = (points.len() * Self::CELLS_PER_POINT).max(1) as f32;
        let mut cell = if longest > 0.0 { (volume / target).cbrt() } else { 1.0 };
        // A thin or lopsided box can still come out with far more cells than points; grow the cells until it doesn't.
        let max_cells = points.len().saturating_mul(2 * Self::CELLS_PER_POINT).max(1);
        let dims = loop {
            let dims = extent.to_array().map(|e| axis_cells(e, cell));
            if dims.iter().product::<usize>() <= max_cells {
                break dims;
            }
            cell *= 1.26; // the cube root of 2: halves the cell count
        };
        let slots: Vec<usize> = points.iter().map(|&p| flat_index(cell_coords(p, min, cell, dims), dims)).collect();
        let mut starts = vec![0_u32; dims.iter().product::<usize>() + 1];
        for &slot in &slots {
            starts[slot + 1] += 1;
        }
        for i in 1..starts.len() {
            starts[i] += starts[i - 1];
        }
        let mut next = starts.clone();
        let mut sorted = vec![Vec3::ZERO; points.len()];
        for (&p, &slot) in points.iter().zip(&slots) {
            sorted[next[slot] as usize] = p;
            next[slot] += 1;
        }
        Self { min, cell, dims, starts, points: sorted }
    }

    /// The distance from `q` to the nearest grid point, found by scanning square rings of cells outward from `q`'s cell
    /// until no unscanned cell can hold anything closer. `q` may lie outside the grid.
    fn nearest_distance(&self, q: Vec3) -> f32 {
        let centre = cell_coords(q, self.min, self.cell, self.dims);
        let mut best = f32::INFINITY; // squared
        for ring in 0..self.dims.into_iter().max().unwrap_or(1) {
            self.scan_ring(centre, ring, q, &mut best);
            // Cells of ring r + 1 and beyond are at least `ring` whole cells away from `q`, wherever `q` lies in its own
            // cell (or beyond the grid edge, which only adds distance).
            #[allow(clippy::cast_precision_loss)] // ring counts are tiny
            let reach = ring as f32 * self.cell;
            if best <= reach * reach {
                break;
            }
        }
        best.sqrt()
    }

    /// Lowers `best` to the squared distance of the nearest point in the cells at Chebyshev distance exactly `ring` from
    /// `centre`.
    fn scan_ring(&self, centre: [usize; 3], ring: usize, query: Vec3, best: &mut f32) {
        #[allow(clippy::cast_possible_wrap)] // a ring is at most the grid's longest side
        let span = ring as isize;
        let axis = |i: usize, d: isize| centre[i].checked_add_signed(d).filter(|&c| c < self.dims[i]);
        for dz in -span..=span {
            let Some(z) = axis(2, dz) else { continue };
            for dy in -span..=span {
                let Some(y) = axis(1, dy) else { continue };
                // On a face of the ring's cube every x is in the ring; elsewhere only its two ends are.
                let step = if dz.abs() == span || dy.abs() == span { 1 } else { 2 * ring };
                for dx in (-span..=span).step_by(step) {
                    let Some(x) = axis(0, dx) else { continue };
                    let slot = flat_index([x, y, z], self.dims);
                    for p in &self.points[self.starts[slot] as usize..self.starts[slot + 1] as usize] {
                        *best = best.min(p.distance_squared(query));
                    }
                }
            }
        }
    }
}

/// Cells needed along an axis of length `extent`.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a non-negative, finite ratio far below usize::MAX
fn axis_cells(extent: f32, cell: f32) -> usize {
    (extent / cell) as usize + 1
}

/// The cell holding `p`, clamped into the grid (so a point outside it maps to the nearest edge cell).
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // `as usize` saturates: negatives and NaN give 0
fn cell_coords(p: Vec3, min: Vec3, cell: f32, dims: [usize; 3]) -> [usize; 3] {
    let rel = ((p - min) / cell).to_array();
    [0, 1, 2].map(|i| (rel[i] as usize).min(dims[i] - 1))
}

fn flat_index([x, y, z]: [usize; 3], dims: [usize; 3]) -> usize {
    (z * dims[1] + y) * dims[0] + x
}

/// Writes both meshes as one OBJ file (objects `actual` and `expected`), every vertex coloured by its error, for
/// looking at in Blender or `MeshLab` (OBJ's `v x y z r g b` extension).
///
/// Blue is no error, green is the tolerance, red is twice the tolerance or more. A comparison that passed in order
/// colours by per-vertex error (against `max_vertex_error`); any other colours by nearest-vertex distance (against
/// `max_hausdorff`), which needs no vertex correspondence, so a reordered but identical mesh is not a sea of red.
/// Meant for PR evidence: write it to a local assets folder, never into git.
///
/// # Errors
/// [`io::ErrorKind::InvalidInput`] when `report` has no measures (a mesh was invalid), else any error from `out`.
pub fn write_error_obj(
    out: &mut impl Write,
    actual: &MeshData,
    expected: &MeshData,
    report: &MeshReport,
) -> io::Result<()> {
    let no_data = || io::Error::new(io::ErrorKind::InvalidInput, "the report has no measures to colour by");
    let Some(unordered) = &report.unordered else { return Err(no_data()) };
    let tolerance = &report.tolerance;
    let (a_errors, e_errors, limit) = match &report.in_order {
        Some(s) if report.verdict == Verdict::InOrder => {
            (&s.position_errors, &s.position_errors, tolerance.max_vertex_error)
        }
        _ => (&unordered.actual_to_expected, &unordered.expected_to_actual, tolerance.max_hausdorff),
    };
    // With a zero limit (an exact comparison) the worst error stands in, so the picture still shows where it is.
    let scale = if limit > 0.0 { 2.0 * limit } else { max_of(a_errors).max(max_of(e_errors)).max(f32::MIN_POSITIVE) };
    writeln!(
        out,
        "# omuzaki-test mesh error: blue = none, green = at the tolerance, red = twice the tolerance or more"
    )?;
    let mut offset = 1; // OBJ indices start at 1 and run across objects
    for (name, mesh, errors) in [("actual", actual, a_errors), ("expected", expected, e_errors)] {
        writeln!(out, "o {name}")?;
        for (p, &e) in mesh.positions.iter().zip(errors.iter()) {
            let [r, g, b] = ramp(e / scale);
            writeln!(out, "v {} {} {} {r:.3} {g:.3} {b:.3}", p.x, p.y, p.z)?;
        }
        for &[a, b, c] in mesh.indices.as_chunks::<3>().0 {
            writeln!(out, "f {} {} {}", a as usize + offset, b as usize + offset, c as usize + offset)?;
        }
        offset += mesh.positions.len();
    }
    Ok(())
}

/// Blue at 0, green at 0.5, red from 1 up.
fn ramp(t: f32) -> [f32; 3] {
    let t = t.clamp(0.0, 1.0);
    if t < 0.5 { [0.0, 2.0 * t, 1.0 - 2.0 * t] } else { [2.0 * t - 1.0, 2.0 - 2.0 * t, 0.0] }
}
