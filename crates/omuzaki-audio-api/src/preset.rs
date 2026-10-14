//! Effect presets as data (design doc section 6.2): a name and an ordered chain of nodes with their parameters.
//!
//! A character's presets live in `characters/<id>/voice.toml`, so a character's voice travels with it. Applying one is a
//! parameter change when the chain's shape matches the bus's, and a structural change otherwise.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::graph::NodeSpec;

/// A named voice-effect chain.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FxPreset {
    /// The name to show ("Radio", "Robot", "Telephone").
    pub name: String,
    /// The chain, in order. Node ids are unique within it.
    #[serde(default)]
    pub chain: Vec<NodeSpec>,
}

impl FxPreset {
    /// Whether applying this preset to `current` only changes parameters: the same nodes (id, kind and config), in the
    /// same order. Anything else is a structural change, which swaps the graph and may move the A/V delay.
    #[must_use]
    pub fn keeps_shape_of(&self, current: &[NodeSpec]) -> bool {
        self.chain.len() == current.len()
            && self.chain.iter().zip(current).all(|(a, b)| a.id == b.id && a.kind == b.kind && a.config == b.config)
    }
}
