//! Accessors, buffer views and buffers: the binary data and how to read it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::{Extensions, is_zero};

/// A typed view of buffer data (`accessor`).
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Accessor {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The buffer view holding the data; none means all zeros (plus the sparse overlay, if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_view: Option<usize>,
    /// Offset into the buffer view.
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    /// Component type code (see [`code`](super::code)).
    pub component_type: u32,
    /// Whether integer components are normalised to 0..1 or -1..1.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub normalized: bool,
    /// Number of elements.
    pub count: usize,
    /// Element type: `SCALAR`, `VEC2`, `VEC3`, `VEC4`, `MAT2`, `MAT3` or `MAT4`.
    #[serde(rename = "type")]
    pub kind: String,
    /// Per-component minimum (required on `POSITION`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<Vec<f32>>,
    /// Per-component maximum (required on `POSITION`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<Vec<f32>>,
    /// Sparse storage: a few elements replacing a base array.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sparse: Option<Sparse>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// Sparse storage (glTF 2.0, "Sparse Accessors"): `count` elements replace the base array at `indices`.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sparse {
    /// How many elements are replaced.
    pub count: usize,
    /// Where they are (strictly increasing element indices).
    pub indices: SparseIndices,
    /// What they become.
    pub values: SparseValues,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// The indices of a [`Sparse`] block.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SparseIndices {
    /// The buffer view holding them.
    pub buffer_view: usize,
    /// Offset into the buffer view.
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    /// Unsigned byte, short or int.
    pub component_type: u32,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// The values of a [`Sparse`] block.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SparseValues {
    /// The buffer view holding them.
    pub buffer_view: usize,
    /// Offset into the buffer view.
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A byte range of a buffer.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct BufferView {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The buffer.
    pub buffer: usize,
    /// Offset into the buffer.
    #[serde(skip_serializing_if = "is_zero")]
    pub byte_offset: usize,
    /// Length of the range.
    pub byte_length: usize,
    /// Bytes between elements for interleaved vertex data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_stride: Option<usize>,
    /// Intended GPU binding (see [`code`](super::code)); only vertex attributes and indices have one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<u32>,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}

/// A block of binary data.
#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Buffer {
    /// Display name.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Where the bytes are: a relative path or `data:` URI. None means the GLB's binary chunk (buffer 0 only).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Length in bytes.
    pub byte_length: usize,
    /// Extensions by name.
    #[serde(skip_serializing_if = "Extensions::is_empty")]
    pub extensions: Extensions,
    /// Application-specific data.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extras: Option<Value>,
}
