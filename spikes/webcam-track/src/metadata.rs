//! The canonical face model MediaPipe ships next to the three networks, and a tiny protobuf reader to get it out.
//!
//! `geometry_pipeline_metadata_landmarks.binarypb` is a `GeometryPipelineMetadata` message (MediaPipe
//! `geometry_pipeline_metadata.proto`): a mesh of 468 canonical vertices in centimetres (5 floats each: x, y, z,
//! u, v) and the weight each landmark gets when fitting a head pose to it. Reading three fields does not justify
//! a protobuf code generator, so this decodes the wire format directly.

use std::path::Path;

use crate::error::{Result, TrackError};

/// Landmarks in the canonical mesh (the mesh landmarks, without the 10 iris points).
pub const MESH_LANDMARKS: usize = 468;
/// Floats per mesh vertex: position (3) and texture coordinate (2).
const FLOATS_PER_VERTEX: usize = 5;

/// A decoded protobuf field value.
#[derive(Debug, Clone, Copy)]
enum Value<'a> {
    Varint(u64),
    Fixed32(u32),
    Bytes(&'a [u8]),
}

/// Walks the fields of one message, yielding `(field number, value)`.
struct Fields<'a> {
    data: &'a [u8],
}

impl<'a> Fields<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data }
    }

    fn varint(&mut self) -> Option<u64> {
        let mut result = 0u64;
        for shift in (0..64).step_by(7) {
            let (&byte, rest) = self.data.split_first()?;
            self.data = rest;
            result |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Some(result);
            }
        }
        None
    }

    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, rest) = self.data.split_at_checked(n)?;
        self.data = rest;
        Some(head)
    }
}

impl<'a> Iterator for Fields<'a> {
    type Item = Result<(u64, Value<'a>)>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.data.is_empty() {
            return None;
        }
        let truncated = || TrackError::Model("truncated protobuf in geometry metadata".into());
        let key = self.varint()?;
        let (field, wire) = (key >> 3, key & 7);
        let value = match wire {
            0 => self.varint().map(Value::Varint),
            2 => self.varint().and_then(|n| usize::try_from(n).ok()).and_then(|n| self.take(n)).map(Value::Bytes),
            5 => self.take(4).and_then(|b| b.try_into().ok()).map(|b| Value::Fixed32(u32::from_le_bytes(b))),
            other => return Some(Err(TrackError::Model(format!("unsupported protobuf wire type {other}")))),
        };
        Some(value.map(|v| (field, v)).ok_or_else(truncated))
    }
}

/// Appends the floats of a repeated `float` field, which proto2 may store packed (one blob) or one per tag.
fn push_floats(out: &mut Vec<f32>, value: Value<'_>) -> Result<()> {
    match value {
        Value::Fixed32(bits) => out.push(f32::from_bits(bits)),
        Value::Bytes(blob) if blob.len() % 4 == 0 => {
            out.extend(blob.as_chunks::<4>().0.iter().map(|b| f32::from_le_bytes(*b)));
        }
        _ => return Err(TrackError::Model("float field has an unexpected encoding".into())),
    }
    Ok(())
}

/// What the head-pose fit needs from the bundle.
#[derive(Debug, Clone)]
pub struct GeometryMetadata {
    /// Canonical landmark positions in centimetres.
    pub canonical: Vec<[f32; 3]>,
    /// Fit weight of each landmark (0 for landmarks that are not part of the Procrustes basis).
    pub weights: Vec<f32>,
}

impl GeometryMetadata {
    /// Reads the metadata file from the model bundle.
    ///
    /// # Errors
    /// Returns an error when the file cannot be read or is not the expected message.
    pub fn load(path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path).map_err(|source| TrackError::Io { path: path.to_path_buf(), source })?;
        Self::from_bytes(&bytes)
    }

    /// Decodes a serialized `GeometryPipelineMetadata`.
    ///
    /// # Errors
    /// Returns [`TrackError::Model`] when the bytes are not valid protobuf or the mesh is not 468 vertices.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut vertex_buffer = Vec::new();
        let mut weights = vec![0.0; MESH_LANDMARKS];
        for field in Fields::new(bytes) {
            match field? {
                (1, Value::Bytes(mesh)) => {
                    for mesh_field in Fields::new(mesh) {
                        if let (3, value) = mesh_field? {
                            push_floats(&mut vertex_buffer, value)?;
                        }
                    }
                }
                (2, Value::Bytes(reference)) => {
                    let (mut id, mut weight) = (None, 0.0f32);
                    for ref_field in Fields::new(reference) {
                        match ref_field? {
                            (1, Value::Varint(v)) => id = usize::try_from(v).ok(),
                            (2, Value::Fixed32(bits)) => weight = f32::from_bits(bits),
                            _ => {}
                        }
                    }
                    let slot = id
                        .and_then(|i| weights.get_mut(i))
                        .ok_or_else(|| TrackError::Model("procrustes landmark id out of range".into()))?;
                    *slot = weight;
                }
                _ => {}
            }
        }
        if vertex_buffer.len() != MESH_LANDMARKS * FLOATS_PER_VERTEX {
            return Err(TrackError::Model(format!(
                "canonical mesh has {} floats, expected {}",
                vertex_buffer.len(),
                MESH_LANDMARKS * FLOATS_PER_VERTEX
            )));
        }
        let canonical = vertex_buffer.as_chunks::<FLOATS_PER_VERTEX>().0.iter().map(|v| [v[0], v[1], v[2]]).collect();
        Ok(Self { canonical, weights })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::cast_precision_loss)] // test fixtures are tiny
    use super::*;

    fn varint(mut v: u64) -> Vec<u8> {
        let mut out = Vec::new();
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(byte);
                return out;
            }
            out.push(byte | 0x80);
        }
    }

    fn len_field(field: u64, payload: &[u8]) -> Vec<u8> {
        let mut out = varint(field << 3 | 2);
        out.extend(varint(payload.len() as u64));
        out.extend_from_slice(payload);
        out
    }

    /// A metadata message with a mesh whose vertex i is (i, 2i, 3i, 0, 0) and weights on landmarks 3 and 5.
    fn sample(packed: bool) -> Vec<u8> {
        let mut mesh = varint(1 << 3); // vertex_type = VERTEX_PT (value 0)
        mesh.push(0);
        let floats: Vec<f32> =
            (0..MESH_LANDMARKS).flat_map(|i| [i as f32, 2.0 * i as f32, 3.0 * i as f32, 0.0, 0.0]).collect();
        if packed {
            mesh.extend(len_field(3, &floats.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<_>>()));
        } else {
            for f in &floats {
                mesh.extend(varint(3 << 3 | 5));
                mesh.extend(f.to_le_bytes());
            }
        }
        let mut msg = len_field(1, &mesh);
        for (id, weight) in [(3u64, 0.5f32), (5, 2.0)] {
            let mut reference = varint(1 << 3);
            reference.extend(varint(id));
            reference.extend(varint(2 << 3 | 5));
            reference.extend(weight.to_le_bytes());
            msg.extend(len_field(2, &reference));
        }
        msg
    }

    #[test]
    fn decodes_packed_and_unpacked_meshes() {
        for packed in [true, false] {
            let meta = GeometryMetadata::from_bytes(&sample(packed)).unwrap();
            assert_eq!(meta.canonical.len(), MESH_LANDMARKS);
            assert_eq!(meta.canonical[7], [7.0, 14.0, 21.0]);
            assert_eq!((meta.weights[3], meta.weights[5], meta.weights[4]), (0.5, 2.0, 0.0));
        }
    }

    #[test]
    fn rejects_truncated_and_wrong_sized_input() {
        let good = sample(true);
        assert!(GeometryMetadata::from_bytes(&good[..good.len() / 2]).is_err());
        assert!(GeometryMetadata::from_bytes(&[]).is_err(), "no mesh at all");
        let mut bad_id = len_field(1, &[]);
        let mut reference = varint(1 << 3);
        reference.extend(varint(9999));
        bad_id.extend(len_field(2, &reference));
        assert!(GeometryMetadata::from_bytes(&bad_id).is_err());
    }
}
