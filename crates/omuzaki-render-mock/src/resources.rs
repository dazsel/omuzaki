//! What the mock stores for each resource kind, and the scratch target painters draw into.

use std::any::Any;
use std::collections::HashMap;

use omuzaki_core::glam::Vec3;
use omuzaki_render_api::{
    MeshData, MeshUsage, MorphSet, PaintTarget, PixelRect, RenderError, TextureDesc, outline_rims,
};

/// A mesh plus the smooth normals the outline pass pushes vertices along.
pub(crate) struct MeshEntry {
    pub(crate) data: MeshData,
    /// Normals averaged over vertices that share a position, so a hard-edged mesh (a cube has three normals per
    /// corner) expands into a closed hull instead of splitting apart at its edges.
    pub(crate) smooth_normals: Vec<Vec3>,
    /// Where the mesh is open, from the positions it was created with (`outline_rims`): a double-sided material's hulls
    /// widen there. Like the GPU renderer's, they are not found again when the vertices are replaced or morphed.
    pub(crate) rims: Vec<Vec3>,
    /// Whether any vertex has a rim: only an open mesh gets a double-sided material's second hull.
    pub(crate) open: bool,
    /// Whether [`MeshEntry::replace_vertices`] is allowed.
    pub(crate) usage: MeshUsage,
    /// The sparse morph targets, if the mesh was created with any.
    pub(crate) morphs: Option<MorphSet>,
}

impl MeshEntry {
    pub(crate) fn new(data: MeshData, usage: MeshUsage, morphs: Option<MorphSet>) -> Self {
        let smooth_normals = weld_normals(&data.positions, &data.normals);
        let rims = outline_rims(&data);
        let open = rims.iter().any(|r| *r != Vec3::ZERO);
        Self { data, smooth_normals, rims, open, usage, morphs }
    }

    /// Swaps in new positions and normals (the caller has checked lengths and finiteness) and re-welds the outline
    /// normals, which depend on which vertices now share a position.
    pub(crate) fn replace_vertices(&mut self, positions: &[Vec3], normals: &[Vec3]) {
        self.data.positions.clear();
        self.data.positions.extend_from_slice(positions);
        self.data.normals.clear();
        self.data.normals.extend_from_slice(normals);
        self.smooth_normals = weld_normals(&self.data.positions, &self.data.normals);
    }

    /// Bytes this mesh would occupy on a GPU, for `ResourceStats`.
    pub(crate) fn gpu_bytes(&self) -> u64 {
        let d = &self.data;
        let per_vertex = size_of::<Vec3>() * 2
            + size_of::<[f32; 2]>()
            + size_of::<[f32; 4]>()
            + size_of::<[u16; 4]>()
            + size_of::<[f32; 4]>();
        let morph_bytes: usize = self
            .morphs
            .iter()
            .flat_map(MorphSet::targets)
            .map(|t| t.indices.len() * size_of::<u32>() + (t.positions.len() + t.normals.len()) * size_of::<Vec3>())
            .sum();
        (d.positions.len() * per_vertex + d.indices.len() * size_of::<u32>() + morph_bytes) as u64
    }
}

/// Averages `normals` over vertices with identical `positions`. The one definition of the outline directions: creation,
/// vertex updates and morphed poses all call it, so a morphed mesh outlines exactly like a freshly created one.
pub(crate) fn weld_normals(positions: &[Vec3], normals: &[Vec3]) -> Vec<Vec3> {
    // `+ 0.0` turns -0.0 into 0.0, so the two zeros weld together.
    let key = |p: Vec3| [(p.x + 0.0).to_bits(), (p.y + 0.0).to_bits(), (p.z + 0.0).to_bits()];
    let mut slot_of: HashMap<[u32; 3], usize> = HashMap::new();
    let mut sums: Vec<Vec3> = Vec::new();
    let slots: Vec<usize> = positions
        .iter()
        .zip(normals)
        .map(|(&p, &n)| {
            let slot = *slot_of.entry(key(p)).or_insert_with(|| {
                sums.push(Vec3::ZERO);
                sums.len() - 1
            });
            sums[slot] += n;
            slot
        })
        .collect();
    slots.iter().zip(normals).map(|(&slot, &own)| sums[slot].try_normalize().unwrap_or(own)).collect()
}

/// A texture's description and its pixels (RGBA8, as stored: possibly premultiplied).
#[derive(Clone)]
pub(crate) struct TextureData {
    pub(crate) desc: TextureDesc,
    pub(crate) rgba: Vec<u8>,
}

/// The mock has no GPU, so a painter's `native()` request gets this empty marker. A GPU painter's downcast fails and
/// it falls back to `write_rgba`, which is exactly the path the conformance checks exercise.
#[derive(Debug)]
pub struct MockNative;

/// A scratch copy of a dynamic texture. The mock commits it only if the painter succeeds, which is how it honours
/// "on error the texture keeps its previous contents".
pub(crate) struct MockPaintTarget {
    desc: TextureDesc,
    pub(crate) rgba: Vec<u8>,
    native: MockNative,
}

impl MockPaintTarget {
    pub(crate) fn new(texture: &TextureData) -> Self {
        Self { desc: texture.desc, rgba: texture.rgba.clone(), native: MockNative }
    }
}

impl PaintTarget for MockPaintTarget {
    fn desc(&self) -> &TextureDesc {
        &self.desc
    }

    fn write_rgba(&mut self, rect: PixelRect, rgba: &[u8]) -> Result<(), RenderError> {
        if !rect.fits(self.desc.size) {
            return Err(RenderError::invalid(
                "paint rectangle",
                format!("{rect:?} doesn't fit a {}x{} texture", self.desc.size.x, self.desc.size.y),
            ));
        }
        if rgba.len() != rect.byte_len() {
            return Err(RenderError::invalid(
                "paint pixels",
                format!("{} bytes for {rect:?} (needs {})", rgba.len(), rect.byte_len()),
            ));
        }
        let row_bytes = rect.width as usize * 4;
        for (row, src) in rgba.chunks_exact(row_bytes.max(1)).enumerate() {
            let at = ((rect.y as usize + row) * self.desc.size.x as usize + rect.x as usize) * 4;
            self.rgba[at..at + row_bytes].copy_from_slice(src);
        }
        Ok(())
    }

    fn native(&mut self) -> &mut dyn Any {
        &mut self.native
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::glam::UVec2;
    use omuzaki_render_api::TextureDesc;

    use super::*;
    use crate::shapes;

    #[test]
    fn a_cube_hull_follows_the_corner_diagonals() {
        let cube = MeshEntry::new(shapes::cuboid(Vec3::splat(0.5)), MeshUsage::Static, None);
        for (p, n) in cube.data.positions.iter().zip(&cube.smooth_normals) {
            let diagonal = p.signum().normalize();
            assert!((*n - diagonal).length() < 1e-5, "corner {p:?} expands along {n:?}, not {diagonal:?}");
        }
    }

    #[test]
    fn writes_land_in_the_right_rows_and_bad_rects_are_rejected() {
        let tex = TextureData { desc: TextureDesc::colour(UVec2::new(4, 4)).dynamic(), rgba: vec![0; 64] };
        let mut target = MockPaintTarget::new(&tex);
        let rect = PixelRect { x: 1, y: 2, width: 2, height: 1 };
        target.write_rgba(rect, &[9; 8]).unwrap();
        let painted: Vec<usize> = (0..16).filter(|i| target.rgba[i * 4] == 9).collect();
        assert_eq!(painted, vec![9, 10], "row 2, columns 1 and 2");
        assert!(target.write_rgba(PixelRect { x: 3, y: 0, width: 2, height: 1 }, &[0; 8]).is_err());
        assert!(target.write_rgba(rect, &[0; 7]).is_err());
    }
}
