//! Accessors decoded into typed arrays (glTF 2.0, "Accessors", "Sparse Accessors", "Buffers and Buffer Views").
//!
//! Handled here so no caller has to think about them: byte strides (interleaved vertex data), every component type
//! with and without normalisation (`KHR_mesh_quantization` needs the integer forms), matrix column padding, accessors
//! with no buffer view (all zeros) and sparse overlays.
//!
//! **Safe on hostile input.** An accessor is checked against its buffer view and buffer with overflow-checked
//! arithmetic, and its declared count against the caller's [`Bound`], *before* anything is allocated. The readers
//! slice with `get` and never index, so a lie in the file can only produce an error.

use omuzaki_io_api::IoError;

use crate::buffers::Buffers;
use crate::schema::{Accessor as AccessorDef, Document, Sparse as SparseDef, code};

/// An accessor's element type (`accessor.type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// One value.
    Scalar,
    /// Two values.
    Vec2,
    /// Three values.
    Vec3,
    /// Four values.
    Vec4,
    /// A 2x2 matrix.
    Mat2,
    /// A 3x3 matrix.
    Mat3,
    /// A 4x4 matrix.
    Mat4,
}

impl Kind {
    fn parse(text: &str) -> Option<Self> {
        Some(match text {
            "SCALAR" => Self::Scalar,
            "VEC2" => Self::Vec2,
            "VEC3" => Self::Vec3,
            "VEC4" => Self::Vec4,
            "MAT2" => Self::Mat2,
            "MAT3" => Self::Mat3,
            "MAT4" => Self::Mat4,
            _ => return None,
        })
    }

    /// The kind that has `n` components, for the typed readers whose output width is a const generic.
    fn of_components(n: usize) -> Option<Self> {
        Some(match n {
            1 => Self::Scalar,
            2 => Self::Vec2,
            3 => Self::Vec3,
            4 => Self::Vec4,
            16 => Self::Mat4,
            _ => return None,
        })
    }

    fn components(self) -> usize {
        match self {
            Self::Scalar => 1,
            Self::Vec2 => 2,
            Self::Vec3 => 3,
            Self::Vec4 | Self::Mat2 => 4,
            Self::Mat3 => 9,
            Self::Mat4 => 16,
        }
    }

    fn columns(self) -> usize {
        match self {
            Self::Mat2 => 2,
            Self::Mat3 => 3,
            Self::Mat4 => 4,
            _ => 1,
        }
    }

    fn is_matrix(self) -> bool {
        self.columns() > 1
    }
}

/// An accessor's component type (`accessor.componentType`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Component {
    /// Signed byte.
    I8,
    /// Unsigned byte.
    U8,
    /// Signed short.
    I16,
    /// Unsigned short.
    U16,
    /// Unsigned int.
    U32,
    /// 32-bit float.
    F32,
}

impl Component {
    fn from_code(value: u32) -> Option<Self> {
        Some(match value {
            code::I8 => Self::I8,
            code::U8 => Self::U8,
            code::I16 => Self::I16,
            code::U16 => Self::U16,
            code::U32 => Self::U32,
            code::F32 => Self::F32,
            _ => return None,
        })
    }

    fn size(self) -> usize {
        match self {
            Self::I8 | Self::U8 => 1,
            Self::I16 | Self::U16 => 2,
            Self::U32 | Self::F32 => 4,
        }
    }
}

/// What a caller will accept as the number of elements; checked before the data is allocated.
#[derive(Clone, Copy, Debug)]
pub enum Bound {
    /// Exactly this many: the accessor must match its primitive's vertex count.
    Exactly(usize),
    /// At most this many, else the named limit is exceeded.
    AtMost {
        /// The most elements allowed.
        max: usize,
        /// The limit's name, for the error.
        what: &'static str,
    },
}

/// Which component types a reader takes, and how it treats integers.
#[derive(Clone, Copy, Debug)]
pub enum Flavor {
    /// Floats, or integers as the file's `normalized` flag says (positions, normals, UVs, colours; quantised forms
    /// need the integers).
    Attribute,
    /// Floats, or integers that are always normalised, because the spec says weights are and some files forget the flag.
    Weight,
    /// Floats only (inverse bind matrices).
    Matrix,
}

impl Flavor {
    fn allowed(self) -> &'static [Component] {
        match self {
            Self::Attribute => &[Component::F32, Component::U8, Component::I8, Component::U16, Component::I16],
            Self::Weight => &[Component::F32, Component::U8, Component::U16],
            Self::Matrix => &[Component::F32],
        }
    }

    fn reader(self) -> Reader<f32> {
        match self {
            Self::Weight => read_weight,
            Self::Attribute | Self::Matrix => read_float,
        }
    }
}

type Reader<T> = fn(Component, &[u8], bool) -> T;

fn bytes<const N: usize>(b: &[u8]) -> [u8; N] {
    b.get(..N).and_then(|s| <[u8; N]>::try_from(s).ok()).unwrap_or([0; N])
}

/// One component as `f32`. The formulas are the spec's table for normalised integers (`max(c / 127, -1)` for signed).
#[allow(clippy::cast_precision_loss)] // a non-normalised u32 asked to be a float; exactness beyond 2^24 isn't promised
fn read_float(c: Component, b: &[u8], normalized: bool) -> f32 {
    let scaled = |v: f32, max: f32| if normalized { (v / max).max(-1.0) } else { v };
    match c {
        Component::F32 => f32::from_le_bytes(bytes(b)),
        Component::U8 => scaled(f32::from(bytes::<1>(b)[0]), 255.0),
        Component::I8 => scaled(f32::from(i8::from_le_bytes(bytes(b))), 127.0),
        Component::U16 => scaled(f32::from(u16::from_le_bytes(bytes(b))), 65535.0),
        Component::I16 => scaled(f32::from(i16::from_le_bytes(bytes(b))), 32767.0),
        Component::U32 => u32::from_le_bytes(bytes(b)) as f32,
    }
}

fn read_weight(c: Component, b: &[u8], _: bool) -> f32 {
    read_float(c, b, true)
}

/// An unsigned integer component. Other types are rejected before any reader runs, so they decode to 0 here.
fn read_uint(c: Component, b: &[u8], _: bool) -> u32 {
    match c {
        Component::U8 => u32::from(bytes::<1>(b)[0]),
        Component::U16 => u32::from(u16::from_le_bytes(bytes(b))),
        Component::U32 => u32::from_le_bytes(bytes(b)),
        Component::I8 | Component::I16 | Component::F32 => 0,
    }
}

fn read_joint(c: Component, b: &[u8], _: bool) -> u16 {
    u16::try_from(read_uint(c, b, false)).unwrap_or(0)
}

/// Where an accessor's elements sit: its type, component, count and normalisation.
#[derive(Clone, Copy)]
struct Layout {
    kind: Kind,
    comp: Component,
    normalized: bool,
    count: usize,
}

impl Layout {
    /// Bytes of one column. Matrix columns are padded to 4 bytes (spec, "Matrix types"); vectors are packed.
    fn column_bytes(&self) -> usize {
        let raw = self.kind.components() / self.kind.columns() * self.comp.size();
        if self.kind.is_matrix() { raw.next_multiple_of(4) } else { raw }
    }

    fn element_size(&self) -> usize {
        self.kind.columns() * self.column_bytes()
    }

    /// Byte offset of component `k` (column-major) inside an element.
    fn offset(&self, k: usize) -> usize {
        let rows = self.kind.components() / self.kind.columns();
        (k / rows) * self.column_bytes() + (k % rows) * self.comp.size()
    }
}

/// A run of elements inside a buffer view: `stride` bytes apart, the first at `bytes[0]`.
struct Region<'a> {
    bytes: &'a [u8],
    stride: usize,
}

impl<'a> Region<'a> {
    /// Element `i`, from its first byte to the end of the region (the layout knows where its components stop).
    fn element(&self, i: usize) -> &'a [u8] {
        i.checked_mul(self.stride).and_then(|at| self.bytes.get(at..)).unwrap_or(&[])
    }
}

/// Decodes accessors of one document. Cheap to copy.
#[derive(Clone, Copy)]
pub struct Accessors<'a> {
    doc: &'a Document,
    buffers: &'a Buffers<'a>,
}

fn malformed(message: String, ix: usize) -> IoError {
    IoError::Malformed { message, at: Some(format!("/accessors/{ix}")) }
}

impl<'a> Accessors<'a> {
    /// Readers for the accessors of `doc`, whose data is in `buffers`.
    pub fn new(doc: &'a Document, buffers: &'a Buffers<'a>) -> Self {
        Self { doc, buffers }
    }

    fn def(&self, ix: usize) -> Result<&'a AccessorDef, IoError> {
        self.doc.accessors.get(ix).ok_or_else(|| malformed("this accessor doesn't exist".into(), ix))
    }

    /// The accessor's declared element count.
    ///
    /// # Errors
    /// [`IoError::Malformed`] if the accessor doesn't exist.
    pub fn count(&self, ix: usize) -> Result<usize, IoError> {
        Ok(self.def(ix)?.count)
    }

    /// The accessor's declared element type.
    ///
    /// # Errors
    /// [`IoError::Malformed`] if the accessor doesn't exist or its type is unknown.
    pub fn kind(&self, ix: usize) -> Result<Kind, IoError> {
        let def = self.def(ix)?;
        Kind::parse(&def.kind).ok_or_else(|| malformed(format!("unknown accessor type {:?}", def.kind), ix))
    }

    fn layout(&self, ix: usize, kind: Kind, allowed: &[Component], bound: Bound) -> Result<Layout, IoError> {
        let def = self.def(ix)?;
        let found = self.kind(ix)?;
        if found != kind {
            return Err(malformed(format!("expected {kind:?} elements, found {found:?}"), ix));
        }
        let comp = Component::from_code(def.component_type)
            .filter(|c| allowed.contains(c))
            .ok_or_else(|| malformed(format!("component type {} isn't allowed here", def.component_type), ix))?;
        match bound {
            Bound::Exactly(n) if def.count != n => {
                return Err(malformed(format!("{} elements, but the primitive has {n} vertices", def.count), ix));
            }
            Bound::AtMost { max, what } if def.count > max => {
                return Err(IoError::Limit { what, limit: max as u64, got: def.count as u64 });
            }
            _ => {}
        }
        Ok(Layout { kind, comp, normalized: def.normalized, count: def.count })
    }

    /// The bytes of buffer view `view`, checked against its buffer.
    ///
    /// # Errors
    /// [`IoError::Malformed`] if the view or its buffer doesn't exist, or the view runs past the buffer.
    pub fn view_bytes(&self, view: usize) -> Result<&'a [u8], IoError> {
        let bad = |message: String| IoError::Malformed { message, at: Some(format!("/bufferViews/{view}")) };
        let def = self.doc.buffer_views.get(view).ok_or_else(|| bad("this bufferView doesn't exist".into()))?;
        let buffer = self.buffers.get(def.buffer).ok_or_else(|| bad(format!("buffer {} doesn't exist", def.buffer)))?;
        def.byte_offset
            .checked_add(def.byte_length)
            .and_then(|end| buffer.get(def.byte_offset..end))
            .ok_or_else(|| bad("the view runs past its buffer".into()))
    }

    fn region(
        &self,
        view: usize,
        offset: usize,
        layout: &Layout,
        tight: bool,
        ix: usize,
    ) -> Result<Region<'a>, IoError> {
        let bad = |m: String| malformed(m, ix);
        let in_view = self.view_bytes(view)?;
        let def = self.doc.buffer_views.get(view).ok_or_else(|| bad(format!("bufferView {view} doesn't exist")))?;
        let element = layout.element_size();
        let stride = match def.byte_stride {
            Some(s) if !tight && s != 0 => s,
            _ => element,
        };
        if stride < element {
            return Err(bad(format!("byteStride {stride} is smaller than the {element}-byte element")));
        }
        let needed = match layout.count {
            0 => Some(0),
            n => (n - 1).checked_mul(stride).and_then(|bytes| bytes.checked_add(element)),
        };
        let bytes = needed
            .and_then(|n| offset.checked_add(n))
            .and_then(|end| in_view.get(offset..end))
            .ok_or_else(|| bad("the data runs past its bufferView".into()))?;
        Ok(Region { bytes, stride })
    }

    /// Decodes into `count` arrays of `N` values, applying the sparse overlay if there is one.
    fn decode<T: Copy + Default, const N: usize>(
        &self,
        ix: usize,
        kind: Kind,
        allowed: &[Component],
        bound: Bound,
        read: Reader<T>,
    ) -> Result<Vec<[T; N]>, IoError> {
        if kind.components() != N {
            return Err(IoError::Internal(format!("a reader for {N} values was asked to decode {kind:?}")));
        }
        let layout = self.layout(ix, kind, allowed, bound)?;
        let def = self.def(ix)?;
        let mut out = match def.buffer_view {
            Some(view) => {
                let region = self.region(view, def.byte_offset, &layout, false, ix)?;
                let offsets: [usize; N] = std::array::from_fn(|k| layout.offset(k));
                (0..layout.count).map(|i| read_element(&layout, &offsets, region.element(i), read)).collect()
            }
            None => vec![[T::default(); N]; layout.count],
        };
        if let Some(sparse) = &def.sparse {
            self.overlay(sparse, &layout, &mut out, read, ix)?;
        }
        Ok(out)
    }

    /// Replaces the elements the sparse block names (glTF 2.0, "Sparse Accessors").
    fn overlay<T: Copy + Default, const N: usize>(
        &self,
        sparse: &SparseDef,
        layout: &Layout,
        out: &mut [[T; N]],
        read: Reader<T>,
        ix: usize,
    ) -> Result<(), IoError> {
        if sparse.count == 0 || sparse.count > layout.count {
            return Err(malformed(format!("sparse count {} for {} elements", sparse.count, layout.count), ix));
        }
        let index_comp = Component::from_code(sparse.indices.component_type)
            .filter(|c| matches!(c, Component::U8 | Component::U16 | Component::U32))
            .ok_or_else(|| malformed("sparse indices must be unsigned 8, 16 or 32 bit".into(), ix))?;
        let index_layout = Layout { kind: Kind::Scalar, comp: index_comp, normalized: false, count: sparse.count };
        let indices = self.region(sparse.indices.buffer_view, sparse.indices.byte_offset, &index_layout, true, ix)?;
        let value_layout = Layout { count: sparse.count, ..*layout };
        let values = self.region(sparse.values.buffer_view, sparse.values.byte_offset, &value_layout, true, ix)?;
        let offsets: [usize; N] = std::array::from_fn(|k| layout.offset(k));
        for k in 0..sparse.count {
            let target = usize::try_from(read_uint(index_comp, indices.element(k), false)).unwrap_or(usize::MAX);
            let slot = out
                .get_mut(target)
                .ok_or_else(|| malformed(format!("a sparse index ({target}) is past the accessor's end"), ix))?;
            *slot = read_element(layout, &offsets, values.element(k), read);
        }
        Ok(())
    }

    /// Float elements of `N` components (`N` picks the accessor type: 1 `SCALAR`, 2 to 4 vectors, 16 `MAT4`).
    ///
    /// # Errors
    /// [`IoError::Malformed`] for an accessor that doesn't exist, has another type or an unusable component type, or
    /// whose data runs past its view; [`IoError::Limit`] when the count is over `bound`.
    pub fn floats<const N: usize>(&self, ix: usize, bound: Bound, flavor: Flavor) -> Result<Vec<[f32; N]>, IoError> {
        let kind =
            Kind::of_components(N).ok_or_else(|| IoError::Internal(format!("no accessor type has {N} components")))?;
        self.decode(ix, kind, flavor.allowed(), bound, flavor.reader())
    }

    /// Vertex indices, widened to `u32`.
    ///
    /// # Errors
    /// As [`Accessors::floats`].
    pub fn indices(&self, ix: usize, bound: Bound) -> Result<Vec<u32>, IoError> {
        let allowed = [Component::U8, Component::U16, Component::U32];
        let one = self.decode::<u32, 1>(ix, Kind::Scalar, &allowed, bound, read_uint)?;
        Ok(one.into_iter().map(|[v]| v).collect())
    }

    /// `JOINTS_n` values: unsigned 8 or 16 bit.
    ///
    /// # Errors
    /// As [`Accessors::floats`].
    pub fn joints(&self, ix: usize, bound: Bound) -> Result<Vec<[u16; 4]>, IoError> {
        self.decode(ix, Kind::Vec4, &[Component::U8, Component::U16], bound, read_joint)
    }
}

impl Accessors<'_> {
    /// A colour attribute (`COLOR_n`) as RGBA: `VEC3` accessors get an alpha of 1, whatever the component type.
    ///
    /// # Errors
    /// As [`Accessors::floats`].
    pub fn colours(&self, ix: usize, bound: Bound) -> Result<Vec<[f32; 4]>, IoError> {
        Ok(match self.kind(ix)? {
            Kind::Vec3 => {
                self.floats::<3>(ix, bound, Flavor::Attribute)?.into_iter().map(|[r, g, b]| [r, g, b, 1.0]).collect()
            }
            _ => self.floats::<4>(ix, bound, Flavor::Attribute)?,
        })
    }

    /// A float scalar or vector accessor of any width as `(components, flat values)`, for attributes whose width is
    /// the file's choice (application-specific `_NAME` attributes). `None` for matrix accessors.
    ///
    /// # Errors
    /// As [`Accessors::floats`].
    pub fn flat_floats(&self, ix: usize, bound: Bound) -> Result<Option<(u8, Vec<f32>)>, IoError> {
        fn flatten<const N: usize>(values: Vec<[f32; N]>) -> Vec<f32> {
            values.into_iter().flatten().collect()
        }
        Ok(Some(match self.kind(ix)? {
            Kind::Scalar => (1, flatten(self.floats::<1>(ix, bound, Flavor::Attribute)?)),
            Kind::Vec2 => (2, flatten(self.floats::<2>(ix, bound, Flavor::Attribute)?)),
            Kind::Vec3 => (3, flatten(self.floats::<3>(ix, bound, Flavor::Attribute)?)),
            Kind::Vec4 => (4, flatten(self.floats::<4>(ix, bound, Flavor::Attribute)?)),
            Kind::Mat2 | Kind::Mat3 | Kind::Mat4 => return Ok(None),
        }))
    }
}

fn read_element<T: Copy + Default, const N: usize>(
    layout: &Layout,
    offsets: &[usize; N],
    element: &[u8],
    read: Reader<T>,
) -> [T; N] {
    let mut out = [T::default(); N];
    for (slot, &at) in out.iter_mut().zip(offsets) {
        *slot = read(layout.comp, element.get(at..).unwrap_or(&[]), layout.normalized);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{Buffer as BufferDef, BufferView as BufferViewDef, SparseIndices, SparseValues};

    /// A document with one buffer of `data`, one view over `[view_offset, +view_len)` with an optional stride.
    fn setup(data: &[u8], view_offset: usize, view_len: usize, stride: Option<usize>) -> (Document, Vec<u8>) {
        let doc = Document {
            buffers: vec![BufferDef { byte_length: data.len(), ..BufferDef::default() }],
            buffer_views: vec![BufferViewDef {
                byte_offset: view_offset,
                byte_length: view_len,
                byte_stride: stride,
                ..BufferViewDef::default()
            }],
            ..Document::default()
        };
        (doc, data.to_vec())
    }

    fn accessor(component_type: u32, count: usize, kind: &str) -> AccessorDef {
        AccessorDef { buffer_view: Some(0), component_type, count, kind: kind.into(), ..AccessorDef::default() }
    }

    fn with<R>(doc: &mut Document, data: &[u8], def: AccessorDef, f: impl FnOnce(Accessors<'_>) -> R) -> R {
        doc.accessors = vec![def];
        let mut side = crate::buffers::Loader::new(&omuzaki_io_api::NoResolver, 1 << 20);
        let buffers = Buffers::load(doc, Some(data), &mut side).unwrap();
        f(Accessors::new(doc, &buffers))
    }

    fn le_f32(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    const ANY: Bound = Bound::AtMost { max: 1000, what: "test" };

    #[test]
    fn reads_packed_floats_and_honours_the_view_and_accessor_offsets() {
        let data = le_f32(&[9.0, 9.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        let (mut doc, data) = setup(&data, 8, 24, None);
        let got = with(&mut doc, &data, accessor(code::F32, 2, "VEC3"), |a| a.floats::<3>(0, ANY, Flavor::Attribute));
        assert_eq!(got.unwrap(), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
    }

    #[test]
    fn reads_interleaved_data_by_stride() {
        // Two vertices of (position vec3, uv vec2) interleaved: stride 20; read the positions, then the uvs.
        let data = le_f32(&[1.0, 2.0, 3.0, 0.5, 0.25, 4.0, 5.0, 6.0, 0.75, 1.0]);
        let (mut doc, data) = setup(&data, 0, 40, Some(20));
        let pos = with(&mut doc, &data, accessor(code::F32, 2, "VEC3"), |a| a.floats::<3>(0, ANY, Flavor::Attribute));
        assert_eq!(pos.unwrap(), [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]);
        let mut uv = accessor(code::F32, 2, "VEC2");
        uv.byte_offset = 12;
        let uv = with(&mut doc, &data, uv, |a| a.floats::<2>(0, ANY, Flavor::Attribute));
        assert_eq!(uv.unwrap(), [[0.5, 0.25], [0.75, 1.0]]);
    }

    #[test]
    fn normalised_integers_follow_the_spec_table() {
        let (mut doc, data) = setup(&[0, 255, 128, 0], 0, 4, None);
        let mut def = accessor(code::U8, 2, "VEC2");
        def.normalized = true;
        let got = with(&mut doc, &data, def, |a| a.floats::<2>(0, ANY, Flavor::Attribute)).unwrap();
        assert_eq!(got[0], [0.0, 1.0]);
        assert!((got[1][0] - 128.0 / 255.0).abs() < 1e-7);

        let signed = [0x80u8, 0x81, 0x7F, 0x00]; // i8: -128, -127, 127, 0
        let (mut doc, data) = setup(&signed, 0, 4, None);
        let mut def = accessor(code::I8, 1, "VEC4");
        def.normalized = true;
        let got = with(&mut doc, &data, def, |a| a.floats::<4>(0, ANY, Flavor::Attribute)).unwrap();
        assert_eq!(got[0], [-1.0, -1.0, 1.0, 0.0], "-128 clamps to -1");

        let (mut doc, data) = setup(&[2, 0], 0, 2, None);
        let got = with(&mut doc, &data, accessor(code::U16, 1, "SCALAR"), |a| a.floats::<1>(0, ANY, Flavor::Attribute));
        assert_eq!(got.unwrap(), [[2.0]], "not normalised: the raw integer");
    }

    #[test]
    fn weights_are_normalised_even_when_the_file_forgets_the_flag() {
        let (mut doc, data) = setup(&[255, 0, 0, 0], 0, 4, None);
        let got = with(&mut doc, &data, accessor(code::U8, 1, "VEC4"), |a| a.floats::<4>(0, ANY, Flavor::Weight));
        assert_eq!(got.unwrap(), [[1.0, 0.0, 0.0, 0.0]]);
    }

    #[test]
    fn matrix_columns_are_padded_to_four_bytes() {
        // MAT3 of unsigned bytes: three columns of 3 bytes, each followed by a pad byte, 12 bytes in all.
        let layout = Layout { kind: Kind::Mat3, comp: Component::U8, normalized: false, count: 1 };
        assert_eq!(layout.element_size(), 12);
        assert_eq!((0..9).map(|k| layout.offset(k)).collect::<Vec<_>>(), [0, 1, 2, 4, 5, 6, 8, 9, 10]);
        let short = Layout { kind: Kind::Mat2, comp: Component::U8, ..layout };
        assert_eq!((short.element_size(), short.offset(2)), (8, 4), "MAT2 of bytes: 2-byte columns padded to 4");
        let wide = Layout { kind: Kind::Mat3, comp: Component::U16, ..layout };
        assert_eq!((wide.element_size(), wide.offset(3)), (24, 8), "MAT3 of shorts: 6-byte columns padded to 8");
        // MAT4 of floats has no padding, and reads column-major.
        let ident: Vec<f32> = (0..16).map(|i| if i % 5 == 0 { 1.0 } else { 0.0 }).collect();
        let (mut doc, data) = setup(&le_f32(&ident), 0, 64, None);
        let got = with(&mut doc, &data, accessor(code::F32, 1, "MAT4"), |a| a.floats::<16>(0, ANY, Flavor::Matrix));
        assert_eq!(got.unwrap()[0].to_vec(), ident);
    }

    #[test]
    fn indices_widen_from_every_integer_size() {
        for (comp, bytes, want) in [
            (code::U8, vec![1u8, 2, 3], vec![1u32, 2, 3]),
            (code::U16, vec![1, 0, 2, 1, 3, 0], vec![1, 258, 3]),
            (code::U32, vec![1, 0, 0, 0, 0, 0, 1, 0, 3, 0, 0, 0], vec![1, 65536, 3]),
        ] {
            let (mut doc, data) = setup(&bytes, 0, bytes.len(), None);
            let got = with(&mut doc, &data, accessor(comp, 3, "SCALAR"), |a| a.indices(0, ANY));
            assert_eq!(got.unwrap(), want);
        }
    }

    #[test]
    fn sparse_accessors_overlay_a_base_or_zeros() {
        // Base: 4 scalars [1, 2, 3, 4]. Sparse: elements 1 and 3 become 20 and 40.
        let mut data = le_f32(&[1.0, 2.0, 3.0, 4.0]);
        data.extend_from_slice(&[1, 3, 0, 0]); // u8 indices at 16 (padded)
        data.extend_from_slice(&le_f32(&[20.0, 40.0])); // values at 20
        let mut doc = Document {
            buffers: vec![BufferDef { byte_length: data.len(), ..BufferDef::default() }],
            buffer_views: [(0, 16), (16, 4), (20, 8)]
                .map(|(byte_offset, byte_length)| BufferViewDef {
                    byte_offset,
                    byte_length,
                    ..BufferViewDef::default()
                })
                .into(),
            ..Document::default()
        };
        let mut def = accessor(code::F32, 4, "SCALAR");
        def.sparse = Some(SparseDef {
            count: 2,
            indices: SparseIndices { buffer_view: 1, component_type: code::U8, ..SparseIndices::default() },
            values: SparseValues { buffer_view: 2, ..SparseValues::default() },
            ..SparseDef::default()
        });
        let got = with(&mut doc, &data, def, |a| a.floats::<1>(0, ANY, Flavor::Attribute));
        assert_eq!(got.unwrap(), [[1.0], [20.0], [3.0], [40.0]]);

        let mut zeros = accessor(code::F32, 4, "SCALAR");
        zeros.buffer_view = None;
        zeros.sparse = Some(SparseDef {
            count: 1,
            indices: SparseIndices {
                buffer_view: 1,
                byte_offset: 1,
                component_type: code::U8,
                ..SparseIndices::default()
            },
            values: SparseValues { buffer_view: 2, ..SparseValues::default() },
            ..SparseDef::default()
        });
        let got = with(&mut doc, &data, zeros, |a| a.floats::<1>(0, ANY, Flavor::Attribute));
        assert_eq!(got.unwrap(), [[0.0], [0.0], [0.0], [20.0]], "no bufferView means zeros, then the overlay");
    }

    #[test]
    fn lies_are_errors_not_panics() {
        let data = le_f32(&[1.0, 2.0, 3.0]);
        let (mut doc, data) = setup(&data, 0, 12, None);
        let cases = [
            (accessor(code::F32, 2, "VEC3"), "runs past"),
            (accessor(code::F32, usize::MAX, "VEC3"), "runs past"), // the size arithmetic overflows
            (accessor(code::F32, 1, "VEC2"), "expected"),
            (accessor(code::F32, 1, "WAT"), "unknown accessor type"),
            (accessor(1234, 1, "VEC3"), "isn't allowed"),
            (AccessorDef { buffer_view: Some(7), ..accessor(code::F32, 1, "VEC3") }, "bufferView doesn't exist"),
            (AccessorDef { byte_offset: usize::MAX, ..accessor(code::F32, 1, "VEC3") }, "runs past"),
        ];
        let anything = Bound::AtMost { max: usize::MAX, what: "test" };
        for (def, expect) in cases {
            let err = with(&mut doc, &data, def, |a| a.floats::<3>(0, anything, Flavor::Attribute)).unwrap_err();
            assert!(
                matches!(err, IoError::Malformed { ref message, .. } if message.contains(expect)),
                "{expect}: {err:?}"
            );
        }
        assert!(matches!(
            with(&mut doc, &data, accessor(code::F32, 1, "VEC3"), |a| a.floats::<3>(5, ANY, Flavor::Attribute)),
            Err(IoError::Malformed { .. })
        ));
    }

    #[test]
    fn the_bound_is_checked_before_any_data_is_read() {
        let (mut doc, data) = setup(&[0; 12], 0, 12, None);
        // A count of four billion over a 12-byte buffer: the limit fires, nothing is allocated.
        let huge = accessor(code::F32, 4_000_000_000, "VEC3");
        let bound = Bound::AtMost { max: 10, what: "vertex count" };
        let err = with(&mut doc, &data, huge, |a| a.floats::<3>(0, bound, Flavor::Attribute)).unwrap_err();
        assert!(matches!(err, IoError::Limit { what: "vertex count", limit: 10, .. }));
        let err = with(&mut doc, &data, accessor(code::F32, 1, "VEC3"), |a| {
            a.floats::<3>(0, Bound::Exactly(2), Flavor::Attribute)
        })
        .unwrap_err();
        assert!(matches!(err, IoError::Malformed { .. }));
    }

    #[test]
    fn a_stride_smaller_than_the_element_is_rejected() {
        let (mut doc, data) = setup(&[0; 24], 0, 24, Some(8));
        let err = with(&mut doc, &data, accessor(code::F32, 2, "VEC3"), |a| a.floats::<3>(0, ANY, Flavor::Attribute));
        assert!(matches!(err, Err(IoError::Malformed { .. })));
    }
}
