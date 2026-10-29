//! What can stop an import or a write, and the bounds a file is held to.

use omuzaki_puppet_api::PuppetError;

/// Bounds on what a file may declare. Every length and count is checked against these (and against the bytes left)
/// before anything is allocated for it, so a hostile file costs at most what the limits allow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Limits {
    /// The whole file, bytes.
    pub max_bytes: u64,
    /// The puppet payload (INP1's JSON, INP2's tree), bytes.
    pub max_payload_bytes: u64,
    /// Nodes in the tree.
    pub max_nodes: usize,
    /// Nesting: node tree depth, and INP2 array and object depth.
    pub max_depth: usize,
    /// Mesh vertices, summed over every mesh.
    pub max_vertices: usize,
    /// Parameters.
    pub max_params: usize,
    /// Deform offsets, summed over every key point of every deform binding.
    pub max_offsets: usize,
    /// Texture slots.
    pub max_textures: usize,
    /// One encoded texture, bytes.
    pub max_texture_bytes: u64,
    /// A texture's width or height, pixels.
    pub max_texture_dim: u32,
    /// Every decoded texture together, RGBA8 bytes.
    pub max_decoded_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: 1 << 30,
            max_payload_bytes: 256 << 20,
            max_nodes: 65_536,
            max_depth: 64,
            max_vertices: 4 << 20,
            max_params: 4096,
            max_offsets: 32 << 20,
            max_textures: 1024,
            max_texture_bytes: 256 << 20,
            max_texture_dim: 16_384,
            max_decoded_bytes: 2 << 30,
        }
    }
}

/// Why a file was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ImportError {
    /// The bytes are not a well-formed puppet: a length past the end, bad JSON, a reference to nothing.
    #[error("{rule} at {path}: {detail}")]
    Malformed {
        /// The rule, `inochi2d.<rule>` (see [`crate::rule`]).
        rule: &'static str,
        /// Where: a byte offset or section in the container, a JSON path in the payload.
        path: String,
        /// What is wrong.
        detail: String,
    },
    /// The file declares more than [`Limits`] allows.
    #[error("inochi2d.limit at {path}: {limit} exceeded: {detail}")]
    Limit {
        /// The [`Limits`] field that was exceeded.
        limit: &'static str,
        /// Where.
        path: String,
        /// What was declared.
        detail: String,
    },
    /// A well-formed file this importer cannot map.
    #[error("{rule}: {detail}")]
    Unsupported {
        /// The rule, `inochi2d.<rule>`.
        rule: &'static str,
        /// Why.
        detail: String,
    },
}

impl ImportError {
    /// The stable rule id: `inochi2d.<rule>`, or `inochi2d.limit` for a [`ImportError::Limit`].
    #[must_use]
    pub fn rule(&self) -> &'static str {
        match self {
            Self::Malformed { rule, .. } | Self::Unsupported { rule, .. } => rule,
            Self::Limit { .. } => crate::rule::LIMIT,
        }
    }

    /// Where the problem is (empty for [`ImportError::Unsupported`]).
    #[must_use]
    pub fn path(&self) -> &str {
        match self {
            Self::Malformed { path, .. } | Self::Limit { path, .. } => path,
            Self::Unsupported { .. } => "",
        }
    }
}

pub(crate) fn malformed(rule: &'static str, path: impl Into<String>, detail: impl Into<String>) -> ImportError {
    ImportError::Malformed { rule, path: path.into(), detail: detail.into() }
}

pub(crate) fn limit(limit: &'static str, path: impl Into<String>, detail: impl Into<String>) -> ImportError {
    ImportError::Limit { limit, path: path.into(), detail: detail.into() }
}

/// Why [`crate::write_inp1`] could not write a puppet.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum WriteError {
    /// The puppet fails the contract's own checks.
    #[error("the puppet fails its checks: {0}")]
    Invalid(#[from] PuppetError),
    /// The textures passed don't match the puppet's texture list.
    #[error("texture {index}: {detail}")]
    Texture {
        /// Which texture.
        index: usize,
        /// What doesn't match.
        detail: String,
    },
    /// Something INP1 cannot hold.
    #[error("{path}: INP1 cannot hold {detail}")]
    Unsupported {
        /// Where in the puppet (`nodes/3`).
        path: String,
        /// What.
        detail: String,
    },
    /// A texture failed to encode.
    #[error("texture {index} failed to encode: {detail}")]
    Encode {
        /// Which texture.
        index: usize,
        /// The encoder's message.
        detail: String,
    },
}
