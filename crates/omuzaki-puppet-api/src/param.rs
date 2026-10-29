//! Parameters and their keyform bindings.
//!
//! A parameter has one or two axes, each with sorted key positions. A [`Binding`] gives one value (a number, or a set
//! of vertex offsets) at every key point of the grid, and the evaluator interpolates between the four key points
//! around the parameter's value. Bindings from several parameters on one node combine: offsets, translations,
//! rotations and draw-order shifts add; scales, opacities and tints multiply.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::NodeIx;

/// A parameter's position in [`crate::PuppetDef::params`].
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct ParamIx(pub u32);

impl ParamIx {
    /// The index as a `usize`.
    #[must_use]
    pub const fn get(self) -> usize {
        self.0 as usize
    }
}

/// A parameter: one or two numbers that move the puppet.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Param {
    /// Its name (`"ParamAngleX"`, `"Head:: Yaw-Pitch"`).
    pub name: String,
    /// Whether it has a second (y) axis.
    #[serde(default)]
    pub two_axes: bool,
    /// The lowest value per axis.
    pub min: [f32; 2],
    /// The highest value per axis.
    pub max: [f32; 2],
    /// The value at rest.
    #[serde(default)]
    pub default: [f32; 2],
    /// The key positions per axis, ascending, in the parameter's units (one `[min]`-style entry for a missing axis).
    pub keys: [Vec<f32>; 2],
    /// What it moves.
    #[serde(default)]
    pub bindings: Vec<Binding>,
}

impl Param {
    /// How many key points the grid has (`keys[0].len() * keys[1].len()`).
    #[must_use]
    pub fn key_points(&self) -> usize {
        self.keys[0].len() * self.keys[1].len()
    }

    /// The index of key point `(ix, iy)` in a binding's values: x-major, `ix * keys[1].len() + iy`.
    #[must_use]
    pub fn key_index(&self, ix: usize, iy: usize) -> usize {
        ix * self.keys[1].len() + iy
    }
}

/// What a binding moves on its node.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BindTarget {
    /// Vertex (or lattice point) offsets, model units in the node's space.
    Deform,
    /// Translation x.
    TranslateX,
    /// Translation y.
    TranslateY,
    /// Rotation in the plane, radians.
    Rotate,
    /// A turn about the vertical axis, radians (flips the node through its x scale, as `Inochi2D` does).
    RotateY,
    /// A turn about the horizontal axis, radians (flips through the y scale).
    RotateX,
    /// Scale x, a factor.
    ScaleX,
    /// Scale y, a factor.
    ScaleY,
    /// A draw-order shift.
    ZSort,
    /// Opacity, a factor.
    Opacity,
    /// The tint's red, a factor.
    TintR,
    /// The tint's green, a factor.
    TintG,
    /// The tint's blue, a factor.
    TintB,
}

impl BindTarget {
    /// Whether this target's values from several parameters multiply (true) or add (false).
    #[must_use]
    pub const fn multiplies(self) -> bool {
        matches!(self, Self::ScaleX | Self::ScaleY | Self::Opacity | Self::TintR | Self::TintG | Self::TintB)
    }

    /// The value that changes nothing: 1 for a factor, 0 for an offset.
    #[must_use]
    pub const fn identity(self) -> f32 {
        if self.multiplies() { 1.0 } else { 0.0 }
    }
}

/// How values between key points are found.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Interp {
    /// The nearest key point's value (swaps: an eye shape on or off).
    Nearest,
    /// Straight lines (bilinear on a 2D grid).
    #[default]
    Linear,
    /// Smooth through the key points (Catmull-Rom per axis), for motion without corners at the keys.
    Cubic,
}

/// The values of a binding at each key point, x-major (see [`Param::key_index`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", content = "values", rename_all = "snake_case")]
pub enum BindValues {
    /// One number per key point.
    Scalar(Vec<f32>),
    /// One set of offsets per key point, one offset per deformable point of the node.
    Offsets(Vec<Vec<[f32; 2]>>),
}

impl BindValues {
    /// How many key points it has values for.
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::Scalar(v) => v.len(),
            Self::Offsets(v) => v.len(),
        }
    }

    /// Whether it has no values.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One thing a parameter moves.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Binding {
    /// The node it moves.
    pub node: NodeIx,
    /// What on that node.
    pub target: BindTarget,
    /// How between key points.
    #[serde(default)]
    pub interp: Interp,
    /// Which key points carry an authored value (x-major); the evaluator fills the others from their neighbours.
    /// Empty means all are set.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub set: Vec<bool>,
    /// The values.
    pub values: BindValues,
}
