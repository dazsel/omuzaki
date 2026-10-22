//! The one direct call into ONNX Runtime's C API: `Run` with names and value lists this crate keeps between runs.
//!
//! Why: `ort`'s own `Session::run_with_options` keeps its per-run name and value lists on the stack up to 6 inputs and
//! 4 outputs and copies them to the heap above that (and copies a name list of 768 bytes or more), on every run. Calling
//! `Run` ourselves with pre-built lists costs nothing per run at any port count. The other candidate, `IoBinding`,
//! was measured and rejected: it wraps every output in a fresh heap-backed value on each run (2 allocations per run for
//! one output, with everything bound once).
#![allow(unsafe_code)] // the single FFI call below; `ort` exposes no allocation-free run

use std::ffi::{CStr, CString, c_char};
use std::ptr;

use ort::AsPointer;
use ort::session::SessionInputValue;
use ort::sys::OrtValue;
use ort::value::DynValue;

/// The names and pointer lists for one session's runs.
pub(super) struct RawRun {
    /// Owns the bytes the name pointers below point into; never resized after `new`, so the pointers stay valid.
    _names: Vec<CString>,
    input_names: Vec<*const c_char>,
    output_names: Vec<*const c_char>,
    /// Refilled on every run; the capacity is reserved up front.
    inputs: Vec<*const OrtValue>,
    outputs: Vec<*mut OrtValue>,
}

// SAFETY: the pointers in `input_names` and `output_names` point into the heap buffers of `_names`, which this struct
// owns and never mutates. `inputs` and `outputs` are cleared before `run` returns and hold nothing between runs.
unsafe impl Send for RawRun {}

impl RawRun {
    /// Names for a model's inputs (in the model's order) and the outputs to compute (in the order the caller reads them).
    ///
    /// # Errors
    /// A name containing a NUL byte, which no C string can hold.
    pub(super) fn new<'a>(
        inputs: impl ExactSizeIterator<Item = &'a str>,
        outputs: impl ExactSizeIterator<Item = &'a str>,
    ) -> Result<Self, String> {
        let (n_in, n_out) = (inputs.len(), outputs.len());
        let names = inputs
            .chain(outputs)
            .map(|name| CString::new(name).map_err(|_| format!("the port name `{name}` contains a NUL byte")))
            .collect::<Result<Vec<_>, _>>()?;
        let pointers: Vec<*const c_char> = names.iter().map(|n| n.as_ptr()).collect();
        Ok(Self {
            input_names: pointers[..n_in].to_vec(),
            output_names: pointers[n_in..].to_vec(),
            _names: names,
            inputs: Vec::with_capacity(n_in),
            outputs: Vec::with_capacity(n_out),
        })
    }

    /// Runs `session` on `inputs` (one per name given to `new`) into `outputs` (one per output name), which ONNX Runtime
    /// fills in place. Allocates nothing on the Rust side after the first call.
    ///
    /// # Errors
    /// ONNX Runtime's message when it refuses the run (for example, an output tensor of the wrong shape).
    pub(super) fn run(
        &mut self,
        session: &ort::session::Session,
        inputs: &[SessionInputValue<'static>],
        outputs: &mut [DynValue],
    ) -> Result<(), String> {
        if inputs.len() != self.input_names.len() || outputs.len() != self.output_names.len() {
            return Err(format!(
                "{} inputs and {} outputs were given for {} input names and {} output names",
                inputs.len(),
                outputs.len(),
                self.input_names.len(),
                self.output_names.len()
            ));
        }
        self.inputs.clear();
        for value in inputs {
            let SessionInputValue::Owned(value) = value else {
                self.inputs.clear();
                return Err("an input tensor is not owned by the session".to_owned());
            };
            self.inputs.push(value.ptr());
        }
        self.outputs.clear();
        self.outputs.extend(outputs.iter_mut().map(AsPointer::ptr_mut));
        // SAFETY: `session` is a live ONNX Runtime session. Each name pointer is a NUL-terminated string owned by
        // `self._names`, and each value pointer comes from a live `ort` value that the caller borrows for this call
        // (inputs shared, outputs exclusively, all distinct). The counts equal the list lengths (checked above). A
        // null run-options pointer means the defaults. ONNX Runtime only reads the lists during the call.
        let status = unsafe {
            (ort::api().Run)(
                session.ptr().cast_mut(),
                ptr::null(),
                self.input_names.as_ptr(),
                self.inputs.as_ptr(),
                self.inputs.len(),
                self.output_names.as_ptr(),
                self.output_names.len(),
                self.outputs.as_mut_ptr(),
            )
        };
        self.inputs.clear();
        self.outputs.clear();
        if status.0.is_null() {
            return Ok(());
        }
        // SAFETY: a non-null status is owned by us; its message is valid until we release it.
        let message = unsafe {
            let text = CStr::from_ptr((ort::api().GetErrorMessage)(status.0)).to_string_lossy().into_owned();
            (ort::api().ReleaseStatus)(status.0);
            text
        };
        Err(message)
    }
}
