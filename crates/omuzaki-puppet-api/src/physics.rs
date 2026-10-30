//! Pendulums: secondary motion (hair, ears, ribbons) that writes a parameter from how its anchor moves.
//!
//! Each pendulum hangs from a node; when the node moves (the head turns, the body sways) the bob lags and swings,
//! and its angle and length become the parameter's value. The equations and their closed-form checks are in
//! `docs/design/puppet2d.md` ("Physics").

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::model::NodeIx;
use crate::param::ParamIx;

/// Gravity and scale for every pendulum of a puppet.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PhysicsWorld {
    /// Model units per metre.
    pub pixels_per_meter: f32,
    /// Gravity, metres per second squared.
    pub gravity: f32,
}

impl Default for PhysicsWorld {
    fn default() -> Self {
        Self { pixels_per_meter: 1000.0, gravity: 9.8 }
    }
}

/// The pendulum's model.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendulumModel {
    /// A rigid rod: only the angle moves.
    #[default]
    Rigid,
    /// A spring: the angle and the length move.
    Spring,
}

/// How the bob's position becomes the parameter's two values.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MapMode {
    /// x is the angle off the vertical, y the stretch of the length.
    #[default]
    AngleLength,
    /// x is the stretch, y the angle.
    LengthAngle,
    /// x and y are the bob's offset from rest, in lengths.
    Xy,
    /// y and x are the bob's offset from rest, in lengths.
    Yx,
}

/// One pendulum.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Pendulum {
    /// What it is called.
    #[serde(default)]
    pub name: String,
    /// The node it hangs from.
    pub anchor: NodeIx,
    /// The parameter it writes.
    pub param: ParamIx,
    /// Rigid or spring.
    #[serde(default)]
    pub model: PendulumModel,
    /// How its state maps to the parameter.
    #[serde(default)]
    pub map: MapMode,
    /// A factor on the world's gravity.
    #[serde(default = "one")]
    pub gravity: f32,
    /// The rod's length, model units.
    pub length: f32,
    /// The spring's natural frequency, hertz (spring model).
    #[serde(default = "one")]
    pub frequency: f32,
    /// Angular damping ratio.
    #[serde(default = "half")]
    pub angle_damping: f32,
    /// Length damping ratio (spring model).
    #[serde(default = "half")]
    pub length_damping: f32,
    /// A factor on each output axis (the parameter's units per unit of the mapped value).
    #[serde(default = "ones")]
    pub output_scale: [f32; 2],
    /// Whether it follows the anchor's own transform only, not its ancestors'.
    #[serde(default)]
    pub local_only: bool,
}

const fn one() -> f32 {
    1.0
}
const fn half() -> f32 {
    0.5
}
const fn ones() -> [f32; 2] {
    [1.0, 1.0]
}
