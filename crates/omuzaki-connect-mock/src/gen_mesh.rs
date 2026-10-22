//! 3D models: a minimal valid GLB 2.0 holding one triangle whose size comes from the seed. Built by hand: a mock never
//! depends on `omuzaki-gltf`.

use omuzaki_connect_api::{Artifact, Capability, ConnectError, MediaKind, Output, Request};
use serde_json::json;

use crate::fake::{Generator, seed_of, wrong_kind};

/// The model fake meshes report.
pub const MODEL: &str = "mock-3d@1";

/// The BIN chunk: three `f32` positions.
const BIN_LEN: usize = 36;

pub(crate) struct MeshGen;

impl Generator for MeshGen {
    fn capability(&self) -> Capability {
        Capability::Model3d
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        if !matches!(req, Request::Model3d(_)) {
            return Err(wrong_kind(Capability::Model3d, req));
        }
        let seed = seed_of(req, index);
        // A size from 0.5 to 1.499 in thousandths, exact in f32.
        let size = 0.5 + f32::from(u16::try_from(seed % 1_000).unwrap_or(0)) / 1_000.0;
        Ok(Output::Asset(Artifact {
            bytes: glb(size)?,
            media_type: "model/gltf-binary".into(),
            kind: MediaKind::Mesh,
            name: None,
            model: MODEL.into(),
            seed: Some(seed),
            rights: None,
        }))
    }
}

/// A GLB with one triangle `(0,0,0) (size,0,0) (0,size,0)`.
fn glb(size: f32) -> Result<Vec<u8>, ConnectError> {
    let positions = [0.0, 0.0, 0.0, size, 0.0, 0.0, 0.0, size, 0.0f32];
    let gltf = json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"mesh": 0}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0.0, 0.0, 0.0], "max": [size, size, 0.0]
        }],
        "bufferViews": [{"buffer": 0, "byteOffset": 0, "byteLength": BIN_LEN}],
        "buffers": [{"byteLength": BIN_LEN}]
    });
    let mut json_chunk = serde_json::to_vec(&gltf).map_err(|e| ConnectError::failed(e.to_string()))?;
    json_chunk.resize(json_chunk.len().next_multiple_of(4), b' ');
    let bin: Vec<u8> = positions.iter().flat_map(|p| p.to_le_bytes()).collect();
    let total = 12 + 8 + json_chunk.len() + 8 + bin.len();
    let len32 = |n: usize| u32::try_from(n).map_err(|_| ConnectError::failed("the fake GLB is too large"));
    let mut out = Vec::with_capacity(total);
    out.extend_from_slice(b"glTF");
    out.extend_from_slice(&2u32.to_le_bytes());
    out.extend_from_slice(&len32(total)?.to_le_bytes());
    out.extend_from_slice(&len32(json_chunk.len())?.to_le_bytes());
    out.extend_from_slice(b"JSON");
    out.extend_from_slice(&json_chunk);
    out.extend_from_slice(&len32(bin.len())?.to_le_bytes());
    out.extend_from_slice(b"BIN\0");
    out.extend_from_slice(&bin);
    Ok(out)
}
