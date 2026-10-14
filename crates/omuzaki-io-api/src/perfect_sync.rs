//! Perfect-sync export: carrying the full `ARKit` face vocabulary into a VRM, so face trackers drive an exported model
//! as richly as they drive the live one (spec §19 FE-09, §27 EX-01).
//!
//! **What "perfect sync" is.** Face trackers (iPhone `ARKit`, `MediaPipe`, `VSeeFace`, Warudo, `VTube` Studio) emit 52 named
//! blendshape weights. A model is "perfect sync" when it defines an expression for each name, so the tracker's values
//! drive it directly instead of being squeezed through the few VRM presets. In VRM 1.0 each is a **custom
//! expression** named exactly like the `ARKit` key (`jawOpen`, `eyeBlinkLeft`): they can't collide with presets, which
//! all have different names. Warudo's model guide states the names are case-sensitive and uses the camelCase forms
//! below; in VRM 0.x the same strings are used as `BlendShapeClip` names with `presetName: Unknown`.
//!
//! **Who does what.** The exporter only *writes* expressions that are already in the scene. Creating them, from a
//! procedurally painted face, is **baking**, which needs the face painter and the character, so it happens before
//! export behind the [`ExpressionBaker`] trait implemented on the face side. The split keeps this crate free of
//! rendering and lets the exporter be tested with a baker that makes trivial morph targets.
//!
//! **How a painted channel becomes binds.** A channel may be realised as morph targets (jaw, cheeks, mouth shapes
//! on geometry), as texture-transform binds into a face atlas (painted eyes, brows), as colour binds (blush), or a
//! mix. Binds interpolate linearly with the weight, so a stepped flipbook must also set `is_binary`. The baker picks;
//! [`BakeReport`] records what it picked and what it skipped, for the user and for agents.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{ExpressionName, IoError, IoScene, NodeIx, Progress, Report};

/// The 52 `ARKit` blendshape names, as Apple spells them (`ARFaceAnchor.BlendShapeLocation` raw values). Order: eyes
/// (left then right), jaw, mouth, brows, cheeks, nose, tongue.
pub const ARKIT_52: [&str; 52] = [
    "eyeBlinkLeft",
    "eyeLookDownLeft",
    "eyeLookInLeft",
    "eyeLookOutLeft",
    "eyeLookUpLeft",
    "eyeSquintLeft",
    "eyeWideLeft",
    "eyeBlinkRight",
    "eyeLookDownRight",
    "eyeLookInRight",
    "eyeLookOutRight",
    "eyeLookUpRight",
    "eyeSquintRight",
    "eyeWideRight",
    "jawForward",
    "jawLeft",
    "jawRight",
    "jawOpen",
    "mouthClose",
    "mouthFunnel",
    "mouthPucker",
    "mouthLeft",
    "mouthRight",
    "mouthSmileLeft",
    "mouthSmileRight",
    "mouthFrownLeft",
    "mouthFrownRight",
    "mouthDimpleLeft",
    "mouthDimpleRight",
    "mouthStretchLeft",
    "mouthStretchRight",
    "mouthRollLower",
    "mouthRollUpper",
    "mouthShrugLower",
    "mouthShrugUpper",
    "mouthPressLeft",
    "mouthPressRight",
    "mouthLowerDownLeft",
    "mouthLowerDownRight",
    "mouthUpperUpLeft",
    "mouthUpperUpRight",
    "browDownLeft",
    "browDownRight",
    "browInnerUp",
    "browOuterUpLeft",
    "browOuterUpRight",
    "cheekPuff",
    "cheekSquintLeft",
    "cheekSquintRight",
    "noseSneerLeft",
    "noseSneerRight",
    "tongueOut",
];

/// Prefix of the custom expressions that carry Omuzaki's anime extras (`^^` eyes, heart eyes, sweat drop, gloom…,
/// spec FE-02). The prefix keeps them from ever colliding with a tracker's names, and lets other apps ignore them.
pub const EXTRA_PREFIX: &str = "omuzaki_";

/// The expression name for the face extra `id` (a lowercase slug such as `heart-eyes`): `omuzaki_heart-eyes`.
///
/// # Errors
/// [`CustomNameError`](crate::CustomNameError) if `id` is empty (the prefixed name can never be a preset).
pub fn extra_expression(id: &str) -> Result<ExpressionName, crate::CustomNameError> {
    ExpressionName::parse(&format!("{EXTRA_PREFIX}{id}"))
}

/// How many face channels an export carries. **The default is [`PerfectSync::Arkit52`]** (decided on review): an
/// avatar exported from Omuzaki drives from any face tracker out of the box. The op layer bakes the channels before
/// exporting; extras are opt-in because they are Omuzaki-specific.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PerfectSync {
    /// Only the VRM presets (`happy`, `aa`, `blink`…): small and universally supported.
    Off,
    /// Presets plus all 52 `ARKit` channels.
    #[default]
    Arkit52,
    /// The 52 `ARKit` channels plus Omuzaki's extras ([`EXTRA_PREFIX`]).
    Arkit52WithExtras,
}

impl PerfectSync {
    /// True if the 52 `ARKit` channels are required.
    pub fn wants_arkit(self) -> bool {
        self != Self::Off
    }
}

/// Limits a baker must respect. `None` means "no limit"; target profiles (`VRChat` performance ranks, Quest) set them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BakeBudget {
    /// Most morph targets to add to the face mesh.
    pub max_morph_targets: Option<u32>,
    /// Longest side, in pixels, of any face atlas it creates.
    pub max_atlas_px: Option<u32>,
}

/// What to bake.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct BakeRequest {
    /// The channels to realise, in the order the report lists them. For [`PerfectSync::Arkit52`] these are the 52
    /// `ARKit` names.
    pub channels: Vec<ExpressionName>,
    /// The node whose mesh receives the morph targets (the head). It must already exist in the scene.
    pub target: NodeIx,
    /// Limits.
    pub budget: BakeBudget,
}

impl BakeRequest {
    /// A request for `channels` on the mesh at `target`, with no limits.
    pub fn new(channels: Vec<ExpressionName>, target: NodeIx) -> Self {
        Self { channels, target, budget: BakeBudget::default() }
    }
}

/// How one channel ended up in the file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Realisation {
    /// Morph targets on the face mesh.
    Morph {
        /// How many targets it drives.
        targets: u32,
    },
    /// Texture-transform binds into a face atlas.
    Flipbook {
        /// Frames in the strip.
        frames: u32,
    },
    /// Material colour binds only (blush).
    Colour,
    /// More than one of the above.
    Mixed,
    /// Defined but doing nothing, with the reason (no tongue geometry, no such feature on this face). The name still
    /// exists so trackers don't report a missing channel.
    Inert {
        /// Why.
        reason: String,
    },
}

/// One channel's outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ChannelBake {
    /// The expression name.
    pub channel: String,
    /// What was made.
    pub realisation: Realisation,
}

/// What a bake produced, for the op output and the Build Theatre.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BakeReport {
    /// Per-channel outcomes, in request order.
    pub channels: Vec<ChannelBake>,
    /// Anything worth saying (budget trimmed a channel, a texture was resampled).
    pub findings: Report,
}

/// Turns the live character's face channels into expressions in a scene. Implemented on the face side
/// (`omuzaki-face`/`omuzaki-char` through the op layer); the mock in `omuzaki-io-mock` bakes trivial morph targets.
///
/// An implementation **adds** morph targets to the mesh at `request.target` (padding other primitives of that mesh
/// with zero deltas, as glTF requires), may add images, textures and materials for atlases, and inserts one
/// [`Expression`](crate::Expression) per requested channel into `scene.avatar`. It must be deterministic: the same
/// character and request give the same scene.
pub trait ExpressionBaker: Send + Sync {
    /// Bakes `request` into `scene`.
    ///
    /// # Errors
    /// [`IoError::Cancelled`] if `progress` is cancelled, [`IoError::Unsupported`] if the character has no face to
    /// bake, [`IoError::Internal`] for bugs. Channels it cannot realise are not errors: they become
    /// [`Realisation::Inert`].
    fn bake(&self, scene: &mut IoScene, request: &BakeRequest, progress: &dyn Progress) -> Result<BakeReport, IoError>;
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn the_arkit_list_is_52_unique_camel_case_names_without_preset_collisions() {
        let unique: BTreeSet<_> = ARKIT_52.iter().collect();
        assert_eq!(unique.len(), 52);
        for name in ARKIT_52 {
            assert!(name.chars().next().is_some_and(|c| c.is_ascii_lowercase()), "{name}");
            assert!(name.chars().all(|c| c.is_ascii_alphabetic()), "{name}");
            assert!(
                matches!(ExpressionName::parse(name), Ok(ExpressionName::Custom(_))),
                "{name} must be a custom expression"
            );
        }
        // 20 left/right pairs and 12 centre channels.
        let sided = ARKIT_52.iter().filter(|n| n.ends_with("Left") || n.ends_with("Right")).count();
        assert_eq!((sided, ARKIT_52.len() - sided), (40, 12));
    }

    #[test]
    fn extras_are_prefixed_and_never_arkit_names() {
        let e = extra_expression("heart-eyes").unwrap();
        assert_eq!(e.as_str(), "omuzaki_heart-eyes");
        assert!(!ARKIT_52.contains(&e.as_str()));
    }

    #[test]
    fn only_off_skips_the_arkit_requirement() {
        assert!(!PerfectSync::Off.wants_arkit());
        assert!(PerfectSync::Arkit52.wants_arkit() && PerfectSync::Arkit52WithExtras.wants_arkit());
        assert_eq!(PerfectSync::default(), PerfectSync::Arkit52, "on by default, extras opt-in");
    }
}
