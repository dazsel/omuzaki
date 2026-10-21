//! Ids and the two closed sets every connector shares: the [`Capability`] a call asks for and the [`MediaKind`] of the
//! bytes that go in or come out.

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

omuzaki_core::slug_id!(
    /// A connector: `comfyui`, `local-llm`, `mock-image`. The manifest's `id`, and the second segment of its key label
    /// `connector/<id>`.
    ConnectorId
);

omuzaki_core::slug_id!(
    /// One tray entry, `r<run>-<index>` for an output of a run ([`CandidateId::of_run`]); the key of its record at
    /// `tray/<id>`.
    CandidateId
);

impl CandidateId {
    /// The id of output `index` of run `run`: `r42-0`.
    ///
    /// # Errors
    /// None in practice: `r`, digits and `-` always form a slug of at most 32 characters. The `Result` keeps the one
    /// validating constructor of [`Slug`](omuzaki_core::id::Slug) the only way in.
    pub fn of_run(run: RunId, index: u32) -> Result<Self, omuzaki_core::id::SlugError> {
        Self::new(format!("r{}-{index}", run.0))
    }
}

/// The run a call belongs to: the job's id (design D4.5). Seeds and candidate ids derive from it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct RunId(pub u64);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// What a call makes. Closed by design (D2): the capability decides the request type, the output kinds and how a
/// caller uses the result, so a new one is an API change. Adapter knobs go in a request's `params`.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Images from a prompt and references.
    Image,
    /// Textures, tileable or not.
    Texture,
    /// A 3D model.
    Model3d,
    /// Speech from text.
    Voice,
    /// Music or a sound effect.
    Music,
    /// Motion for a rig.
    Motion,
    /// One finished LLM turn (the LLM seam does the rest).
    Llm,
    /// An answer about images.
    Vision,
    /// A call into a DCC program (Blender, Krita) over MCP.
    Dcc,
}

impl Capability {
    /// Every capability, in declaration order.
    pub const ALL: [Capability; 9] = [
        Capability::Image,
        Capability::Texture,
        Capability::Model3d,
        Capability::Voice,
        Capability::Music,
        Capability::Motion,
        Capability::Llm,
        Capability::Vision,
        Capability::Dcc,
    ];

    /// The name used in manifests and requests: `image`, `model3d`.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Image => "image",
            Capability::Texture => "texture",
            Capability::Model3d => "model3d",
            Capability::Voice => "voice",
            Capability::Music => "music",
            Capability::Motion => "motion",
            Capability::Llm => "llm",
            Capability::Vision => "vision",
            Capability::Dcc => "dcc",
        }
    }

    /// Whether an output of `kind` fits this capability: image and texture give images, model3d meshes, voice and
    /// music audio, motion motion, llm and vision text or JSON; dcc may give anything.
    pub fn allows_output(self, kind: MediaKind) -> bool {
        match self {
            Capability::Image | Capability::Texture => kind == MediaKind::Image,
            Capability::Model3d => kind == MediaKind::Mesh,
            Capability::Voice | Capability::Music => kind == MediaKind::Audio,
            Capability::Motion => kind == MediaKind::Motion,
            Capability::Llm | Capability::Vision => matches!(kind, MediaKind::Text | MediaKind::Json),
            Capability::Dcc => true,
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What kind of bytes go in or out; the exact media type travels with each artifact.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    /// A raster image.
    Image,
    /// Audio.
    Audio,
    /// A 3D mesh or scene.
    Mesh,
    /// Motion data (BVH, FBX animation).
    Motion,
    /// Video.
    Video,
    /// Plain text.
    Text,
    /// A JSON value.
    Json,
}

impl MediaKind {
    /// The name used in manifests: `image`, `mesh`.
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Audio => "audio",
            MediaKind::Mesh => "mesh",
            MediaKind::Motion => "motion",
            MediaKind::Video => "video",
            MediaKind::Text => "text",
            MediaKind::Json => "json",
        }
    }
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_ids_derive_from_the_run() {
        assert_eq!(CandidateId::of_run(RunId(42), 0).unwrap().as_str(), "r42-0");
        assert_eq!(
            CandidateId::of_run(RunId(u64::MAX), u32::MAX).unwrap().as_str(),
            "r18446744073709551615-4294967295"
        );
    }

    #[test]
    fn capability_names_match_serde() {
        for c in Capability::ALL {
            assert_eq!(serde_json::to_value(c).unwrap(), serde_json::Value::String(c.as_str().to_owned()));
        }
    }

    #[test]
    fn output_kinds_follow_the_capability() {
        assert!(Capability::Texture.allows_output(MediaKind::Image));
        assert!(!Capability::Image.allows_output(MediaKind::Mesh));
        assert!(Capability::Vision.allows_output(MediaKind::Json));
        assert!(Capability::Dcc.allows_output(MediaKind::Video));
    }
}
