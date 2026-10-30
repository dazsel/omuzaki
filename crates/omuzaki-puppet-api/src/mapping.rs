//! How the shared channel bus (the `ARKit` 52, head and body, the anime extras) drives a puppet's parameters.
//!
//! A mapping is data stored with the puppet and edited like any other value: each entry reads a channel (or the
//! difference of two, for a smile against a frown), maps its input range linearly onto an output range, clamps, and
//! writes one axis of one parameter. Several entries on one axis add. A parameter no entry names keeps its default
//! (or what physics writes); a channel the puppet doesn't use is simply unread.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What an entry reads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelSource {
    /// The channel's name in the channel table (`"eyeBlinkLeft"`, `"headYaw"`, `"eyesSqueeze"`).
    pub channel: String,
    /// A channel subtracted from it (`"mouthFrownLeft"` under `"mouthSmileLeft"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minus: Option<String>,
}

/// One channel onto one parameter axis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MappingEntry {
    /// The parameter's name.
    pub param: String,
    /// Which axis: 0 (x) or 1 (y).
    #[serde(default)]
    pub axis: u8,
    /// What it reads.
    pub source: ChannelSource,
    /// The input range (channel units: weights `0..1`, angles in radians).
    pub input: [f32; 2],
    /// The parameter values the input range's ends map to (reversed to invert).
    pub output: [f32; 2],
}

impl MappingEntry {
    /// The parameter value for channel value `v`: linear from `input` to `output`, clamped to the input range.
    #[must_use]
    pub fn map(&self, v: f32) -> f32 {
        let [a, b] = self.input;
        let span = b - a;
        let t = if span.abs() <= f32::EPSILON { 0.0 } else { ((v - a) / span).clamp(0.0, 1.0) };
        self.output[0] + (self.output[1] - self.output[0]) * t
    }
}

/// A puppet's channel mapping.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ParamMapping {
    /// The entries, applied in order.
    pub entries: Vec<MappingEntry>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(input: [f32; 2], output: [f32; 2]) -> MappingEntry {
        MappingEntry {
            param: "p".into(),
            axis: 0,
            source: ChannelSource { channel: "jawOpen".into(), minus: None },
            input,
            output,
        }
    }

    #[test]
    fn an_entry_maps_linearly_and_clamps() {
        let e = entry([0.0, 1.0], [0.0, 30.0]);
        assert!((e.map(0.5) - 15.0).abs() < 1e-6);
        assert!((e.map(2.0) - 30.0).abs() < 1e-6);
        assert!((e.map(-1.0)).abs() < 1e-6);
        let inverted = entry([0.0, 1.0], [1.0, 0.0]);
        assert!((inverted.map(0.25) - 0.75).abs() < 1e-6);
        assert!(
            (entry([1.0, 1.0], [2.0, 5.0]).map(9.0) - 2.0).abs() < 1e-6,
            "an empty input range gives the first output"
        );
    }
}
