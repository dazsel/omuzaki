//! The geometry kit: curves, grids, lofts, sweeps and skinning helpers shared by every part.
//!
//! Ported from the earlier web prototype's `uniform-geo.js`. Geometry is
//! built in `f64`, as the prototype does, and rounded to `f32` once, when it enters a [`grid::MeshBuilder`]; the
//! builder produces the `omuzaki_core::mesh::MeshData` every consumer speaks, with skin weights keyed by `JointRef`
//! instead of a skeleton's bone order.
//!
//! | Module | Holds |
//! |---|---|
//! | [`curve`] | scalar helpers and the monotone cubic (PCHIP) curve |
//! | [`grid`] | the point [`grid::Grid`], smooth grid normals and offsets, the [`grid::MeshBuilder`] |
//! | [`loft`] | grids from a Y-axis loft or a lathe around any axis |
//! | [`js_names`] | the prototype's bone names as `JointRef`s, for parity tests |
//! | [`catmull`] | Catmull-Rom arc-length points, as three.js computes them |
//! | [`resample`] | polyline resampling by arc length |
//! | [`sweep`] | frames and elliptical sweeps along a polyline, tubes, the placement matrix |
//! | [`skin`] | skin weights: capsule weighting from a body's fit, the top-four rule, brushes, smoothing and mirroring |

use omuzaki_core::mesh::MeshError;

pub mod catmull;
pub mod curve;
pub mod grid;
pub mod js_names;
pub mod loft;
pub mod resample;
pub mod skin;
pub mod sweep;

/// Why a kit function could not build its geometry: an input that would otherwise panic or produce a broken mesh.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum KitError {
    /// Fewer items than the operation needs (a one-row grid, an empty knot list).
    #[error("{what}: need at least {need}, got {got}")]
    TooFew {
        /// What was too short.
        what: &'static str,
        /// The least the operation accepts.
        need: usize,
        /// What it was given.
        got: usize,
    },
    /// Grid rows of different lengths.
    #[error("grid row {row} has {got} points, expected {expected}")]
    Ragged {
        /// The first row whose length differs from row 0.
        row: usize,
        /// Row 0's length.
        expected: usize,
        /// This row's length.
        got: usize,
    },
    /// A NaN or infinite input.
    #[error("{what} must be finite")]
    NonFinite {
        /// Which input.
        what: &'static str,
    },
    /// Two curve knots at the same x, which leaves the slope between them undefined.
    #[error("two knots share x = {x}")]
    DuplicateKnot {
        /// The repeated x.
        x: f64,
    },
    /// A count that does not fit in `usize`: a grid's points (rows times columns) or a segment count plus one.
    #[error("{what} does not fit in a usize")]
    Overflow {
        /// The count that overflowed.
        what: &'static str,
    },
    /// An input that has no direction or size (a zero-length axis).
    #[error("{what} is degenerate")]
    Degenerate {
        /// Which input.
        what: &'static str,
    },
    /// More vertices or joints than the mesh format can index.
    #[error("a mesh can hold at most {max} {what}")]
    TooMany {
        /// What overflowed.
        what: &'static str,
        /// The limit.
        max: usize,
    },
    /// The finished mesh breaks a mesh or part-mesh rule.
    #[error(transparent)]
    Mesh(#[from] MeshError),
}
