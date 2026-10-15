//! The loaded session (design §3.5, §3.6): one LiteRT interpreter with the shim's XNNPACK delegate, presenting exactly
//! the slot's signature, on the caller's thread.
//!
//! - **Load** checks the interpreter against the scan (counts, names, element types), resizes an `any` input the slot
//!   fixes, and allocates the tensors once.
//! - **Run** copies each input in, invokes, then checks every output (type, rank, dims, byte size) against the slot
//!   before the first caller buffer is written. A call whose input shapes differ from the last call's takes the cold
//!   path first: resize and `AllocateTensors`. Otherwise a run allocates nothing.
//! - Every value LiteRT hands back is checked before Rust relies on it; a failed check is an error, never a panic.
#![allow(unsafe_code)] // calls into LiteRT's C API; each call says why it is sound

use std::collections::BTreeMap;
use std::ffi::{c_char, c_int, c_void};
use std::ptr::NonNull;

use omuzaki_ai_api::{
    DType, Dim, LoadError, MAX_RANK, ModelRef, Port, Provider, RunError, Session, SessionInfo, SessionOptions,
    Signature, TensorBuf, TensorData, TensorDataMut, TensorProblem, TensorRef,
};

use crate::library::{Api, Opaque, Status, c_text};
use crate::scan::MAX_NAME_BYTES;

/// The bytes the shim's error reporter may write LiteRT's last message into, its own state included.
pub const ERROR_BYTES: usize = 1024;

/// Why a provider other than the CPU is skipped.
pub const CPU_ONLY: &str = "the `tflite` runtime runs on the CPU (XNNPACK) only";

/// `kTfLiteOk`.
const OK: Status = 0;

/// The `TfLiteType` (`tflite/core/c/c_api_types.h`) and element size of a `DType` the runtime carries, or `None`.
fn carried(dtype: DType) -> Option<(c_int, usize)> {
    match dtype {
        DType::F32 => Some((1, 4)),
        DType::I32 => Some((2, 4)),
        DType::U8 => Some((3, 1)),
        DType::I64 => Some((4, 8)),
        _ => None,
    }
}

/// The `TfLiteType` of a carried `DType`.
fn tflite_type(dtype: DType) -> Option<c_int> {
    carried(dtype).map(|(ty, _)| ty)
}

/// A shape held without allocating.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Dims {
    dims: [usize; MAX_RANK],
    rank: usize,
}

impl Dims {
    const EMPTY: Self = Self { dims: [0; MAX_RANK], rank: 0 };

    /// `shape`, which callers have checked is at most `MAX_RANK` long (`Signature::check_inputs` refuses longer).
    fn of(shape: &[usize]) -> Self {
        debug_assert!(shape.len() <= MAX_RANK, "a shape over MAX_RANK reached the session");
        let mut dims = Self::EMPTY;
        let rank = shape.len().min(MAX_RANK);
        dims.dims[..rank].copy_from_slice(&shape[..rank]);
        dims.rank = rank;
        dims
    }

    fn as_slice(&self) -> &[usize] {
        &self.dims[..self.rank]
    }

    /// The element count, or `None` when it overflows.
    fn elements(&self) -> Option<usize> {
        self.as_slice().iter().try_fold(1usize, |n, &d| n.checked_mul(d))
    }
}

/// The C handles of one session, deleted in the order design §3.5 gives: the interpreter, the delegate, the options,
/// the model. Built one by one at load, so a failure part-way deletes only what exists.
struct Handles {
    api: &'static Api,
    interpreter: Option<NonNull<Opaque>>,
    delegate: Option<NonNull<Opaque>>,
    options: Option<NonNull<Opaque>>,
    model: Option<NonNull<Opaque>>,
}

impl Drop for Handles {
    fn drop(&mut self) {
        // SAFETY: each handle came from its own create function of this library, is deleted once, and the interpreter
        // goes before the delegate and options it uses, and those before the model it was built from.
        unsafe {
            if let Some(interpreter) = self.interpreter.take() {
                (self.api.TfLiteInterpreterDelete)(interpreter.as_ptr());
            }
            if let Some(delegate) = self.delegate.take() {
                (self.api.omz_litert_xnnpack_delete)(delegate.as_ptr());
            }
            if let Some(options) = self.options.take() {
                (self.api.TfLiteInterpreterOptionsDelete)(options.as_ptr());
            }
            if let Some(model) = self.model.take() {
                (self.api.TfLiteModelDelete)(model.as_ptr());
            }
        }
    }
}

/// The buffer the shim's reporter writes LiteRT's messages into. It stays at one address for the session's life
/// (the options hold a pointer into it), and Rust reads it only between calls into LiteRT.
struct ErrorText(NonNull<[u8]>);

impl ErrorText {
    fn new() -> Self {
        Self(NonNull::from(Box::leak(vec![0u8; ERROR_BYTES].into_boxed_slice())))
    }

    fn as_ptr(&self) -> *mut c_char {
        self.0.as_ptr().cast()
    }

    /// The last message, read up to its NUL within [`ERROR_BYTES`], as lossy UTF-8.
    fn text(&self) -> String {
        // SAFETY: the buffer is `ERROR_BYTES` long and no LiteRT call is running (`&self` of a session that is
        // `&mut` in every call into LiteRT, or a load that holds it alone).
        let bytes = unsafe { c_text(self.as_ptr(), ERROR_BYTES) }.unwrap_or_default();
        String::from_utf8_lossy(&bytes).into_owned()
    }
}

impl Drop for ErrorText {
    fn drop(&mut self) {
        // SAFETY: the box leaked in `new`, freed once; the options that pointed into it are gone (`Handles` is
        // declared before this in `LiteRtSession`, so it drops first).
        drop(unsafe { Box::from_raw(self.0.as_ptr()) });
    }
}

/// A model loaded through LiteRT.
pub struct LiteRtSession {
    // Field order is drop order: the handles before the error buffer and the model bytes they point into.
    handles: Handles,
    errors: ErrorText,
    /// `TfLiteModelCreate` reads the bytes for as long as the model lives.
    bytes: Box<[u8]>,
    signature: Signature,
    /// For each model input, in the model's order, the slot input that feeds it.
    feeds: Vec<usize>,
    /// For each slot output, the model output it reads.
    picks: Vec<usize>,
    /// Each model input's shape in the interpreter; `None` after a failed resize, so the next run sets it again.
    shapes: Vec<Option<Dims>>,
    /// Each slot output's dims from the run being checked: read once, then copied out.
    outputs: Vec<Dims>,
    info: SessionInfo,
    skipped: Vec<Provider>,
}

// SAFETY: LiteRT's interpreter has no thread affinity, but must not be called from two threads at once. Every call
// into it takes `&mut self` (`run`) or happens before the session exists (load), so moving the session to another
// thread is sound; it is not `Sync`, so it can't be shared.
unsafe impl Send for LiteRtSession {}

impl LiteRtSession {
    /// The providers that were asked for and not used, in order; each was skipped because of [`CPU_ONLY`].
    pub fn skipped_providers(&self) -> &[Provider] {
        &self.skipped
    }
}

/// Loads `bytes`, whose scan gave `model`, as a session presenting `slot` (`io` maps slot names to the model's).
///
/// # Errors
/// [`LoadError::Format`] when the model doesn't fit the slot or LiteRT disagrees with the scan,
/// [`LoadError::Runtime`] when LiteRT can't build the interpreter.
pub(crate) fn create(
    api: &'static Api,
    bytes: Vec<u8>,
    model: &Signature,
    slot: &Signature,
    io: &BTreeMap<String, String>,
    options: &SessionOptions,
    model_ref: ModelRef,
) -> Result<LiteRtSession, LoadError> {
    let (feeds, picks) = wiring(model, slot, io)?;
    let threads = i32::from(options.threads.max(1));
    let mut session = LiteRtSession {
        handles: Handles { api, interpreter: None, delegate: None, options: None, model: None },
        errors: ErrorText::new(),
        bytes: bytes.into_boxed_slice(),
        signature: slot.clone(),
        shapes: vec![None; feeds.len()],
        outputs: vec![Dims::EMPTY; picks.len()],
        feeds,
        picks,
        info: SessionInfo { model: model_ref, provider: Provider::Cpu, sandboxed: false, fallback_for: None },
        skipped: options.providers.iter().copied().filter(|p| *p != Provider::Cpu).collect(),
    };
    session.build(threads)?;
    session.check_ports(model)?;
    session.fit_slot()?;
    Ok(session)
}

/// Which slot input feeds each model input, and which model output each slot output reads, matched by name after
/// `io`'s renaming, as `omuzaki-ai-ort` does it.
fn wiring(
    model: &Signature,
    slot: &Signature,
    io: &BTreeMap<String, String>,
) -> Result<(Vec<usize>, Vec<usize>), LoadError> {
    let named = model.with_slot_names(io);
    slot.accepts(&named).map_err(|mismatches| {
        let list: Vec<String> = mismatches.iter().map(ToString::to_string).collect();
        LoadError::Format(format!("the model does not fit the slot: {}", list.join("; ")))
    })?;
    let feeds = named
        .inputs
        .iter()
        .map(|port| slot.inputs.iter().position(|s| s.name == port.name))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| LoadError::Format("a model input has no slot input".to_owned()))?;
    let picks = slot
        .outputs
        .iter()
        .map(|port| named.outputs.iter().position(|m| m.name == port.name))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| LoadError::Format("a slot output has no model output".to_owned()))?;
    Ok((feeds, picks))
}

impl LiteRtSession {
    fn api(&self) -> &'static Api {
        self.handles.api
    }

    /// The interpreter; set by `build` before anything else runs.
    fn interpreter(&self) -> *mut Opaque {
        self.handles.interpreter.map_or(std::ptr::null_mut(), NonNull::as_ptr)
    }

    /// `TfLiteModelCreate`, the options with `threads` and the error buffer, the delegate, then the interpreter.
    fn build(&mut self, threads: i32) -> Result<(), LoadError> {
        let api = self.api();
        // SAFETY: the bytes are the session's and outlive the model (`bytes` drops after `handles`).
        let model = unsafe { (api.TfLiteModelCreate)(self.bytes.as_ptr().cast(), self.bytes.len()) };
        self.handles.model =
            Some(NonNull::new(model).ok_or_else(|| LoadError::Format("LiteRT refused the model".to_owned()))?);
        // SAFETY: takes nothing; null is checked.
        let options = NonNull::new(unsafe { (api.TfLiteInterpreterOptionsCreate)() })
            .ok_or_else(|| LoadError::Runtime("LiteRT could not create interpreter options".to_owned()))?;
        self.handles.options = Some(options);
        // SAFETY: the options are live; the error buffer is `ERROR_BYTES` long, stays in place and outlives them.
        unsafe {
            (api.TfLiteInterpreterOptionsSetNumThreads)(options.as_ptr(), threads);
            (api.omz_litert_capture_errors)(options.as_ptr(), self.errors.as_ptr(), ERROR_BYTES);
        }
        // SAFETY: takes a thread count; null is checked.
        let delegate = NonNull::new(unsafe { (api.omz_litert_xnnpack_create)(threads) })
            .ok_or_else(|| LoadError::Runtime("XNNPACK could not start".to_owned()))?;
        self.handles.delegate = Some(delegate);
        // SAFETY: both live; the delegate outlives the interpreter that will use it (`Handles::drop`'s order).
        unsafe { (api.TfLiteInterpreterOptionsAddDelegate)(options.as_ptr(), delegate.as_ptr()) };
        // SAFETY: the model and options are live; null is checked.
        let interpreter = unsafe { (api.TfLiteInterpreterCreate)(model, options.as_ptr()) };
        let interpreter = NonNull::new(interpreter).ok_or_else(|| {
            LoadError::Runtime(format!("LiteRT could not build the interpreter: {}", self.errors.text()))
        })?;
        self.handles.interpreter = Some(interpreter);
        Ok(())
    }

    /// The interpreter's ports against the scan's: counts, names and element types (design §3.6).
    fn check_ports(&self, model: &Signature) -> Result<(), LoadError> {
        let api = self.api();
        let interpreter = self.interpreter();
        // SAFETY: the interpreter is live.
        let counts = unsafe {
            (
                (api.TfLiteInterpreterGetInputTensorCount)(interpreter),
                (api.TfLiteInterpreterGetOutputTensorCount)(interpreter),
            )
        };
        for (kind, count, specs) in [("inputs", counts.0, &model.inputs), ("outputs", counts.1, &model.outputs)] {
            if usize::try_from(count).ok() != Some(specs.len()) {
                return Err(LoadError::Format(format!(
                    "LiteRT reads {count} {kind} where the scan read {}",
                    specs.len()
                )));
            }
        }
        for (port, specs) in [(Port::Input, &model.inputs), (Port::Output, &model.outputs)] {
            for (index, spec) in specs.iter().enumerate() {
                let tensor = self.tensor(port, index).map_err(|e| LoadError::Format(e.to_string()))?;
                // SAFETY: a live tensor of the live interpreter; the name is read within `MAX_NAME_BYTES`.
                let (name, ty) =
                    unsafe { (c_text((api.TfLiteTensorName)(tensor), MAX_NAME_BYTES), (api.TfLiteTensorType)(tensor)) };
                if name.as_deref() != Some(spec.name.as_bytes()) {
                    return Err(LoadError::Format(format!("LiteRT names {port} {index} differently from the scan")));
                }
                if tflite_type(spec.dtype) != Some(ty) {
                    return Err(LoadError::Format(format!(
                        "LiteRT reads {port} {index} as type {ty}, the scan as {:?}",
                        spec.dtype
                    )));
                }
            }
        }
        Ok(())
    }

    /// Resizes each input to the slot's fixed dims where the model leaves them open, keeps the model's own where the
    /// slot does too, and allocates the tensors once.
    fn fit_slot(&mut self) -> Result<(), LoadError> {
        let mut targets = Vec::with_capacity(self.feeds.len());
        for (index, &feed) in self.feeds.iter().enumerate() {
            let current = self.dims(Port::Input, index).map_err(|e| LoadError::Format(e.to_string()))?;
            let spec = &self.signature.inputs[feed].shape;
            if spec.len() != current.rank {
                return Err(LoadError::Format(format!(
                    "LiteRT reads input {index} with rank {}, the slot {}",
                    current.rank,
                    spec.len()
                )));
            }
            let mut target = current;
            for (dim, slot) in target.dims.iter_mut().zip(spec) {
                if let Dim::Fixed(fixed) = *slot {
                    *dim = fixed;
                }
            }
            targets.push(target);
        }
        self.reshape(&targets).map_err(|e| LoadError::Runtime(e.to_string()))
    }

    /// The tensor of `port` at `index`, re-read on every use: LiteRT may move it on any `AllocateTensors`.
    fn tensor(&self, port: Port, index: usize) -> Result<*mut Opaque, RunError> {
        let api = self.api();
        let index32 = c_index(index)?;
        // SAFETY: the interpreter is live and the index below its port count (checked at load).
        let tensor = unsafe {
            match port {
                Port::Input => (api.TfLiteInterpreterGetInputTensor)(self.interpreter(), index32),
                Port::Output => (api.TfLiteInterpreterGetOutputTensor)(self.interpreter(), index32).cast_mut(),
            }
        };
        if tensor.is_null() { Err(failed("LiteRT returned no tensor for a port")) } else { Ok(tensor) }
    }

    /// A port's dims as LiteRT reports them: rank in `0..=MAX_RANK`, every dim non-negative.
    fn dims(&self, port: Port, index: usize) -> Result<Dims, RunError> {
        let api = self.api();
        let tensor = self.tensor(port, index)?;
        // SAFETY: a live tensor.
        let rank = usize::try_from(unsafe { (api.TfLiteTensorNumDims)(tensor) })
            .ok()
            .filter(|&r| r <= MAX_RANK)
            .ok_or_else(|| failed("LiteRT reports a rank below 0 or over MAX_RANK"))?;
        let mut dims = Dims { dims: [0; MAX_RANK], rank };
        for (axis, dim) in dims.dims[..rank].iter_mut().enumerate() {
            // SAFETY: a live tensor, and `axis` below its rank; `MAX_RANK` fits `i32`.
            let found = unsafe { (api.TfLiteTensorDim)(tensor, c_index(axis)?) };
            *dim = usize::try_from(found).map_err(|_| failed("LiteRT reports a negative dimension"))?;
        }
        Ok(dims)
    }

    /// Gives every input whose shape differs from `targets` that shape, then allocates; the cold path.
    fn reshape(&mut self, targets: &[Dims]) -> Result<(), RunError> {
        let api = self.api();
        for (index, target) in targets.iter().enumerate() {
            if self.shapes[index] == Some(*target) {
                continue;
            }
            self.shapes[index] = None;
            let mut c_dims = [0 as c_int; MAX_RANK];
            for (c, &d) in c_dims.iter_mut().zip(target.as_slice()) {
                *c = c_int::try_from(d).map_err(|_| failed("a dimension is larger than LiteRT takes"))?;
            }
            // SAFETY: a live interpreter, an input index below its count, and `rank` dims (at most `MAX_RANK`).
            let status = unsafe {
                (api.TfLiteInterpreterResizeInputTensor)(
                    self.interpreter(),
                    c_index(index)?,
                    c_dims.as_ptr(),
                    c_index(target.rank)?,
                )
            };
            if status != OK {
                return Err(self.native_error("LiteRT refused an input shape"));
            }
        }
        // SAFETY: a live interpreter.
        if unsafe { (api.TfLiteInterpreterAllocateTensors)(self.interpreter()) } != OK {
            self.shapes.iter_mut().for_each(|s| *s = None);
            return Err(self.native_error("LiteRT could not allocate tensors for these shapes"));
        }
        for (shape, target) in self.shapes.iter_mut().zip(targets) {
            *shape = Some(*target);
        }
        Ok(())
    }

    /// A failure with a fixed message; LiteRT's own text goes to the log, so the run doesn't format it.
    fn native_error(&self, what: &'static str) -> RunError {
        tracing::warn!(target: "omuzaki_ai_litert", "{what}: {}", self.errors.text());
        failed(what)
    }

    /// The cold path, taken only when a model input's shape differs from the last call's.
    fn fit_inputs(&mut self, inputs: &[TensorRef<'_>]) -> Result<(), RunError> {
        let changed = self
            .feeds
            .iter()
            .zip(&self.shapes)
            .any(|(&feed, shape)| shape.map(|s| s.as_slice() != inputs[feed].shape) != Some(false));
        if !changed {
            return Ok(());
        }
        let targets: Vec<Dims> = self.feeds.iter().map(|&feed| Dims::of(inputs[feed].shape)).collect();
        self.reshape(&targets)
    }

    fn copy_in(&self, inputs: &[TensorRef<'_>]) -> Result<(), RunError> {
        let api = self.api();
        for (index, &feed) in self.feeds.iter().enumerate() {
            let (data, len): (*const c_void, usize) = match inputs[feed].data {
                TensorData::F32(d) => (d.as_ptr().cast(), size_of_val(d)),
                TensorData::I32(d) => (d.as_ptr().cast(), size_of_val(d)),
                TensorData::I64(d) => (d.as_ptr().cast(), size_of_val(d)),
                TensorData::U8(d) => (d.as_ptr().cast(), size_of_val(d)),
                TensorData::F16(_) => return Err(failed("float16 tensors are not carried by this runtime")),
            };
            let tensor = self.tensor(Port::Input, index)?;
            // SAFETY: a live tensor; it is copied into only when its byte size equals the caller's slice, so neither
            // side is overrun.
            unsafe {
                if (api.TfLiteTensorByteSize)(tensor) != len {
                    return Err(failed("LiteRT's input tensor differs in size from the call's"));
                }
                if (api.TfLiteTensorCopyFromBuffer)(tensor, data, len) != OK {
                    return Err(self.native_error("LiteRT could not take an input"));
                }
            }
        }
        Ok(())
    }

    /// Every slot output against the slot before any caller buffer is written: element type, rank, fixed dims and
    /// byte size. The dims are kept for `copy_out`.
    fn check_results(&mut self, outputs: &[TensorBuf]) -> Result<(), RunError> {
        let api = self.api();
        for (index, (&model_index, buf)) in self.picks.iter().zip(outputs).enumerate() {
            let tensor = self.tensor(Port::Output, model_index)?;
            // SAFETY: a live tensor.
            let ty = unsafe { (api.TfLiteTensorType)(tensor) };
            if tflite_type(buf.dtype()) != Some(ty) {
                return Err(failed("LiteRT returned an output of another element type"));
            }
            let dims = self.dims(Port::Output, model_index)?;
            check_shape(&self.signature, index, dims.as_slice())?;
            let bytes = dims.elements().zip(carried(buf.dtype())).and_then(|(n, (_, size))| n.checked_mul(size));
            // SAFETY: a live tensor.
            if bytes != Some(unsafe { (api.TfLiteTensorByteSize)(tensor) }) {
                return Err(failed("LiteRT's output size does not match its shape"));
            }
            self.outputs[index] = dims;
        }
        Ok(())
    }

    /// Copies each checked output into the caller's buffer.
    fn copy_out(&self, outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        let api = self.api();
        for ((&model_index, dims), buf) in self.picks.iter().zip(&self.outputs).zip(outputs) {
            let tensor = self.tensor(Port::Output, model_index)?;
            let (data, len): (*mut c_void, usize) =
                match buf.resize(dims.as_slice()).map_err(|p| failed_owned(p.to_string()))? {
                    TensorDataMut::F32(d) => (d.as_mut_ptr().cast(), size_of_val(d)),
                    TensorDataMut::I32(d) => (d.as_mut_ptr().cast(), size_of_val(d)),
                    TensorDataMut::I64(d) => (d.as_mut_ptr().cast(), size_of_val(d)),
                    TensorDataMut::U8(d) => (d.as_mut_ptr().cast(), size_of_val(d)),
                    TensorDataMut::F16(_) => return Err(failed("float16 tensors are not carried by this runtime")),
                };
            // SAFETY: a live tensor whose byte size `check_results` found equal to `len`, the buffer's size.
            if unsafe { (api.TfLiteTensorCopyToBuffer)(tensor, data, len) } != OK {
                return Err(self.native_error("LiteRT could not hand an output back"));
            }
        }
        Ok(())
    }
}

/// A port index, axis or rank as the C API's `int32_t`.
fn c_index(value: usize) -> Result<i32, RunError> {
    i32::try_from(value).map_err(|_| failed("an index is out of LiteRT's range"))
}

fn failed(what: &'static str) -> RunError {
    RunError::Failed(what.to_owned())
}

fn failed_owned(what: String) -> RunError {
    RunError::Failed(what)
}

/// An output's shape against the slot's: same rank, every fixed dim equal (an `any` dim takes what came back).
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

impl Session for LiteRtSession {
    fn signature(&self) -> &Signature {
        &self.signature
    }

    fn run(&mut self, inputs: &[TensorRef<'_>], outputs: &mut [TensorBuf]) -> Result<(), RunError> {
        self.signature.check_inputs(inputs)?;
        self.signature.check_outputs(outputs)?;
        self.fit_inputs(inputs)?;
        self.copy_in(inputs)?;
        // SAFETY: a live interpreter with its tensors allocated (`fit_inputs`).
        if unsafe { (self.api().TfLiteInterpreterInvoke)(self.interpreter()) } != OK {
            return Err(self.native_error("LiteRT failed to run the model"));
        }
        self.check_results(outputs)?;
        self.copy_out(outputs)
    }

    fn reset(&mut self) {
        // Nothing to forget: the scan refuses variable tensors, variable ops and mutating inputs (design §5), so no
        // state survives a run inside the interpreter.
    }

    fn info(&self) -> SessionInfo {
        self.info.clone()
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_ai_api::{ModelId, Sha256, TensorSpec};

    use super::*;
    use crate::fake::{self, POISON};

    fn model_ref() -> ModelRef {
        ModelRef { id: ModelId::new("fake").unwrap(), version: "1.0.0".parse().unwrap(), digest: Sha256([1; 32]) }
    }

    /// The `ADD` graph's signature with each input dim `dims`, and its output's.
    fn add(names: [&str; 3], dims: &[Dim]) -> Signature {
        let spec = |name: &str| TensorSpec::new(name, DType::F32, dims.to_vec());
        Signature { inputs: vec![spec(names[0]), spec(names[1])], outputs: vec![spec(names[2])] }
    }

    fn four() -> Vec<Dim> {
        vec![Dim::Fixed(1), Dim::Fixed(4)]
    }

    fn load(faults: &str, model: &Signature, slot: &Signature) -> Result<LiteRtSession, LoadError> {
        let io = BTreeMap::new();
        create(fake::api(), faults.as_bytes().to_vec(), model, slot, &io, &SessionOptions::default(), model_ref())
    }

    fn run(
        session: &mut LiteRtSession,
        a: &[f32],
        b: &[f32],
        shape: &[usize],
        out: &mut [TensorBuf],
    ) -> Result<(), RunError> {
        session.run(&[TensorRef::f32(a, shape), TensorRef::f32(b, shape)], out)
    }

    #[test]
    fn a_fixed_add_graph_loads_and_runs() {
        let sig = add(["a", "b", "out"], &four());
        let mut session = load("", &sig, &sig).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        run(&mut session, &[1.0, 2.0, 3.0, 4.0], &[10.0; 4], &[1, 4], &mut out).unwrap();
        assert_eq!(out[0].shape(), [1, 4]);
        assert_eq!(out[0].as_f32().unwrap(), [11.0, 12.0, 13.0, 14.0]);
        assert_eq!(session.info().provider, Provider::Cpu);
    }

    #[test]
    fn the_slot_names_and_order_come_from_io_and_the_slot() {
        // The slot calls the inputs `y` and `x` (in that order) and the output `sum`; `io` maps them to the model's.
        let model = add(["a", "b", "out"], &four());
        let spec = |name: &str| TensorSpec::new(name, DType::F32, four());
        let slot = Signature { inputs: vec![spec("y"), spec("x")], outputs: vec![spec("sum")] };
        let io: BTreeMap<String, String> =
            [("x", "a"), ("y", "b"), ("sum", "out")].map(|(s, m)| (s.to_owned(), m.to_owned())).into();
        let options = SessionOptions::new(vec![Provider::Cuda, Provider::Cpu], 2);
        let mut session = create(fake::api(), Vec::new(), &model, &slot, &io, &options, model_ref()).unwrap();
        assert_eq!(session.skipped_providers(), [Provider::Cuda]);
        let mut out = TensorBuf::for_outputs(session.signature());
        // y = b = [10; 4], x = a = [1, 2, 3, 4].
        run(&mut session, &[10.0; 4], &[1.0, 2.0, 3.0, 4.0], &[1, 4], &mut out).unwrap();
        assert_eq!(out[0].as_f32().unwrap(), [11.0, 12.0, 13.0, 14.0]);
    }

    #[test]
    fn an_any_input_is_resized_to_the_slots_fixed_shape_at_load() {
        // The model says [1, -1] (LiteRT starts it at [1, 1]); the slot fixes [1, 4].
        let model = add(["a", "b", "out"], &[Dim::Fixed(1), Dim::ANY]);
        let slot = add(["a", "b", "out"], &four());
        let mut session = load("any", &model, &slot).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        let allocations = fake::count_allocations(|| {
            run(&mut session, &[1.0; 4], &[2.0; 4], &[1, 4], &mut out).unwrap();
        });
        assert_eq!(out[0].as_f32().unwrap(), [3.0; 4]);
        // Only the output buffer grew: the load already gave the interpreter the slot's shape.
        assert_eq!(allocations, 1);
    }

    #[test]
    fn a_new_input_shape_takes_the_cold_path_and_runs() {
        let any = add(["a", "b", "out"], &[Dim::Fixed(1), Dim::ANY]);
        let mut session = load("any", &any, &any).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        run(&mut session, &[1.0, 2.0], &[3.0, 4.0], &[1, 2], &mut out).unwrap();
        assert_eq!((out[0].shape(), out[0].as_f32().unwrap()), ([1, 2].as_slice(), [4.0, 6.0].as_slice()));
        run(&mut session, &[1.0; 3], &[1.0; 3], &[1, 3], &mut out).unwrap();
        assert_eq!((out[0].shape(), out[0].as_f32().unwrap()), ([1, 3].as_slice(), [2.0; 3].as_slice()));
        // And back: each change is one resize, the same shape again none.
        run(&mut session, &[0.5, 0.5], &[0.5, 0.5], &[1, 2], &mut out).unwrap();
        assert_eq!(out[0].as_f32().unwrap(), [1.0, 1.0]);
    }

    #[test]
    fn nothing_is_allocated_after_warm_up() {
        let any = add(["a", "b", "out"], &[Dim::Fixed(1), Dim::ANY]);
        let mut session = load("any", &any, &any).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        let (a, b) = ([1.0f32; 64], [2.0f32; 64]);
        for _ in 0..3 {
            run(&mut session, &a, &b, &[1, 64], &mut out).unwrap();
        }
        let allocations = fake::count_allocations(|| {
            for _ in 0..1000 {
                run(&mut session, &a, &b, &[1, 64], &mut out).unwrap();
            }
        });
        assert_eq!(allocations, 0);
        // A smaller shape after the larger: the cold path allocates (the targets), a run at it again does not.
        run(&mut session, &a[..8], &b[..8], &[1, 8], &mut out).unwrap();
        assert_eq!(fake::count_allocations(|| run(&mut session, &a[..8], &b[..8], &[1, 8], &mut out).unwrap()), 0);
    }

    #[test]
    fn a_shape_litert_refuses_is_an_error_and_the_next_good_call_recovers() {
        let any = add(["a", "b", "out"], &[Dim::Fixed(1), Dim::ANY]);
        let mut session = load("any", &any, &any).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        let refused = run_mixed(&mut session, &mut out);
        assert_eq!(refused, Err(failed("LiteRT could not allocate tensors for these shapes")));
        run(&mut session, &[1.0; 2], &[1.0; 2], &[1, 2], &mut out).unwrap();
        assert_eq!(out[0].as_f32().unwrap(), [2.0, 2.0]);
    }

    /// `a` as `[1, 2]` and `b` as `[1, 3]`: the fake's `ADD` refuses to allocate them.
    fn run_mixed(session: &mut LiteRtSession, out: &mut [TensorBuf]) -> Result<(), RunError> {
        session.run(&[TensorRef::f32(&[1.0; 2], &[1, 2]), TensorRef::f32(&[1.0; 3], &[1, 3])], out)
    }

    #[test]
    fn a_bad_output_is_refused_and_no_caller_buffer_changes() {
        let sig = add(["a", "b", "out"], &four());
        for (fault, expected) in [
            (
                "out-rank",
                RunError::Mismatch {
                    port: Port::Output,
                    index: 0,
                    problem: TensorProblem::Dim { axis: 0, expected: 1, found: 2 },
                },
            ),
            ("out-bytes", failed("LiteRT's output size does not match its shape")),
            ("out-type", failed("LiteRT returned an output of another element type")),
        ] {
            let mut session = load(fault, &sig, &sig).unwrap();
            let mut out = TensorBuf::for_outputs(session.signature());
            run(&mut session, &[1.0; 4], &[1.0; 4], &[1, 4], &mut out).unwrap();
            let before = out.clone();
            let poisoned = [POISON, 0.0, 0.0, 0.0];
            assert_eq!(run(&mut session, &poisoned, &[1.0; 4], &[1, 4], &mut out), Err(expected), "{fault}");
            assert_eq!(out, before, "{fault}: the caller's buffers are as they were");
        }
    }

    #[test]
    fn a_failed_invoke_is_a_fixed_message_with_litert_s_text_kept() {
        let sig = add(["a", "b", "out"], &four());
        let mut session = load("fail-invoke", &sig, &sig).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        assert_eq!(
            run(&mut session, &[1.0; 4], &[1.0; 4], &[1, 4], &mut out),
            Err(failed("LiteRT failed to run the model"))
        );
        assert_eq!(session.errors.text(), "fake: this model always fails");
    }

    #[test]
    fn calls_that_do_not_fit_the_slot_are_refused_before_litert() {
        let sig = add(["a", "b", "out"], &four());
        let mut session = load("", &sig, &sig).unwrap();
        let mut out = TensorBuf::for_outputs(session.signature());
        let wrong = run(&mut session, &[1.0; 3], &[1.0; 3], &[1, 3], &mut out);
        assert!(matches!(wrong, Err(RunError::Mismatch { port: Port::Input, index: 0, .. })), "{wrong:?}");
        let mut i64_out = vec![TensorBuf::new(DType::I64)];
        assert!(matches!(
            run(&mut session, &[1.0; 4], &[1.0; 4], &[1, 4], &mut i64_out),
            Err(RunError::Mismatch { port: Port::Output, .. })
        ));
    }

    #[test]
    fn load_refuses_what_litert_and_the_scan_disagree_on() {
        let sig = add(["a", "b", "out"], &four());
        let refused = |result: Result<LiteRtSession, LoadError>| result.map(|_| ()).unwrap_err();
        let renamed = refused(load("rename", &sig, &sig));
        assert!(matches!(&renamed, LoadError::Format(why) if why.contains("names input 0")), "{renamed:?}");
        let three = Signature { inputs: [sig.inputs.clone(), vec![sig.inputs[0].clone()]].concat(), ..sig.clone() };
        let mut three = three;
        three.inputs[2].name = "c".to_owned();
        let counted = refused(load("", &three, &three));
        assert!(
            matches!(&counted, LoadError::Format(why) if why.contains("2 inputs where the scan read 3")),
            "{counted:?}"
        );
        let none = refused(load("no-interpreter", &sig, &sig));
        assert_eq!(
            none,
            LoadError::Runtime("LiteRT could not build the interpreter: fake: no interpreter today".to_owned())
        );
        let slot = add(["a", "b", "sum"], &four());
        assert!(
            matches!(refused(load("", &sig, &slot)), LoadError::Format(why) if why.contains("does not fit the slot"))
        );
    }

    #[test]
    fn the_handles_are_deleted_interpreter_first_and_only_those_made() {
        let sig = add(["a", "b", "out"], &four());
        fake::take_deleted();
        drop(load("", &sig, &sig).unwrap());
        assert_eq!(fake::take_deleted(), ["interpreter", "delegate", "options", "model"]);
        let _ = load("no-interpreter", &sig, &sig);
        assert_eq!(fake::take_deleted(), ["delegate", "options", "model"]);
    }
}
