//! The traced head's radius map: its byte format and the bilinear sampling of [`TraceSample`], a port of the prototype's
//! `tracedRadius` (`head.js`).
//!
//! The asset is two grids of `theta_steps * phi_steps` little-endian `f32`s, row-major: every radius first, then every
//! weight (decision G12). Rows run from the top pole down and columns round from the front, at cell centres. The
//! sampling is the prototype's, down to the half-cell offsets and the wrap: theta clamps at its first and last rows,
//! phi wraps. The blend into the analytic cranium lives in [`super::super::cranium`], not here.

use std::f64::consts::{PI, TAU};
use std::fmt::{self, Debug};

use omuzaki_core::glam::Vec3;

use crate::head::cranium::TraceSample;

/// Bytes of one `f32`.
const BYTES_PER_VALUE: usize = size_of::<f32>();

/// A traced radius map: a radius and a confidence weight per cell of a (theta, phi) grid.
#[derive(Clone, PartialEq)]
pub struct HeadTrace {
    theta_steps: u16,
    phi_steps: u16,
    /// `theta_steps * phi_steps` radii in metres, row-major.
    radii: Vec<f32>,
    /// `theta_steps * phi_steps` weights, expected in `0..=1`, row-major, in the same layout as `radii`.
    weights: Vec<f32>,
}

impl HeadTrace {
    /// Parses a map of `theta_steps` rows by `phi_steps` columns from its bytes: the radii, then the weights, each a
    /// little-endian `f32`.
    ///
    /// # Errors
    /// [`HeadTraceError::Empty`] for a grid with no rows or no columns, [`HeadTraceError::TooLarge`] when the byte
    /// length cannot be represented, [`HeadTraceError::Length`] when `bytes` is not exactly two grids long,
    /// [`HeadTraceError::NotFinite`] for a radius or weight that is NaN or infinite,
    /// [`HeadTraceError::NegativeRadius`] for a radius below zero, and [`HeadTraceError::WeightOutOfRange`] for a weight
    /// outside `0..=1`.
    pub fn from_bytes(theta_steps: u16, phi_steps: u16, bytes: &[u8]) -> Result<Self, HeadTraceError> {
        if theta_steps == 0 || phi_steps == 0 {
            return Err(HeadTraceError::Empty { theta_steps, phi_steps });
        }
        let cells = usize::from(theta_steps) * usize::from(phi_steps);
        let expected =
            cells.checked_mul(2 * BYTES_PER_VALUE).ok_or(HeadTraceError::TooLarge { theta_steps, phi_steps })?;
        if bytes.len() != expected {
            return Err(HeadTraceError::Length { expected, actual: bytes.len() });
        }
        let (radius_bytes, weight_bytes) = bytes.split_at(cells * BYTES_PER_VALUE);
        let radii = decode(radius_bytes);
        let weights = decode(weight_bytes);
        let columns = usize::from(phi_steps);
        check_finite("radius", &radii, columns)?;
        check_radii(&radii, columns)?;
        check_finite("weight", &weights, columns)?;
        check_weights(&weights, columns)?;
        Ok(Self { theta_steps, phi_steps, radii, weights })
    }

    /// Rows of theta, from the top pole.
    pub fn theta_steps(&self) -> u16 {
        self.theta_steps
    }

    /// Columns of phi, from the front.
    pub fn phi_steps(&self) -> u16 {
        self.phi_steps
    }
}

/// Reads a grid of little-endian `f32`s. `bytes` is a whole number of them, so every chunk is four bytes.
fn decode(bytes: &[u8]) -> Vec<f32> {
    let (values, _) = bytes.as_chunks::<BYTES_PER_VALUE>();
    values.iter().map(|v| f32::from_le_bytes(*v)).collect()
}

/// The first value of `grid` that is not finite, named by its row and column.
fn check_finite(grid: &'static str, values: &[f32], columns: usize) -> Result<(), HeadTraceError> {
    match values.iter().position(|v| !v.is_finite()) {
        None => Ok(()),
        Some(i) => Err(HeadTraceError::NotFinite { grid, row: i / columns, column: i % columns }),
    }
}

/// The first radius below zero, named by its row and column. A radius is a distance from the head centre, so a negative
/// one with a positive weight would put the vertex on the far side of the centre. Runs after [`check_finite`].
fn check_radii(radii: &[f32], columns: usize) -> Result<(), HeadTraceError> {
    match radii.iter().enumerate().find(|(_, r)| **r < 0.0) {
        None => Ok(()),
        Some((i, &radius)) => Err(HeadTraceError::NegativeRadius { row: i / columns, column: i % columns, radius }),
    }
}

/// The first weight outside `0..=1`, named by its row and column. The blend in the cranium assumes that range, so a
/// weight outside it would push the head past the traced radius rather than toward it. Runs after [`check_finite`], so
/// every weight compared here is a number.
fn check_weights(weights: &[f32], columns: usize) -> Result<(), HeadTraceError> {
    match weights.iter().enumerate().find(|(_, w)| !(0.0..=1.0).contains(*w)) {
        None => Ok(()),
        Some((i, &weight)) => Err(HeadTraceError::WeightOutOfRange { row: i / columns, column: i % columns, weight }),
    }
}

impl TraceSample for HeadTrace {
    /// The prototype's `tracedRadius`: `dir` is a unit direction from the head centre. The sampling does not normalise
    /// it. A NaN component gives NaN; it does not panic.
    // The trait speaks f32; the radius is a fraction of a metre. The casts to an index follow a clamp or a wrap, so
    // they are never negative.
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    fn sample(&self, dir: Vec3) -> (f32, f32) {
        let d = dir.as_dvec3();
        let nt = f64::from(self.theta_steps);
        let np = f64::from(self.phi_steps);
        let th = d.y.clamp(-1.0, 1.0).acos();
        let ph = ((d.x.atan2(d.z) % TAU) + TAU) % TAU;
        // Cell centres sit at half-cell offsets, so the lookup is taken at `f - 0.5`.
        let ft = th / PI * nt - 0.5;
        let fp = ph / TAU * np - 0.5;
        // Theta clamps to its first and last rows (the poles do not wrap). The index is clamped before the cast.
        let rows = usize::from(self.theta_steps);
        let columns = usize::from(self.phi_steps);
        let t_floor = ft.floor().clamp(0.0, nt - 1.0);
        let t0 = t_floor as usize;
        let t1 = (t0 + 1).min(rows - 1);
        let at = (ft - t_floor).clamp(0.0, 1.0);
        // Phi wraps: the integer floor is taken before the modulo, so a negative floor wraps to the last column.
        let phi_floor = fp.floor() as i64;
        let np_i = i64::from(self.phi_steps);
        let p0 = (((phi_floor % np_i) + np_i) % np_i) as usize;
        let p1 = (p0 + 1) % columns;
        let ap = fp - fp.floor();
        let bl = |grid: &[f32]| {
            let g = |t: usize, p: usize| f64::from(grid[t * columns + p]);
            (g(t0, p0) * (1.0 - ap) + g(t0, p1) * ap) * (1.0 - at) + (g(t1, p0) * (1.0 - ap) + g(t1, p1) * ap) * at
        };
        (bl(&self.radii) as f32, bl(&self.weights) as f32)
    }
}

impl Debug for HeadTrace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HeadTrace")
            .field("theta_steps", &self.theta_steps)
            .field("phi_steps", &self.phi_steps)
            .finish_non_exhaustive()
    }
}

/// Why a traced radius map could not be read.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum HeadTraceError {
    /// The grid has no rows or no columns.
    #[error("a radius map needs at least one theta row and one phi column, got {theta_steps} x {phi_steps}")]
    Empty {
        /// Rows of theta.
        theta_steps: u16,
        /// Columns of phi.
        phi_steps: u16,
    },
    /// The grid is too large for its byte length to be represented on this platform.
    #[error("a radius map of {theta_steps} x {phi_steps} is too large to index on this platform")]
    TooLarge {
        /// Rows of theta.
        theta_steps: u16,
        /// Columns of phi.
        phi_steps: u16,
    },
    /// The bytes are not the two grids the size asks for.
    #[error("a radius map needs {expected} bytes, got {actual}")]
    Length {
        /// The byte length the grid size asks for.
        expected: usize,
        /// The byte length given.
        actual: usize,
    },
    /// A radius or weight is NaN or infinite.
    #[error("the {grid} at row {row}, column {column} of the radius map is not finite")]
    NotFinite {
        /// Which grid: `radius` or `weight`.
        grid: &'static str,
        /// Row of theta, from the top pole.
        row: usize,
        /// Column of phi, from the front.
        column: usize,
    },
    /// A radius is below zero, which would place the surface on the far side of the head centre.
    #[error("the radius at row {row}, column {column} of the radius map is {radius}, below zero")]
    NegativeRadius {
        /// Row of theta, from the top pole.
        row: usize,
        /// Column of phi, from the front.
        column: usize,
        /// The radius given, in metres.
        radius: f32,
    },
    /// A weight is outside `0..=1`, the range the blend in the cranium assumes.
    #[error("the weight at row {row}, column {column} of the radius map is {weight}, outside 0 to 1")]
    WeightOutOfRange {
        /// Row of theta, from the top pole.
        row: usize,
        /// Column of phi, from the front.
        column: usize,
        /// The weight given.
        weight: f32,
    },
}
