//! The binary buffer an export builds: one buffer view per accessor, each starting on a 4-byte boundary, which keeps
//! every component aligned to its size (glTF 2.0, "Buffers and Buffer Views", "Accessors").

use omuzaki_core::glam::{Mat4, Vec3};
use omuzaki_io_api::ExtraAttribute;

use crate::schema::{Accessor as AccessorDef, BufferView as BufferViewDef, Sparse, SparseIndices, SparseValues, code};

/// Accessors and views being assembled, and the bytes they index.
///
/// Every method appends one accessor (or one view) and returns its index; the `Vec`s are public so a caller can
/// read back or adjust what was packed. Output is deterministic: the same calls give the same bytes.
#[derive(Default)]
pub struct Packer {
    /// The binary buffer (buffer 0).
    pub bin: Vec<u8>,
    /// The buffer views, indexing `bin`.
    pub views: Vec<BufferViewDef>,
    /// The accessors, indexing `views`.
    pub accessors: Vec<AccessorDef>,
}

/// Per-component minimum and maximum, which glTF requires on every `POSITION` accessor (and morph-target position
/// accessor).
fn bounds(data: &[Vec3]) -> (Vec<f32>, Vec<f32>) {
    let Some(first) = data.first() else { return (vec![0.0; 3], vec![0.0; 3]) };
    let (min, max) = data.iter().fold((*first, *first), |(lo, hi), v| (lo.min(*v), hi.max(*v)));
    // Non-finite data is refused before export (`gltf.non_finite`); this keeps a stray infinity out of the JSON anyway.
    let finite = |v: Vec3| v.to_array().map(|c| if c.is_finite() { c } else { 0.0 }).to_vec();
    (finite(min), finite(max))
}

fn le_f32(values: impl Iterator<Item = f32>) -> Vec<u8> {
    values.flat_map(f32::to_le_bytes).collect()
}

/// The smallest unsigned component type that can index `n` elements, with its size in bytes.
fn sparse_index_type(n: usize) -> (u32, usize) {
    if n <= 256 {
        (code::U8, 1)
    } else if n <= 65_536 {
        (code::U16, 2)
    } else {
        (code::U32, 4)
    }
}

/// The index `i` in the little-endian bytes of `component_type` (it fits: the type was chosen from the element count).
fn index_bytes(i: usize, component_type: u32) -> Vec<u8> {
    let i = u32::try_from(i).unwrap_or(u32::MAX);
    match component_type {
        code::U8 => vec![u8::try_from(i).unwrap_or(u8::MAX)],
        code::U16 => u16::try_from(i).unwrap_or(u16::MAX).to_le_bytes().to_vec(),
        _ => i.to_le_bytes().to_vec(),
    }
}

/// Sparse storage wins when the moved elements' indices and values (plus the two views' alignment padding) are fewer
/// bytes than the dense array. An all-zero target is always sparse: it costs nothing.
fn is_sparse_smaller(nonzero: usize, index_size: usize, n: usize) -> bool {
    nonzero == 0 || nonzero * (index_size + 12) + 64 <= n * 12
}

impl Packer {
    /// Appends `bytes` as a buffer view (4-byte aligned) and returns its index. `target` is the GPU binding for vertex
    /// attributes and indices, and `None` for everything else (images, matrices, sparse parts).
    pub fn view(&mut self, bytes: &[u8], target: Option<u32>) -> usize {
        self.bin.resize(self.bin.len().next_multiple_of(4), 0);
        self.views.push(BufferViewDef {
            buffer: 0,
            byte_offset: self.bin.len(),
            byte_length: bytes.len(),
            target,
            ..BufferViewDef::default()
        });
        self.bin.extend_from_slice(bytes);
        self.views.len() - 1
    }

    fn accessor(&mut self, bytes: &[u8], target: Option<u32>, def: AccessorDef) -> usize {
        let view = self.view(bytes, target);
        self.accessors.push(AccessorDef { buffer_view: Some(view), ..def });
        self.accessors.len() - 1
    }

    fn float_accessor(
        &mut self,
        bytes: &[u8],
        count: usize,
        kind: &str,
        min_max: Option<(Vec<f32>, Vec<f32>)>,
    ) -> usize {
        let (min, max) = min_max.unzip();
        let def =
            AccessorDef { component_type: code::F32, count, kind: kind.to_owned(), min, max, ..AccessorDef::default() };
        self.accessor(bytes, Some(code::ARRAY_BUFFER), def)
    }

    /// A `VEC3` of floats; `with_bounds` adds the `min`/`max` that positions need (glTF requires them on `POSITION`,
    /// morph target positions included).
    pub fn vec3(&mut self, data: &[Vec3], with_bounds: bool) -> usize {
        let bytes = le_f32(data.iter().flat_map(Vec3::to_array));
        self.float_accessor(&bytes, data.len(), "VEC3", with_bounds.then(|| bounds(data)))
    }

    /// A `VEC2` of floats.
    pub fn vec2(&mut self, data: &[omuzaki_core::glam::Vec2]) -> usize {
        let bytes = le_f32(data.iter().flat_map(omuzaki_core::glam::Vec2::to_array));
        self.float_accessor(&bytes, data.len(), "VEC2", None)
    }

    /// A `VEC4` of floats (colours, weights).
    pub fn vec4(&mut self, data: &[[f32; 4]]) -> usize {
        let bytes = le_f32(data.iter().flatten().copied());
        self.float_accessor(&bytes, data.len(), "VEC4", None)
    }

    /// `JOINTS_0`: four unsigned 16-bit joint indices per vertex.
    pub fn joints(&mut self, data: &[[u16; 4]]) -> usize {
        let bytes: Vec<u8> = data.iter().flatten().flat_map(|j| j.to_le_bytes()).collect();
        let def =
            AccessorDef { component_type: code::U16, count: data.len(), kind: "VEC4".into(), ..AccessorDef::default() };
        self.accessor(&bytes, Some(code::ARRAY_BUFFER), def)
    }

    /// Triangle indices, 16-bit when every vertex index fits (the value 65535 is reserved by the spec, so a mesh needs at
    /// most 65535 vertices), else 32-bit.
    pub fn indices(&mut self, data: &[u32], vertices: usize) -> usize {
        let (component_type, bytes): (u32, Vec<u8>) = if u16::try_from(vertices).is_ok() {
            let narrow = data.iter().flat_map(|&i| u16::try_from(i).unwrap_or(u16::MAX).to_le_bytes());
            (code::U16, narrow.collect())
        } else {
            (code::U32, data.iter().flat_map(|i| i.to_le_bytes()).collect())
        };
        let def = AccessorDef { component_type, count: data.len(), kind: "SCALAR".into(), ..AccessorDef::default() };
        self.accessor(&bytes, Some(code::ELEMENT_ARRAY_BUFFER), def)
    }

    /// Morph target deltas as a `VEC3`, stored sparsely when that is smaller. A face target usually moves a fraction
    /// of the vertices: 52 dense targets over 30,000 vertices are 19 MB, sparse ones a fraction of it. An all-zero
    /// target needs no data at all: an accessor without a buffer view is zero-filled (glTF 2.0, "Accessors").
    /// `with_bounds` adds `min`/`max` over the full data, as for a dense accessor.
    ///
    /// The choice depends only on the data, so the same deltas always produce the same bytes.
    pub fn deltas(&mut self, data: &[Vec3], with_bounds: bool) -> usize {
        let nonzero: Vec<usize> = (0..data.len()).filter(|&i| data[i] != Vec3::ZERO).collect();
        let bounds = with_bounds.then(|| bounds(data));
        let (min, max) = bounds.clone().unzip();
        let (index_type, index_size) = sparse_index_type(data.len());
        if !is_sparse_smaller(nonzero.len(), index_size, data.len()) {
            return self.float_accessor(&le_f32(data.iter().flat_map(Vec3::to_array)), data.len(), "VEC3", bounds);
        }
        let mut def = AccessorDef {
            component_type: code::F32,
            count: data.len(),
            kind: "VEC3".into(),
            min,
            max,
            ..AccessorDef::default()
        };
        if !nonzero.is_empty() {
            let index_bytes: Vec<u8> = nonzero.iter().flat_map(|&i| index_bytes(i, index_type)).collect();
            let value_bytes = le_f32(nonzero.iter().flat_map(|&i| data[i].to_array()));
            let indices = self.view(&index_bytes, None);
            let values = self.view(&value_bytes, None);
            def.sparse = Some(Sparse {
                count: nonzero.len(),
                indices: SparseIndices { buffer_view: indices, component_type: index_type, ..SparseIndices::default() },
                values: SparseValues { buffer_view: values, ..SparseValues::default() },
                ..Sparse::default()
            });
        }
        self.accessors.push(def);
        self.accessors.len() - 1
    }

    /// A skin's inverse bind matrices. Not vertex data, so the view has no `target` (the spec reserves targets for
    /// vertex attributes and indices, and validators flag a view used both ways).
    pub fn matrices(&mut self, data: &[Mat4]) -> usize {
        let bytes = le_f32(data.iter().flat_map(Mat4::to_cols_array));
        let def =
            AccessorDef { component_type: code::F32, count: data.len(), kind: "MAT4".into(), ..AccessorDef::default() };
        self.accessor(&bytes, None, def)
    }

    /// A named per-vertex attribute (`_STRAND_UV`): floats, one to four per vertex.
    pub fn extra(&mut self, attr: &ExtraAttribute) -> usize {
        let components = usize::from(attr.components).max(1);
        let kind = match components {
            1 => "SCALAR",
            2 => "VEC2",
            3 => "VEC3",
            _ => "VEC4",
        };
        let bytes = le_f32(attr.data.iter().copied());
        self.float_accessor(&bytes, attr.data.len() / components, kind, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn views_start_on_four_byte_boundaries_and_positions_get_bounds() {
        let mut p = Packer::default();
        p.view(&[1, 2, 3], None);
        let a = p.vec3(&[Vec3::new(-1.0, 2.0, 0.5), Vec3::new(3.0, -4.0, 0.0)], true);
        assert_eq!(p.views[1].byte_offset, 4, "padded after 3 bytes");
        assert_eq!(p.accessors[a].min.as_deref(), Some(&[-1.0, -4.0, 0.0][..]));
        assert_eq!(p.accessors[a].max.as_deref(), Some(&[3.0, 2.0, 0.5][..]));
        assert_eq!(p.accessors[a].count, 2);
    }

    #[test]
    fn small_deltas_stay_dense_and_mostly_zero_ones_go_sparse() {
        let mut p = Packer::default();
        let few: Vec<Vec3> = (1..=8u8).map(|i| Vec3::splat(f32::from(i))).collect();
        let dense = p.deltas(&few, true);
        assert!(p.accessors[dense].sparse.is_none() && p.accessors[dense].buffer_view.is_some());

        // 1000 vertices, three of them move: sparse, with u16 indices and bounds over the whole target.
        let mut mostly_zero = vec![Vec3::ZERO; 1000];
        mostly_zero[3] = Vec3::new(1.0, -2.0, 0.5);
        mostly_zero[999] = Vec3::new(-1.0, 0.0, 3.0);
        mostly_zero[500] = Vec3::Y;
        let sparse = p.deltas(&mostly_zero, true);
        let a = &p.accessors[sparse];
        let s = a.sparse.as_ref().expect("sparse");
        assert_eq!((a.buffer_view, a.count, s.count, s.indices.component_type), (None, 1000, 3, code::U16));
        assert_eq!(a.min.as_deref(), Some(&[-1.0, -2.0, 0.0][..]));
        assert_eq!(a.max.as_deref(), Some(&[1.0, 1.0, 3.0][..]));
        assert_eq!((p.views[s.indices.buffer_view].target, p.views[s.values.buffer_view].target), (None, None));
        assert_eq!(p.views[s.values.buffer_view].byte_length, 36);
    }

    #[test]
    fn an_all_zero_target_is_an_accessor_without_data() {
        let mut p = Packer::default();
        let zero = p.deltas(&[Vec3::ZERO; 5], false);
        let a = &p.accessors[zero];
        assert_eq!((a.buffer_view, a.sparse.is_none(), a.count), (None, true, 5));
        assert!(p.views.is_empty() && p.bin.is_empty());
    }

    #[test]
    fn sparse_index_widths_follow_the_element_count() {
        assert_eq!(sparse_index_type(256), (code::U8, 1));
        assert_eq!(sparse_index_type(257), (code::U16, 2));
        assert_eq!(sparse_index_type(65_537), (code::U32, 4));
        assert_eq!(index_bytes(513, code::U16), [1, 2]);
        assert_eq!(index_bytes(70_000, code::U32), 70_000u32.to_le_bytes());
    }

    #[test]
    fn inverse_bind_matrices_have_a_view_without_a_target() {
        let mut p = Packer::default();
        p.matrices(&[Mat4::IDENTITY]);
        p.vec3(&[Vec3::ZERO], false);
        assert_eq!((p.views[0].target, p.views[1].target), (None, Some(code::ARRAY_BUFFER)));
    }

    #[test]
    fn indices_narrow_only_when_every_index_fits_below_the_reserved_value() {
        let mut p = Packer::default();
        let small = p.indices(&[0, 1, 2], 3);
        assert_eq!((p.accessors[small].component_type, p.views[0].byte_length), (code::U16, 6));
        let edge = p.indices(&[0, 1, 2], 65_535);
        assert_eq!(p.accessors[edge].component_type, code::U16);
        let big = p.indices(&[0, 1, 70_000], 65_536);
        assert_eq!(p.accessors[big].component_type, code::U32);
    }
}
