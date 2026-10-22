//! The loaded session: runs on the caller's thread, presents exactly the slot's signature, and allocates nothing in a
//! run once it has seen the shapes it is called with (`Session::run`'s contract).
//!
//! How that is done with `ort`, which wraps every value in a heap object:
//! - **Inputs** are copied into tensors the session owns, one per model input, kept between runs and replaced only when
//!   a call arrives with another shape. (Copying costs a memcpy; wrapping the caller's slices allocates per run.)
//! - **Outputs** are preallocated tensors, built from the shapes the first run at a given input shape produced and
//!   reused until the input shapes change. That first run, and the first after each shape change, allocates; a model
//!   with fixed shapes does it once.
//! - **The run itself** calls ONNX Runtime's `Run` through [`raw::RawRun`] with name and pointer lists built once:
//!   `ort`'s own run keeps its lists on the stack only up to 6 inputs and 4 outputs, and copies a name list of 768 bytes
//!   or more, on every run. `IoBinding` was measured and not used: it allocates 2 per run for one output even with
//!   every tensor bound once, because each run wraps every output in a new value.
//! - Only the outputs the slot reads are computed.
//! - All of a call's output shapes are checked against the slot before any caller buffer is written.
//! - `float16` tensors travel as raw bits ([`half::f16`] only at the `ort` boundary), so a round trip is bit for bit.

use half::f16;
use omuzaki_ai_api::{
    DType, Dim, MAX_RANK, Port, RunError, Session, SessionInfo, Signature, TensorBuf, TensorData, TensorDataMut,
    TensorProblem, TensorRef,
};
use ort::memory::Allocator;
use ort::session::{OutputSelector, RunOptions, Session as OrtSession, SessionInputValue};
use ort::value::{DynValue, Tensor};

use crate::graph::Skipped;

mod raw;
use raw::RawRun;

/// Output tensors preallocated for one set of input shapes.
struct Prepared {
    /// The input shapes (model input order) these outputs were sized for.
    input_shapes: Vec<Vec<usize>>,
    /// One per picked output, in the order of [`OrtModelSession::picks`].
    outputs: Vec<DynValue>,
}

/// A model loaded through ONNX Runtime.
pub struct OrtModelSession {
    session: OrtSession,
    signature: Signature,
    /// For each of the model's inputs, in the model's order, the position of the slot input that feeds it.
    feeds: Vec<usize>,
    /// For each slot output, the model's name for it (the ones to compute and read).
    picks: Vec<String>,
    info: SessionInfo,
    skipped: Vec<Skipped>,
    /// The session's own input tensors, in model order, and the shapes they have.
    values: Vec<SessionInputValue<'static>>,
    shapes: Vec<Vec<usize>>,
    prepared: Option<Prepared>,
    /// Names and pointer lists for `Run`, built on the first prepared run.
    raw: Option<RawRun>,
    /// A prepared run was refused for outputs sized by the data, not the input shapes: allocate outputs per call.
    data_dependent: bool,
}

impl OrtModelSession {
    pub(crate) fn new(
        session: OrtSession,
        signature: Signature,
        feeds: Vec<usize>,
        picks: Vec<String>,
        info: SessionInfo,
        skipped: Vec<Skipped>,
    ) -> Self {
        let shapes = vec![Vec::new(); feeds.len()];
        Self {
            session,
            signature,
            feeds,
            picks,
            info,
            skipped,
            values: Vec::new(),
            shapes,
            prepared: None,
            raw: None,
            data_dependent: false,
        }
    }

    /// The providers that were asked for and not used, with the reason for each, in the order they were tried. Empty
    /// when the first choice (or the CPU, when that was all that was asked for) worked.
    pub fn skipped_providers(&self) -> &[Skipped] {
        &self.skipped
    }
}

fn failed(error: &ort::Error) -> RunError {
    RunError::Failed(error.to_string())
}

fn tensor(dtype: DType, shape: &[usize]) -> Result<DynValue, RunError> {
    let allocator = Allocator::default();
    let value = match dtype {
        DType::F32 => Tensor::<f32>::new(&allocator, shape).map(Tensor::into_dyn),
        DType::I64 => Tensor::<i64>::new(&allocator, shape).map(Tensor::into_dyn),
        DType::I32 => Tensor::<i32>::new(&allocator, shape).map(Tensor::into_dyn),
        DType::U8 => Tensor::<u8>::new(&allocator, shape).map(Tensor::into_dyn),
        DType::F16 => Tensor::<f16>::new(&allocator, shape).map(Tensor::into_dyn),
        other => return Err(RunError::Failed(format!("tensors of type {other:?} are not carried by this runtime"))),
    };
    value.map_err(|e| failed(&e))
}

/// Copies the caller's input into the session's tensor, which has the same shape and type.
fn fill(value: &mut SessionInputValue<'static>, data: TensorData<'_>) -> Result<(), RunError> {
    let SessionInputValue::Owned(value) = value else {
        return Err(RunError::Failed("an input tensor is not owned by the session".to_owned()));
    };
    macro_rules! write {
        ($ty:ty, $src:expr, $convert:expr) => {
            value.try_extract_tensor_mut::<$ty>().map(|(_, dst)| {
                for (d, s) in dst.iter_mut().zip($src) {
                    *d = $convert(*s);
                }
            })
        };
    }
    match data {
        TensorData::F32(d) => write!(f32, d, |s| s),
        TensorData::I64(d) => write!(i64, d, |s| s),
        TensorData::I32(d) => write!(i32, d, |s| s),
        TensorData::U8(d) => write!(u8, d, |s| s),
        TensorData::F16(d) => write!(f16, d, f16::from_bits),
    }
    .map_err(|e| failed(&e))
}

/// Checks an output's actual shape against the slot's: same rank, same fixed dimensions. (`check_outputs` only knows
/// the buffers; the shapes are the model's to produce.)
fn check_shape(signature: &Signature, index: usize, shape: &[usize]) -> Result<(), RunError> {
    let mismatch = |problem| RunError::Mismatch { port: Port::Output, index, problem };
    let spec = &signature.outputs[index];
    if spec.shape.len() != shape.len() {
        return Err(mismatch(TensorProblem::Rank { expected: spec.shape.len(), found: shape.len() }));
    }
    for (axis, (dim, &found)) in spec.shape.iter().zip(shape).enumerate() {
        if let Dim::Fixed(expected) = *dim
            && expected != found
        {
            return Err(mismatch(TensorProblem::Dim { axis, expected, found }));
        }
    }
    Ok(())
}

/// An output's dimensions in the form the buffers take.
fn dims(shape: &ort::value::Shape) -> Result<([usize; MAX_RANK], usize), RunError> {
    let rank = shape.len();
    if rank > MAX_RANK {
        return Err(RunError::Failed(format!("an output has {rank} dimensions, more than {MAX_RANK}")));
    }
    let mut out = [0usize; MAX_RANK];
    for (slot, &d) in out.iter_mut().zip(shape.iter()) {
        *slot = usize::try_from(d).map_err(|_| RunError::Failed("an output has a negative dimension".to_owned()))?;
    }
    Ok((out, rank))
}

/// An output's actual dimensions, read as `dtype`.
fn output_dims(value: &DynValue, dtype: DType) -> Result<([usize; MAX_RANK], usize), RunError> {
    match dtype {
        DType::F32 => dims(value.try_extract_tensor::<f32>().map_err(|e| failed(&e))?.0),
        DType::I64 => dims(value.try_extract_tensor::<i64>().map_err(|e| failed(&e))?.0),
        DType::I32 => dims(value.try_extract_tensor::<i32>().map_err(|e| failed(&e))?.0),
        DType::U8 => dims(value.try_extract_tensor::<u8>().map_err(|e| failed(&e))?.0),
        DType::F16 => dims(value.try_extract_tensor::<f16>().map_err(|e| failed(&e))?.0),
        other => Err(RunError::Failed(format!("outputs of type {other:?} are not carried by this runtime"))),
    }
}

/// Checks one output's actual shape against the slot's, without touching any buffer.
fn check_output(signature: &Signature, index: usize, value: &DynValue, dtype: DType) -> Result<(), RunError> {
    let (dims, rank) = output_dims(value, dtype)?;
    check_shape(signature, index, &dims[..rank])
}

/// Copies one ONNX Runtime output (already checked) into the caller's buffer through [`TensorBuf::resize`].
fn copy_out(value: &DynValue, buf: &mut TensorBuf) -> Result<(), RunError> {
    macro_rules! copy {
        ($ty:ty, $variant:ident, $convert:expr) => {{
            let (shape, data) = value.try_extract_tensor::<$ty>().map_err(|e| failed(&e))?;
            let (dims, rank) = dims(shape)?;
            match buf.resize(&dims[..rank]) {
                Ok(TensorDataMut::$variant(dst)) if dst.len() == data.len() => {
                    for (d, s) in dst.iter_mut().zip(data) {
                        *d = $convert(*s);
                    }
                }
                Ok(_) => return Err(RunError::Failed("an output has an unexpected size or type".to_owned())),
                Err(problem) => return Err(RunError::Failed(problem.to_string())),
            }
        }};
    }
    match buf.dtype() {
        DType::F32 => copy!(f32, F32, |s| s),
        DType::I64 => copy!(i64, I64, |s| s),
        DType::I32 => copy!(i32, I32, |s| s),
        DType::U8 => copy!(u8, U8, |s| s),
        DType::F16 => copy!(f16, F16, f16::to_bits),
        other => return Err(RunError::Failed(format!("outputs of type {other:?} are not carried by this runtime"))),
    }
    Ok(())
}

/// Moves every picked output into the caller's buffers, or none: all shapes are checked against the slot before the
/// first buffer is touched, so a rejected call leaves the caller's buffers as they were. `values` yields the outputs in
/// slot order and is walked twice.
fn deliver<'a>(
    signature: &Signature,
    values: impl Iterator<Item = &'a DynValue> + Clone,
    outputs: &mut [TensorBuf],
) -> Result<(), RunError> {
    for (index, (value, buf)) in values.clone().zip(outputs.iter()).enumerate() {
        check_output(signature, index, value, buf.dtype())?;
    }
    for (value, buf) in values.zip(outputs.iter_mut()) {
        copy_out(value, buf)?;
    }
    Ok(())
}

impl OrtModelSession {
    /// Gives every model input a tensor of the call's shape (replacing it only when the shape changed) and fills it.
    fn load_inputs(&mut self, inputs: &[TensorRef<'_>]) -> Result<(), RunError> {
        for (position, &slot_input) in self.feeds.iter().enumerate() {
            let call = &inputs[slot_input];
            if self.values.len() <= position || self.shapes[position] != call.shape {
                let fresh = SessionInputValue::Owned(tensor(call.dtype(), call.shape)?);
                if self.values.len() <= position {
                    self.values.push(fresh);
                } else {
                    self.values[position] = fresh;
                }
                self.shapes[position].clear();
                self.shapes[position].extend_from_slice(call.shape);
            }
            fill(&mut self.values[position], call.data)?;
        }
        Ok(())
    }

    fn prepared_for_these_inputs(&self) -> bool {
        self.prepared.as_ref().is_some_and(|p| p.input_shapes == self.shapes)
    }

    /// Runs into the preallocated outputs. `None` when ONNX Runtime refused (the outputs no longer fit).
    fn run_prepared(&mut self, outputs: &mut [TensorBuf]) -> Option<Result<(), RunError>> {
        let prepared = self.prepared.as_mut()?;
        let raw = self.raw.as_mut()?;
        raw.run(&self.session, &self.values, &mut prepared.outputs).ok()?;
        Some(deliver(&self.signature, prepared.outputs.iter(), outputs))
    }

    /// Runs with the outputs ONNX Runtime allocates, copies them out, and sizes output tensors for the next call at
    /// these input shapes.
    fn run_and_learn(&mut self, outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        let mut selector = OutputSelector::no_default();
        for name in &self.picks {
            selector = selector.with(name.as_str());
        }
        let options = RunOptions::new().map_err(|e| failed(&e))?.with_outputs(selector);
        let results = self.session.run_with_options(self.values.as_slice(), &options).map_err(|e| failed(&e))?;
        deliver(&self.signature, self.picks.iter().map(|name| &results[name.as_str()]), outputs)?;
        drop(results);
        if self.raw.is_none() {
            let inputs = self.session.inputs().iter().map(ort::value::Outlet::name);
            self.raw = Some(RawRun::new(inputs, self.picks.iter().map(String::as_str)).map_err(RunError::Failed)?);
        }
        if !self.data_dependent {
            let prepared = outputs.iter().map(|buf| tensor(buf.dtype(), buf.shape())).collect::<Result<_, _>>()?;
            self.prepared = Some(Prepared { input_shapes: self.shapes.clone(), outputs: prepared });
        }
        Ok(())
    }
}

impl Session for OrtModelSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        self.load_inputs(inputs)?;
        if !self.prepared_for_these_inputs() {
            self.prepared = None;
            return self.run_and_learn(outputs);
        }
        if let Some(done) = self.run_prepared(outputs) {
            return done;
        }
        // The output sizes depend on more than the input shapes (data-dependent shapes, as a TTS model's waveform):
        // size them afresh, and from now on let ONNX Runtime allocate them. A refused prepared run has already done
        // most of the graph's work, so trying again on every call would nearly double the cost of each.
        self.prepared = None;
        self.data_dependent = true;
        self.run_and_learn(outputs)
    }

    fn reset(&mut self) {
        // State travels in tensors (the slot's own ports), so there is nothing carried inside ONNX Runtime to forget.
    }

    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}
