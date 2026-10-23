//! Upper bounds for the GPU memory a renderer holds for what it is given (#2092): what a caller adds up to plan a job
//! before anything is uploaded, such as the cost hint of a render. A renderer's [`ResourceStats::gpu_bytes`] for a
//! resource never exceeds its bound here; `omuzaki-render`'s `memory` test holds both renderers to that, and to the
//! bounds being no more than twice what they report, so a plan neither admits what does not fit nor refuses what does.
//!
//! [`ResourceStats::gpu_bytes`]: crate::ResourceStats::gpu_bytes

use crate::{AlphaEncoding, MeshData, MeshOptions, MeshUsage, TextureDesc, TextureUsage};

/// Bytes a vertex of an uploaded mesh takes at most: its position and normal (24), its UV, colour, joints and weights
/// (48), the smoothed normal the outline is pushed along (12), and on an open mesh its rim (12, `outline_rims`).
const VERTEX_BYTES: u64 = 96;
/// Bytes of an index.
const INDEX_BYTES: u64 = 4;
/// Bytes of one moved vertex of one morph target, regrouped by vertex for the GPU blend: the target, whether it moves
/// the normal, and the position and normal deltas.
const MORPH_ENTRY_BYTES: u64 = 32;
/// Bytes a vertex takes in the per-vertex tables of the GPU morph blend and the outline weld: the first entry of each
/// vertex, its welded slot, the first member of each slot and the members themselves.
const MORPH_TABLE_BYTES_PER_VERTEX: u64 = 16;
/// Bytes a vertex takes in the morphed copy an instance of a morphed mesh draws: its blended position and normal (24)
/// and its smoothed normal (12).
const MORPH_COPY_BYTES_PER_VERTEX: u64 = 36;
/// Fixed bytes of a morphed mesh's tables and copy: each storage buffer is padded to at least 16 bytes, and the copy's
/// parameters start with a 16 byte header.
const MORPH_FIXED_BYTES: u64 = 256;

/// Bytes a material takes at most: its uniform block (a renderer's is a few hundred bytes).
pub const MATERIAL_BYTES: u64 = 1024;

/// The most GPU memory a renderer holds for a texture made from `desc`: its pixels, a third more for a mip chain, and
/// twice that for a repaintable straight-alpha texture, which keeps the painted copy beside the premultiplied one it
/// samples.
#[must_use]
pub fn texture_bytes(desc: &TextureDesc) -> u64 {
    let pixels = desc.byte_len() as u64;
    let chain = if desc.mips { pixels * 4 / 3 } else { pixels };
    let copies = if desc.usage == TextureUsage::Dynamic && desc.alpha == AlphaEncoding::Straight { 2 } else { 1 };
    copies * chain
}

/// The most GPU memory a renderer holds for `mesh` created with `options`, drawn by one instance: its vertices,
/// indices and extra attributes; for a morphed mesh, its targets regrouped by vertex, the per-vertex tables of the
/// blend and the weld, and the morphed copy the instance draws; and for a dynamic mesh, the weld's tables.
#[must_use]
pub fn mesh_bytes(mesh: &MeshData, options: &MeshOptions) -> u64 {
    let vertices = mesh.positions.len() as u64;
    let extras: u64 = mesh.extra.iter().map(|a| a.data.len() as u64 * 4).sum();
    let mut bytes = vertices * VERTEX_BYTES + mesh.indices.len() as u64 * INDEX_BYTES + extras;
    let morphs = options.morphs.as_ref().filter(|set| !set.is_empty());
    if morphs.is_some() || options.usage == MeshUsage::Dynamic {
        bytes += (vertices + 1) * MORPH_TABLE_BYTES_PER_VERTEX + MORPH_FIXED_BYTES;
    }
    if let Some(set) = morphs {
        let entries: u64 = set.targets().iter().map(|t| t.indices.len() as u64).sum();
        bytes += entries * MORPH_ENTRY_BYTES + vertices * MORPH_COPY_BYTES_PER_VERTEX + set.len() as u64 * 4;
    }
    bytes
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::{UVec2, Vec3};

    use super::*;
    use crate::{MorphSet, SparseMorph};

    #[test]
    fn a_4096_mipped_texture_is_its_pixels_and_a_third() {
        let desc = TextureDesc::colour(UVec2::splat(4096));
        assert_eq!(texture_bytes(&desc), 4096 * 4096 * 4 * 4 / 3);
        let mut painted = desc;
        (painted.usage, painted.mips) = (TextureUsage::Dynamic, false);
        assert_eq!(texture_bytes(&painted), 2 * 4096 * 4096 * 4, "a painted straight texture keeps two copies");
    }

    #[test]
    fn a_morphed_mesh_counts_its_entries_and_its_copy() {
        let mesh = omuzaki_core::mesh::MeshData {
            positions: vec![Vec3::ZERO; 3],
            normals: vec![Vec3::Z; 3],
            indices: vec![0, 1, 2],
            ..Default::default()
        };
        let plain = mesh_bytes(&mesh, &MeshOptions::default());
        assert_eq!(plain, 3 * VERTEX_BYTES + 3 * INDEX_BYTES);
        let target = SparseMorph {
            name: omuzaki_core::id::Slug::new("blink").unwrap(),
            indices: vec![0, 2],
            positions: vec![Vec3::X; 2],
            normals: Vec::new(),
        };
        let options =
            MeshOptions { usage: MeshUsage::Static, morphs: Some(MorphSet::from_targets(3, vec![target]).unwrap()) };
        let morphed = mesh_bytes(&mesh, &options);
        assert_eq!(
            morphed - plain,
            4 * MORPH_TABLE_BYTES_PER_VERTEX
                + MORPH_FIXED_BYTES
                + 2 * MORPH_ENTRY_BYTES
                + 3 * MORPH_COPY_BYTES_PER_VERTEX
                + 4
        );
    }
}
