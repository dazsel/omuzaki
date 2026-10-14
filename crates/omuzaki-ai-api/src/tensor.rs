//! Tensors: the specs a slot's signature is made of, the borrowed inputs a caller passes, and the reusable output
//! buffers a session fills.
//!
//! Inputs are borrowed ([`TensorRef`]) and outputs are caller-owned buffers ([`TensorBuf`]) that keep their capacity
//! between runs, so a session called every frame allocates nothing once its buffers have grown to size.

use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The most dimensions a tensor may have. ONNX models in practice use at most five.
pub const MAX_RANK: usize = 8;

/// An element type.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum DType {
    /// 32-bit float.
    F32,
    /// 16-bit float, carried as its raw bits (`u16`).
    F16,
    /// 64-bit signed integer.
    I64,
    /// 32-bit signed integer.
    I32,
    /// Unsigned byte; also UTF-8 text (one tensor of bytes).
    U8,
}

/// One dimension of a [`TensorSpec`]: a fixed size, or any size (chosen per run).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Dim {
    /// Exactly this many.
    Fixed(usize),
    /// Any size, written `"any"`.
    Any(AnyDim),
}

/// The marker for [`Dim::Any`], which serialises as the string `"any"`.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnyDim {
    /// Any size.
    #[default]
    Any,
}

impl Dim {
    /// Any size.
    pub const ANY: Dim = Dim::Any(AnyDim::Any);
}

impl fmt::Display for Dim {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Dim::Fixed(n) => write!(f, "{n}"),
            Dim::Any(_) => f.write_str("any"),
        }
    }
}

/// A named tensor in a signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TensorSpec {
    /// The name: the slot's own for a slot signature, the graph's for a probed model.
    pub name: String,
    /// The element type.
    pub dtype: DType,
    /// The dimensions; an empty list is a scalar.
    pub shape: Vec<Dim>,
}

impl TensorSpec {
    /// A spec from its parts.
    pub fn new(name: impl Into<String>, dtype: DType, shape: impl Into<Vec<Dim>>) -> Self {
        Self { name: name.into(), dtype, shape: shape.into() }
    }
}

/// What goes in and what comes out, in order. A slot's signature is the fixed interface every model in it must
/// present; a runtime's probe reports a model's own signature, which [`Signature::accepts`] compares.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Signature {
    /// The inputs, in the order [`Session::run`](crate::Session::run) takes them.
    pub inputs: Vec<TensorSpec>,
    /// The outputs, in the order `run` fills them.
    pub outputs: Vec<TensorSpec>,
}

/// One way a model's signature fails to fit a slot's. Shown to people as-is, so each says what is wrong.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Mismatch {
    /// The slot feeds an input the model doesn't have.
    #[error("the model has no input `{tensor}`")]
    MissingInput {
        /// The slot's name for it.
        tensor: String,
    },
    /// The model needs an input the slot never feeds.
    #[error("the model needs an input `{tensor}` that this slot doesn't provide")]
    ExtraInput {
        /// The model's name for it.
        tensor: String,
    },
    /// The slot reads an output the model doesn't produce.
    #[error("the model has no output `{tensor}`")]
    MissingOutput {
        /// The slot's name for it.
        tensor: String,
    },
    /// The element types differ.
    #[error("`{tensor}` is {found:?}, the slot needs {expected:?}")]
    DType {
        /// The slot's name for it.
        tensor: String,
        /// What the slot needs.
        expected: DType,
        /// What the model has.
        found: DType,
    },
    /// The numbers of dimensions differ.
    #[error("`{tensor}` has {found} dimensions, the slot needs {expected}")]
    Rank {
        /// The slot's name for it.
        tensor: String,
        /// What the slot needs.
        expected: usize,
        /// What the model has.
        found: usize,
    },
    /// A dimension can't take the sizes the slot uses.
    #[error("dimension {axis} of `{tensor}` is {found}, the slot needs {expected}")]
    Dim {
        /// The slot's name for it.
        tensor: String,
        /// Which dimension, from 0.
        axis: usize,
        /// What the slot needs.
        expected: Dim,
        /// What the model has.
        found: Dim,
    },
}

/// Whether a model whose port has `found` can serve a slot port of `expected`. For an input the slot chooses the size,
/// so a fixed model dimension can't take a slot's "any". For an output the model chooses, and a model's "any" is
/// checked on every run instead.
fn dim_fits(expected: Dim, found: Dim, input: bool) -> bool {
    match (expected, found) {
        (Dim::Fixed(a), Dim::Fixed(b)) => a == b,
        (Dim::Fixed(_) | Dim::Any(_), Dim::Any(_)) => true,
        (Dim::Any(_), Dim::Fixed(_)) => !input,
    }
}

fn compare(slot: &TensorSpec, model: &TensorSpec, input: bool, out: &mut Vec<Mismatch>) {
    let tensor = || slot.name.clone();
    if slot.dtype != model.dtype {
        out.push(Mismatch::DType { tensor: tensor(), expected: slot.dtype, found: model.dtype });
    }
    if slot.shape.len() != model.shape.len() {
        out.push(Mismatch::Rank { tensor: tensor(), expected: slot.shape.len(), found: model.shape.len() });
        return;
    }
    for (axis, (&expected, &found)) in slot.shape.iter().zip(&model.shape).enumerate() {
        if !dim_fits(expected, found, input) {
            out.push(Mismatch::Dim { tensor: tensor(), axis, expected, found });
        }
    }
}

impl Signature {
    /// This (model) signature with its ports renamed to the slot's names through a manifest's `io` table, which
    /// maps a slot port name to the model's own. Ports the table doesn't mention keep their names.
    #[must_use]
    pub fn with_slot_names(&self, io: &BTreeMap<String, String>) -> Signature {
        let rename = |spec: &TensorSpec| {
            let slot_name = io.iter().find(|(_, model)| **model == spec.name).map(|(slot, _)| slot.clone());
            TensorSpec { name: slot_name.unwrap_or_else(|| spec.name.clone()), ..spec.clone() }
        };
        Signature {
            inputs: self.inputs.iter().map(rename).collect(),
            outputs: self.outputs.iter().map(rename).collect(),
        }
    }

    /// Whether a model with signature `model` can serve this (slot) signature. Ports are matched **by name**, so
    /// export order doesn't matter; rename a model's ports with the manifest's `io` table before calling. Extra
    /// model outputs are ignored; extra model inputs are not, because nothing would feed them.
    ///
    /// # Errors
    /// Every mismatch found, so a person sees all of them at once.
    pub fn accepts(&self, model: &Signature) -> Result<(), Vec<Mismatch>> {
        let mut out = Vec::new();
        for slot in &self.inputs {
            match model.inputs.iter().find(|m| m.name == slot.name) {
                Some(m) => compare(slot, m, true, &mut out),
                None => out.push(Mismatch::MissingInput { tensor: slot.name.clone() }),
            }
        }
        for m in model.inputs.iter().filter(|m| !self.inputs.iter().any(|s| s.name == m.name)) {
            out.push(Mismatch::ExtraInput { tensor: m.name.clone() });
        }
        for slot in &self.outputs {
            match model.outputs.iter().find(|m| m.name == slot.name) {
                Some(m) => compare(slot, m, false, &mut out),
                None => out.push(Mismatch::MissingOutput { tensor: slot.name.clone() }),
            }
        }
        if out.is_empty() { Ok(()) } else { Err(out) }
    }

    /// Checks a run's inputs against this signature: count, element type, rank, fixed dimensions and data length.
    /// Every [`Session`](crate::Session) calls it first, so all of them reject a bad call the same way. Allocates
    /// nothing.
    ///
    /// # Errors
    /// [`RunError::Mismatch`] for the first input that is wrong.
    pub fn check_inputs(&self, inputs: &[TensorRef<'_>]) -> Result<(), RunError> {
        check_count(Port::Input, self.inputs.len(), inputs.len())?;
        for (index, (spec, tensor)) in self.inputs.iter().zip(inputs).enumerate() {
            check_tensor(spec, tensor.dtype(), tensor.shape, Some(tensor.data.len()))
                .map_err(|problem| RunError::Mismatch { port: Port::Input, index, problem })?;
        }
        Ok(())
    }

    /// Checks a run's output buffers: count and element type (a session sets their shapes). Allocates nothing.
    ///
    /// # Errors
    /// [`RunError::Mismatch`] for the first buffer that is wrong.
    pub fn check_outputs(&self, outputs: &[TensorBuf]) -> Result<(), RunError> {
        check_count(Port::Output, self.outputs.len(), outputs.len())?;
        for (index, (spec, buf)) in self.outputs.iter().zip(outputs).enumerate() {
            if spec.dtype != buf.dtype() {
                let problem = TensorProblem::DType { expected: spec.dtype, found: buf.dtype() };
                return Err(RunError::Mismatch { port: Port::Output, index, problem });
            }
        }
        Ok(())
    }
}

fn check_count(port: Port, expected: usize, found: usize) -> Result<(), RunError> {
    match found.cmp(&expected) {
        std::cmp::Ordering::Less => Err(RunError::Mismatch { port, index: found, problem: TensorProblem::Missing }),
        std::cmp::Ordering::Greater => Err(RunError::Mismatch { port, index: expected, problem: TensorProblem::Extra }),
        std::cmp::Ordering::Equal => Ok(()),
    }
}

/// Checks one tensor against its spec; `len` is the data length when there is data to check.
fn check_tensor(spec: &TensorSpec, dtype: DType, shape: &[usize], len: Option<usize>) -> Result<(), TensorProblem> {
    if spec.dtype != dtype {
        return Err(TensorProblem::DType { expected: spec.dtype, found: dtype });
    }
    if spec.shape.len() != shape.len() {
        return Err(TensorProblem::Rank { expected: spec.shape.len(), found: shape.len() });
    }
    for (axis, (&dim, &found)) in spec.shape.iter().zip(shape).enumerate() {
        if let Dim::Fixed(expected) = dim
            && expected != found
        {
            return Err(TensorProblem::Dim { axis, expected, found });
        }
    }
    let elements = element_count(shape)?;
    match len {
        Some(found) if found != elements => Err(TensorProblem::Length { expected: elements, found }),
        _ => Ok(()),
    }
}

/// The product of `shape`, refusing more than [`MAX_RANK`] dimensions or a product that overflows.
fn element_count(shape: &[usize]) -> Result<usize, TensorProblem> {
    if shape.len() > MAX_RANK {
        return Err(TensorProblem::TooLarge);
    }
    shape.iter().try_fold(1usize, |n, &d| n.checked_mul(d)).ok_or(TensorProblem::TooLarge)
}

/// Which side of a run a [`RunError::Mismatch`] is about.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Port {
    /// The inputs.
    Input,
    /// The outputs.
    Output,
}

impl fmt::Display for Port {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Port::Input => "input",
            Port::Output => "output",
        })
    }
}

/// What is wrong with one tensor of a run. `Copy`, so reporting it never allocates on the hot path.
#[derive(Copy, Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TensorProblem {
    /// Fewer tensors than the signature has; this index is the first missing one.
    #[error("missing")]
    Missing,
    /// More tensors than the signature has; this index is the first extra one.
    #[error("not in the signature")]
    Extra,
    /// Wrong element type.
    #[error("is {found:?}, expected {expected:?}")]
    DType {
        /// The signature's type.
        expected: DType,
        /// The tensor's.
        found: DType,
    },
    /// Wrong number of dimensions.
    #[error("has {found} dimensions, expected {expected}")]
    Rank {
        /// The signature's rank.
        expected: usize,
        /// The tensor's.
        found: usize,
    },
    /// A fixed dimension has another size.
    #[error("dimension {axis} is {found}, expected {expected}")]
    Dim {
        /// Which dimension.
        axis: usize,
        /// The signature's size.
        expected: usize,
        /// The tensor's.
        found: usize,
    },
    /// The data length isn't the product of the shape.
    #[error("holds {found} values, its shape needs {expected}")]
    Length {
        /// The product of the shape.
        expected: usize,
        /// The data length.
        found: usize,
    },
    /// More than [`MAX_RANK`] dimensions, or more elements than memory can address.
    #[error("is too large")]
    TooLarge,
}

/// Why a [`Session::run`](crate::Session::run) produced nothing. Only [`RunError::Failed`] carries heap data, so the
/// errors a real-time caller sees repeatedly ([`Unavailable`](RunError::Unavailable)) cost nothing.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum RunError {
    /// A tensor doesn't match the signature: a bug in the caller, fixed by reading
    /// [`Session::signature`](crate::Session::signature).
    #[error("{port} {index} {problem}")]
    Mismatch {
        /// Inputs or outputs.
        port: Port,
        /// Its position.
        index: usize,
        /// What is wrong.
        problem: TensorProblem,
    },
    /// No result this time: the slot is switching models or falling back. Skip this run and call again; a real-time
    /// caller treats it like a dropped frame.
    #[error("the model is being switched; no result this time")]
    Unavailable,
    /// The model's sandboxed worker crashed or stalled past its slot's deadline. A slot session handles this itself
    /// (it falls back and returns [`Unavailable`](RunError::Unavailable)); only a raw runtime session returns it.
    #[error("the model's worker crashed or stopped responding")]
    Crashed,
    /// The runtime reported an error for this run.
    #[error("the model failed: {0}")]
    Failed(String),
}

/// Borrowed tensor data, by element type.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum TensorData<'a> {
    /// [`DType::F32`].
    F32(&'a [f32]),
    /// [`DType::F16`], as raw bits.
    F16(&'a [u16]),
    /// [`DType::I64`].
    I64(&'a [i64]),
    /// [`DType::I32`].
    I32(&'a [i32]),
    /// [`DType::U8`].
    U8(&'a [u8]),
}

impl TensorData<'_> {
    /// The element type.
    pub fn dtype(&self) -> DType {
        match self {
            TensorData::F32(_) => DType::F32,
            TensorData::F16(_) => DType::F16,
            TensorData::I64(_) => DType::I64,
            TensorData::I32(_) => DType::I32,
            TensorData::U8(_) => DType::U8,
        }
    }

    /// The number of elements.
    pub fn len(&self) -> usize {
        match self {
            TensorData::F32(d) => d.len(),
            TensorData::F16(d) => d.len(),
            TensorData::I64(d) => d.len(),
            TensorData::I32(d) => d.len(),
            TensorData::U8(d) => d.len(),
        }
    }

    /// True if there are no elements.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Mutable tensor storage handed to a runtime by [`TensorBuf::resize`], by element type.
#[derive(Debug, PartialEq)]
pub enum TensorDataMut<'a> {
    /// [`DType::F32`].
    F32(&'a mut [f32]),
    /// [`DType::F16`], as raw bits.
    F16(&'a mut [u16]),
    /// [`DType::I64`].
    I64(&'a mut [i64]),
    /// [`DType::I32`].
    I32(&'a mut [i32]),
    /// [`DType::U8`].
    U8(&'a mut [u8]),
}

/// A borrowed input tensor: data plus shape. `Copy`, so building the input list of a run costs nothing.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct TensorRef<'a> {
    /// The elements, row-major.
    pub data: TensorData<'a>,
    /// The dimensions; empty for a scalar.
    pub shape: &'a [usize],
}

impl<'a> TensorRef<'a> {
    /// An `f32` tensor.
    pub fn f32(data: &'a [f32], shape: &'a [usize]) -> Self {
        Self { data: TensorData::F32(data), shape }
    }

    /// An `i64` tensor.
    pub fn i64(data: &'a [i64], shape: &'a [usize]) -> Self {
        Self { data: TensorData::I64(data), shape }
    }

    /// A byte tensor; UTF-8 text is one of these with shape `[len]`.
    pub fn u8(data: &'a [u8], shape: &'a [usize]) -> Self {
        Self { data: TensorData::U8(data), shape }
    }

    /// The element type.
    pub fn dtype(&self) -> DType {
        self.data.dtype()
    }
}

/// Owned storage of one element type, kept between runs.
#[derive(Clone, Debug, PartialEq)]
enum Storage {
    F32(Vec<f32>),
    F16(Vec<u16>),
    I64(Vec<i64>),
    I32(Vec<i32>),
    U8(Vec<u8>),
}

/// A reusable output tensor. The caller makes one per output (with the signature's element type) and passes the same
/// buffers to every run; a session sets the shape and writes the data through [`TensorBuf::resize`]. Storage only
/// grows, so after the first run at the largest size, runs allocate nothing.
///
/// Callers may also build inputs in one and pass [`TensorBuf::as_ref`].
#[derive(Clone, Debug, PartialEq)]
pub struct TensorBuf {
    storage: Storage,
    dims: [usize; MAX_RANK],
    rank: usize,
}

impl TensorBuf {
    /// An empty buffer (shape `[0]`) of `dtype`.
    pub fn new(dtype: DType) -> Self {
        let storage = match dtype {
            DType::F32 => Storage::F32(Vec::new()),
            DType::F16 => Storage::F16(Vec::new()),
            DType::I64 => Storage::I64(Vec::new()),
            DType::I32 => Storage::I32(Vec::new()),
            DType::U8 => Storage::U8(Vec::new()),
        };
        Self { storage, dims: [0; MAX_RANK], rank: 1 }
    }

    /// One buffer per output of `signature`, ready for [`Session::run`](crate::Session::run).
    pub fn for_outputs(signature: &Signature) -> Vec<TensorBuf> {
        signature.outputs.iter().map(|spec| TensorBuf::new(spec.dtype)).collect()
    }

    /// The element type.
    pub fn dtype(&self) -> DType {
        self.data().dtype()
    }

    /// The current shape.
    pub fn shape(&self) -> &[usize] {
        &self.dims[..self.rank]
    }

    /// The current data.
    pub fn data(&self) -> TensorData<'_> {
        match &self.storage {
            Storage::F32(v) => TensorData::F32(v),
            Storage::F16(v) => TensorData::F16(v),
            Storage::I64(v) => TensorData::I64(v),
            Storage::I32(v) => TensorData::I32(v),
            Storage::U8(v) => TensorData::U8(v),
        }
    }

    /// The data as `f32`, if that is the element type.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.storage {
            Storage::F32(v) => Some(v),
            _ => None,
        }
    }

    /// The data as bytes, if that is the element type (text outputs are UTF-8 bytes).
    pub fn as_u8(&self) -> Option<&[u8]> {
        match &self.storage {
            Storage::U8(v) => Some(v),
            _ => None,
        }
    }

    /// This buffer as an input tensor.
    pub fn as_ref(&self) -> TensorRef<'_> {
        TensorRef { data: self.data(), shape: self.shape() }
    }

    /// Copies `src` (same element type) into this buffer: how a stream queues a pushed block and how a session hands
    /// back a cached result. Allocates only to grow.
    ///
    /// # Errors
    /// [`TensorProblem::DType`] for another element type, [`TensorProblem::Length`] if `src`'s data doesn't fill its
    /// shape, [`TensorProblem::TooLarge`] as for [`resize`](Self::resize).
    pub fn set(&mut self, src: &TensorRef<'_>) -> Result<(), TensorProblem> {
        let expected = element_count(src.shape)?;
        if src.data.len() != expected {
            return Err(TensorProblem::Length { expected, found: src.data.len() });
        }
        let dtype = self.dtype();
        match (self.resize(src.shape)?, src.data) {
            (TensorDataMut::F32(d), TensorData::F32(s)) => d.copy_from_slice(s),
            (TensorDataMut::F16(d), TensorData::F16(s)) => d.copy_from_slice(s),
            (TensorDataMut::I64(d), TensorData::I64(s)) => d.copy_from_slice(s),
            (TensorDataMut::I32(d), TensorData::I32(s)) => d.copy_from_slice(s),
            (TensorDataMut::U8(d), TensorData::U8(s)) => d.copy_from_slice(s),
            _ => return Err(TensorProblem::DType { expected: dtype, found: src.dtype() }),
        }
        Ok(())
    }

    /// Sets the shape and returns storage for exactly that many elements (zero-filled where it grew). Allocates only
    /// when the buffer has never been this large; this is how a runtime writes an output.
    ///
    /// # Errors
    /// [`TensorProblem::TooLarge`] for more than [`MAX_RANK`] dimensions or an element count that overflows.
    pub fn resize(&mut self, shape: &[usize]) -> Result<TensorDataMut<'_>, TensorProblem> {
        let n = element_count(shape)?;
        self.dims[..shape.len()].copy_from_slice(shape);
        self.rank = shape.len();
        Ok(match &mut self.storage {
            Storage::F32(v) => TensorDataMut::F32(sized(v, n)),
            Storage::F16(v) => TensorDataMut::F16(sized(v, n)),
            Storage::I64(v) => TensorDataMut::I64(sized(v, n)),
            Storage::I32(v) => TensorDataMut::I32(sized(v, n)),
            Storage::U8(v) => TensorDataMut::U8(sized(v, n)),
        })
    }
}

fn sized<T: Copy + Default>(v: &mut Vec<T>, n: usize) -> &mut [T] {
    v.resize(n, T::default());
    v.as_mut_slice()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str, dtype: DType, shape: &[Dim]) -> TensorSpec {
        TensorSpec::new(name, dtype, shape.to_vec())
    }

    fn vad_slot() -> Signature {
        Signature {
            inputs: vec![
                spec("input", DType::F32, &[Dim::Fixed(1), Dim::Fixed(576)]),
                spec("state", DType::F32, &[Dim::Fixed(2), Dim::Fixed(1), Dim::Fixed(128)]),
                spec("sr", DType::I64, &[]),
            ],
            outputs: vec![spec("output", DType::F32, &[Dim::Fixed(1), Dim::Fixed(1)])],
        }
    }

    #[test]
    fn a_model_fits_by_name_whatever_its_port_order_and_extra_outputs() {
        let mut model = vad_slot();
        model.inputs[0].shape = vec![Dim::ANY, Dim::Fixed(576)]; // a dynamic batch takes the slot's fixed 1
        model.inputs.reverse();
        model.outputs.push(spec("debug", DType::F32, &[Dim::ANY]));
        assert_eq!(vad_slot().accepts(&model), Ok(()));
    }

    #[test]
    fn every_mismatch_is_reported_at_once() {
        let mut model = vad_slot();
        model.inputs[0].dtype = DType::F16;
        model.inputs[1].shape = vec![Dim::Fixed(2), Dim::Fixed(128)];
        model.inputs.push(spec("hidden", DType::F32, &[Dim::Fixed(64)]));
        model.outputs.clear();
        let errors = vad_slot().accepts(&model).unwrap_err();
        assert_eq!(errors.len(), 4, "{errors:?}");
        assert!(errors.iter().any(|e| matches!(e, Mismatch::ExtraInput { tensor } if tensor == "hidden")));
        assert!(errors.contains(&Mismatch::MissingOutput { tensor: "output".into() }));
    }

    #[test]
    fn a_fixed_model_input_cannot_take_a_slot_that_feeds_any_size() {
        let slot = Signature { inputs: vec![spec("pcm", DType::F32, &[Dim::ANY])], outputs: vec![] };
        let model = Signature { inputs: vec![spec("pcm", DType::F32, &[Dim::Fixed(480_000)])], outputs: vec![] };
        let errors = slot.accepts(&model).unwrap_err();
        assert!(matches!(errors[0], Mismatch::Dim { axis: 0, .. }));
        // The other way round is fine: the slot always feeds 480 000.
        assert_eq!(model.accepts(&slot), Ok(()));
    }

    #[test]
    fn inputs_are_checked_for_count_type_shape_and_length() {
        let slot = vad_slot();
        let frame = [0.0f32; 576];
        let state = [0.0f32; 256];
        let sr = [16_000i64];
        let ok = [TensorRef::f32(&frame, &[1, 576]), TensorRef::f32(&state, &[2, 1, 128]), TensorRef::i64(&sr, &[])];
        assert_eq!(slot.check_inputs(&ok), Ok(()));
        let missing = RunError::Mismatch { port: Port::Input, index: 2, problem: TensorProblem::Missing };
        assert_eq!(slot.check_inputs(&ok[..2]), Err(missing));
        let mut short = ok;
        short[0] = TensorRef::f32(&frame[..512], &[1, 576]);
        assert!(matches!(
            slot.check_inputs(&short),
            Err(RunError::Mismatch { index: 0, problem: TensorProblem::Length { expected: 576, found: 512 }, .. })
        ));
        let mut wrong = ok;
        wrong[1] = TensorRef::f32(&state, &[1, 2, 128]);
        assert!(matches!(slot.check_inputs(&wrong), Err(RunError::Mismatch { index: 1, .. })));
    }

    #[test]
    fn buffers_keep_their_capacity_and_report_their_shape() {
        let mut buf = TensorBuf::new(DType::F32);
        if let TensorDataMut::F32(d) = buf.resize(&[2, 3]).unwrap() {
            d.copy_from_slice(&[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        }
        assert_eq!((buf.shape(), buf.as_f32().unwrap().len()), (&[2usize, 3][..], 6));
        assert!(matches!(buf.resize(&[1]).unwrap(), TensorDataMut::F32(d) if d == [1.0]));
        assert_eq!(buf.resize(&[1; MAX_RANK + 1]), Err(TensorProblem::TooLarge));
        assert_eq!(buf.resize(&[usize::MAX, 2]), Err(TensorProblem::TooLarge));
        assert_eq!(buf.as_ref().dtype(), DType::F32);
    }

    #[test]
    fn set_copies_a_block_of_the_same_type_only() {
        let mut buf = TensorBuf::new(DType::F32);
        buf.set(&TensorRef::f32(&[1.0, 2.0], &[2])).unwrap();
        assert_eq!((buf.shape(), buf.as_f32().unwrap()), (&[2usize][..], &[1.0, 2.0][..]));
        let wrong = TensorRef::u8(b"ab", &[2]);
        assert_eq!(buf.set(&wrong), Err(TensorProblem::DType { expected: DType::F32, found: DType::U8 }));
        assert_eq!(buf.set(&TensorRef::f32(&[1.0], &[2])), Err(TensorProblem::Length { expected: 2, found: 1 }));
    }

    #[test]
    fn io_renames_map_model_ports_onto_slot_names() {
        let model = Signature {
            inputs: vec![spec("x", DType::F32, &[Dim::ANY]), spec("sr", DType::I64, &[])],
            outputs: vec![spec("out0", DType::F32, &[Dim::ANY])],
        };
        let io = BTreeMap::from([("audio".to_owned(), "x".to_owned()), ("audio_out".to_owned(), "out0".to_owned())]);
        let named = model.with_slot_names(&io);
        let ports: Vec<&str> = named.inputs.iter().chain(&named.outputs).map(|t| t.name.as_str()).collect();
        assert_eq!(ports, ["audio", "sr", "audio_out"]);
    }

    #[test]
    fn dims_serialise_as_numbers_or_any() {
        let s = spec("x", DType::F32, &[Dim::Fixed(1), Dim::ANY]);
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"name":"x","dtype":"f32","shape":[1,"any"]}"#);
        assert_eq!(serde_json::from_str::<TensorSpec>(&json).unwrap(), s);
    }
}
