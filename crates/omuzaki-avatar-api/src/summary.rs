//! [`AvatarSummary`]: what `io.inspect {as: avatar}` reports about a prepared avatar (design D6).

use std::time::Duration;

use omuzaki_anim_api::ChannelMask;
use omuzaki_char_api::CharId;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A stage of "file drop to performing", the five rows of D6's machine-time budget.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Stage {
    /// Sniff, read the file, `run_import` (budget 1.5 s).
    Read,
    /// The humanoid map, T rest, cleanup, `ARKit` synthesis and the driver table (budget 1.0 s).
    Prepare,
    /// Decode the textures (budget 3.0 s).
    Decode,
    /// Hash and store the original and its images (budget 1.0 s).
    Store,
    /// Upload to the renderer, up to the first performing frame (budget 1.5 s).
    Upload,
}

/// A prepared avatar at a glance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct AvatarSummary {
    /// The character id the import suggests, from the file name or the meta.
    pub id_suggestion: CharId,
    /// The channels that reach the model ([`ImportedAvatar::driven`](crate::avatar::ImportedAvatar::driven)).
    pub driven: ChannelMask,
    /// How long each stage took, in the order they ran.
    pub timings: Vec<(Stage, Duration)>,
    /// GPU bytes of every texture ([`ImportedAvatar::texture_bytes`](crate::avatar::ImportedAvatar::texture_bytes)):
    /// reported so a person can see it, never acted on by downgrading a texture.
    pub texture_bytes: u64,
}

impl AvatarSummary {
    /// The summary of an avatar suggested as `id_suggestion`, driving `driven`, with `texture_bytes` of textures and
    /// no timings yet.
    pub fn new(id_suggestion: CharId, driven: ChannelMask, texture_bytes: u64) -> Self {
        Self { id_suggestion, driven, timings: Vec::new(), texture_bytes }
    }

    /// Records how long `stage` took (builder style).
    #[must_use]
    pub fn with_timing(mut self, stage: Stage, took: Duration) -> Self {
        self.timings.push((stage, took));
        self
    }

    /// The total machine time over every recorded stage.
    pub fn total(&self) -> Duration {
        self.timings.iter().map(|(_, took)| *took).sum()
    }
}
