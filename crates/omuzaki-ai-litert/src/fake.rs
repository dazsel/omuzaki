//! A stand-in for LiteRT's C API, so the session's own logic runs in unit tests with no native library: a function
//! table ([`api`]) over an interpreter of one `ADD` graph (`out = a + b`, `FLOAT32`, `a` and `b` the same shape).
//!
//! The model's bytes choose its faults, as words in them: `any` (the inputs start as `[1, 1]`, the model's `-1`
//! default, instead of `[1, 4]`), `rename` (LiteRT names input 0 `x`), `no-interpreter`, `fail-invoke`, and
//! `out-rank`, `out-bytes`, `out-type`, which bend the output only on a run whose `a[0]` is [`POISON`], so a test can
//! run the same session well first. The buffer `omz_litert_capture_errors` hands over gets each failure's text, as the
//! shim's reporter does.
//!
//! The binary's allocator counts each thread's allocations while [`count_allocations`] watches.
#![allow(unsafe_code, clippy::cast_sign_loss, clippy::cast_possible_wrap, clippy::cast_possible_truncation)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::{Cell, RefCell};
use std::ffi::{CStr, c_char, c_int, c_void};

use crate::library::{Api, Opaque, Status};

/// The `a[0]` that makes an `out-*` fault bend the output.
pub const POISON: f32 = 99.0;

thread_local! {
    static WATCHING: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    /// What was deleted, in order, on this thread.
    static DELETED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

struct Counting;

// Counts and forwards to `System`: no obligations beyond `GlobalAlloc`'s own.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if WATCHING.with(Cell::get) {
            ALLOCATIONS.with(|n| n.set(n.get() + 1));
        }
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

/// How many times `body` allocated on this thread.
pub fn count_allocations(body: impl FnOnce()) -> usize {
    ALLOCATIONS.with(|n| n.set(0));
    WATCHING.with(|w| w.set(true));
    body();
    WATCHING.with(|w| w.set(false));
    ALLOCATIONS.with(Cell::get)
}

/// What this thread deleted so far, oldest first, and forgets it.
pub fn take_deleted() -> Vec<&'static str> {
    DELETED.with(|d| std::mem::take(&mut *d.borrow_mut()))
}

fn deleted(what: &'static str) {
    DELETED.with(|d| d.borrow_mut().push(what));
}

struct Model {
    faults: String,
}

impl Model {
    fn has(&self, fault: &str) -> bool {
        self.faults.split_whitespace().any(|word| word == fault)
    }
}

struct Options {
    errors: *mut c_char,
    cap: usize,
}

struct Tensor {
    name: &'static CStr,
    ty: c_int,
    dims: Vec<i32>,
    data: Vec<f32>,
    /// Added to the byte size LiteRT reports.
    extra_bytes: usize,
}

impl Tensor {
    fn new(name: &'static CStr, dims: &[i32]) -> Self {
        Self { name, ty: 1, dims: dims.to_vec(), data: Vec::new(), extra_bytes: 0 }
    }
}

struct Interpreter {
    model: *const Model,
    errors: *mut c_char,
    cap: usize,
    /// `a`, `b`, `out`.
    tensors: [Tensor; 3],
    allocated: bool,
}

impl Interpreter {
    fn model(&self) -> &Model {
        // SAFETY: the session deletes the interpreter before the model.
        unsafe { &*self.model }
    }

    /// Writes `text` where the shim's reporter would, NUL-terminated within half the buffer (the shim keeps its own
    /// state in the tail).
    fn report(&self, text: &str) -> Status {
        if !self.errors.is_null() {
            let n = text.len().min(self.cap / 2 - 1);
            // SAFETY: the session's buffer, `cap` long.
            unsafe {
                std::ptr::copy_nonoverlapping(text.as_ptr(), self.errors.cast::<u8>(), n);
                self.errors.add(n).write(0);
            }
        }
        1
    }
}

unsafe fn interpreter<'a>(i: *const Opaque) -> &'a mut Interpreter {
    unsafe { &mut *i.cast_mut().cast::<Interpreter>() }
}

unsafe fn tensor<'a>(t: *const Opaque) -> &'a mut Tensor {
    unsafe { &mut *t.cast_mut().cast::<Tensor>() }
}

fn boxed<T>(value: T) -> *mut Opaque {
    Box::into_raw(Box::new(value)).cast()
}

unsafe extern "C" fn version() -> *const c_char {
    c"fake".as_ptr()
}
unsafe extern "C" fn schema_version() -> c_int {
    3
}
unsafe extern "C" fn model_create(data: *const c_void, size: usize) -> *mut Opaque {
    let bytes = unsafe { std::slice::from_raw_parts(data.cast::<u8>(), size) };
    boxed(Model { faults: String::from_utf8_lossy(bytes).into_owned() })
}
unsafe extern "C" fn model_delete(model: *mut Opaque) {
    deleted("model");
    drop(unsafe { Box::from_raw(model.cast::<Model>()) });
}
unsafe extern "C" fn options_create() -> *mut Opaque {
    boxed(Options { errors: std::ptr::null_mut(), cap: 0 })
}
unsafe extern "C" fn options_delete(options: *mut Opaque) {
    deleted("options");
    drop(unsafe { Box::from_raw(options.cast::<Options>()) });
}
unsafe extern "C" fn set_threads(_: *mut Opaque, _: i32) {}
unsafe extern "C" fn add_delegate(_: *mut Opaque, _: *mut Opaque) {}
unsafe extern "C" fn interpreter_create(model: *const Opaque, options: *const Opaque) -> *mut Opaque {
    let (model, options) = unsafe { (&*model.cast::<Model>(), &*options.cast::<Options>()) };
    let dims: &[i32] = if model.has("any") { &[1, 1] } else { &[1, 4] };
    let a = if model.has("rename") { c"x" } else { c"a" };
    let interpreter = Interpreter {
        model,
        errors: options.errors,
        cap: options.cap,
        tensors: [Tensor::new(a, dims), Tensor::new(c"b", dims), Tensor::new(c"out", dims)],
        allocated: false,
    };
    if model.has("no-interpreter") {
        interpreter.report("fake: no interpreter today");
        return std::ptr::null_mut();
    }
    boxed(interpreter)
}
unsafe extern "C" fn interpreter_delete(i: *mut Opaque) {
    deleted("interpreter");
    drop(unsafe { Box::from_raw(i.cast::<Interpreter>()) });
}
unsafe extern "C" fn allocate(i: *mut Opaque) -> Status {
    let i = unsafe { interpreter(i) };
    i.allocated = false;
    if i.tensors[0].dims != i.tensors[1].dims {
        return i.report("fake ADD: the inputs differ in shape");
    }
    let dims = i.tensors[0].dims.clone();
    let elements = dims.iter().map(|&d| d as usize).product();
    for t in &mut i.tensors {
        t.dims.clone_from(&dims);
        t.data.resize(elements, 0.0);
    }
    i.allocated = true;
    0
}
unsafe extern "C" fn invoke(i: *mut Opaque) -> Status {
    let i = unsafe { interpreter(i) };
    if !i.allocated {
        return i.report("fake: invoke before AllocateTensors");
    }
    if i.model().has("fail-invoke") {
        return i.report("fake: this model always fails");
    }
    let [a, b, out] = &mut i.tensors;
    for ((o, x), y) in out.data.iter_mut().zip(&a.data).zip(&b.data) {
        *o = x + y;
    }
    let poisoned = a.data.first() == Some(&POISON);
    let model = unsafe { &*i.model };
    // The faults bend the output in place: no allocation, so the allocation test still sees only the session.
    out.ty = if poisoned && model.has("out-type") { 2 } else { 1 };
    out.extra_bytes = if poisoned && model.has("out-bytes") { 4 } else { 0 };
    if poisoned && model.has("out-rank") {
        out.dims[0] = 2; // [2, n] with n's data: the slot's fixed `1` no longer holds
    } else {
        out.dims.clone_from_slice(&a.dims);
    }
    0
}
unsafe extern "C" fn resize(i: *mut Opaque, index: i32, dims: *const c_int, rank: i32) -> Status {
    let i = unsafe { interpreter(i) };
    let dims = unsafe { std::slice::from_raw_parts(dims, rank as usize) };
    i.tensors[index as usize].dims = dims.to_vec();
    i.allocated = false;
    0
}
unsafe extern "C" fn input_count(_: *const Opaque) -> i32 {
    2
}
unsafe extern "C" fn input(i: *const Opaque, index: i32) -> *mut Opaque {
    let i = unsafe { interpreter(i) };
    i.tensors
        .get_mut(index as usize)
        .filter(|_| index < 2)
        .map_or(std::ptr::null_mut(), |t| std::ptr::from_mut(t).cast())
}
unsafe extern "C" fn output_count(_: *const Opaque) -> i32 {
    1
}
unsafe extern "C" fn output(i: *const Opaque, index: i32) -> *const Opaque {
    let i = unsafe { interpreter(i) };
    if index == 0 { (&raw const i.tensors[2]).cast() } else { std::ptr::null() }
}
unsafe extern "C" fn tensor_type(t: *const Opaque) -> Status {
    unsafe { tensor(t) }.ty
}
unsafe extern "C" fn num_dims(t: *const Opaque) -> i32 {
    unsafe { tensor(t) }.dims.len() as i32
}
unsafe extern "C" fn dim(t: *const Opaque, axis: i32) -> i32 {
    unsafe { tensor(t) }.dims[axis as usize]
}
unsafe extern "C" fn name(t: *const Opaque) -> *const c_char {
    unsafe { tensor(t) }.name.as_ptr()
}
unsafe extern "C" fn byte_size(t: *const Opaque) -> usize {
    let t = unsafe { tensor(t) };
    t.data.len() * 4 + t.extra_bytes
}
unsafe extern "C" fn copy_from(t: *mut Opaque, data: *const c_void, size: usize) -> Status {
    let t = unsafe { tensor(t) };
    if size != t.data.len() * 4 {
        return 1;
    }
    unsafe { std::ptr::copy_nonoverlapping(data.cast::<f32>(), t.data.as_mut_ptr(), t.data.len()) };
    0
}
unsafe extern "C" fn copy_to(t: *const Opaque, data: *mut c_void, size: usize) -> Status {
    let t = unsafe { tensor(t) };
    if size != t.data.len() * 4 + t.extra_bytes {
        return 1;
    }
    unsafe { std::ptr::copy_nonoverlapping(t.data.as_ptr(), data.cast::<f32>(), t.data.len()) };
    0
}
unsafe extern "C" fn abi() -> u32 {
    crate::builds::OMZ_LITERT_ABI
}
unsafe extern "C" fn xnnpack_create(_: i32) -> *mut Opaque {
    boxed(0u8)
}
unsafe extern "C" fn xnnpack_delete(delegate: *mut Opaque) {
    deleted("delegate");
    drop(unsafe { Box::from_raw(delegate.cast::<u8>()) });
}
unsafe extern "C" fn capture_errors(options: *mut Opaque, buf: *mut c_char, cap: usize) {
    let options = unsafe { &mut *options.cast::<Options>() };
    options.errors = buf;
    options.cap = cap;
}

/// The fake's function table, for the life of the test binary.
pub fn api() -> &'static Api {
    static API: Api = Api {
        TfLiteVersion: version,
        TfLiteSchemaVersion: schema_version,
        TfLiteModelCreate: model_create,
        TfLiteModelDelete: model_delete,
        TfLiteInterpreterOptionsCreate: options_create,
        TfLiteInterpreterOptionsDelete: options_delete,
        TfLiteInterpreterOptionsSetNumThreads: set_threads,
        TfLiteInterpreterOptionsAddDelegate: add_delegate,
        TfLiteInterpreterCreate: interpreter_create,
        TfLiteInterpreterDelete: interpreter_delete,
        TfLiteInterpreterAllocateTensors: allocate,
        TfLiteInterpreterInvoke: invoke,
        TfLiteInterpreterResizeInputTensor: resize,
        TfLiteInterpreterGetInputTensorCount: input_count,
        TfLiteInterpreterGetInputTensor: input,
        TfLiteInterpreterGetOutputTensorCount: output_count,
        TfLiteInterpreterGetOutputTensor: output,
        TfLiteTensorType: tensor_type,
        TfLiteTensorNumDims: num_dims,
        TfLiteTensorDim: dim,
        TfLiteTensorName: name,
        TfLiteTensorByteSize: byte_size,
        TfLiteTensorCopyFromBuffer: copy_from,
        TfLiteTensorCopyToBuffer: copy_to,
        omz_litert_abi: abi,
        omz_litert_xnnpack_create: xnnpack_create,
        omz_litert_xnnpack_delete: xnnpack_delete,
        omz_litert_capture_errors: capture_errors,
    };
    &API
}
