//! A global allocator that attributes allocations to who asked for them, for the hot-loop tests of any crate (the
//! renderer's, and the stage area's face on the GPU).
//!
//! A library can't install a global allocator for the programs that link it, so each test binary does, naming the crate
//! it tests:
//!
//! ```ignore
//! #[global_allocator]
//! static ALLOCATOR: omuzaki_render_mock::counting::Counting =
//!     omuzaki_render_mock::counting::Counting::new(env!("CARGO_PKG_NAME"));
//! ```
//!
//! Every allocation made on the measuring thread while [`measure`] runs is attributed to the first backtrace frame whose
//! source file is not the standard library's or this file's: the crate under test ([`OWN`]), another workspace crate
//! ([`WORKSPACE`]), tests and mocks ([`TESTS`]), wgpu ([`WGPU`]), Vello ([`VELLO`]) or any other dependency. Counting is
//! off while the hook itself runs (taking a backtrace allocates), and off outside [`measure`].
//!
//! **A hook that takes backtraces must never run inside someone else's backtrace.** `std::backtrace::Backtrace` holds a
//! process-wide, non-reentrant lock while it captures, and capturing allocates. With `RUST_BACKTRACE=1` (CI sets it)
//! wgpu-core captures a backtrace on every snatch-lock acquisition in debug builds, so the hook was entered by an
//! allocation made *inside* that capture, tried to take the same lock on the same thread, and waited forever (zero CPU,
//! one futex wait per test thread). `unwinding::inside_capture` spots that case from the raw stack, without taking
//! any lock, and the hook stands aside. [`measure_within`] adds a deadline, so a hang of any other kind aborts with a
//! message instead of freezing the run.

// A counting allocator is the only way to *measure* allocation; the workspace denies unsafe code by default, and a
// global allocator and the unwinder's C interface are unsafe by definition. This module only counts and forwards.
#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::backtrace::Backtrace;
use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::io::Write;
use std::sync::mpsc;
use std::time::Duration;

/// The counting allocator: install it as the test binary's `#[global_allocator]` (see the module docs).
#[derive(Debug)]
pub struct Counting {
    /// The package name of the crate under test (`env!("CARGO_PKG_NAME")` in the test): its allocations are [`OWN`].
    own: &'static str,
}

impl Counting {
    /// The allocator for tests of the crate `own`, by its package name.
    pub const fn new(own: &'static str) -> Self {
        Self { own }
    }
}

thread_local! {
    // `const` init with no destructor: reading these never allocates, so it is safe inside the allocator.
    static MEASURING: Cell<bool> = const { Cell::new(false) };
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
    static BY_OWNER: RefCell<BTreeMap<&'static str, usize>> = const { RefCell::new(BTreeMap::new()) };
}

/// The crate under test's own code.
pub const OWN: &str = "this crate";
/// Another crate of the workspace (an API crate, a sibling implementation).
pub const WORKSPACE: &str = "other workspace crates";
/// The test itself and the mocks it drives.
pub const TESTS: &str = "test and mock";
/// wgpu and its parts (wgpu-core, wgpu-hal, naga): allocations inside its calls, which no caller can avoid.
pub const WGPU: &str = "wgpu";
/// Vello and the crates it encodes scenes with (`vello_encoding`, peniko, kurbo).
pub const VELLO: &str = "vello";
/// Every other dependency.
pub const DEPENDENCIES: &str = "other dependencies";
/// No frame of the backtrace had a source path.
pub const UNATTRIBUTED: &str = "unattributed";

/// How the names of the global allocator's entry points start, after their path (`__rustc::__rust_alloc`).
const ALLOCATOR_SHIMS: [&str; 3] = ["__rg_", "__rust_", "__rdl_"];

/// Whether `path` is a source file of the workspace crate `own` (`crates/<own>/src/...`).
fn in_crate(path: &str, own: &str) -> bool {
    path.split("crates/").skip(1).any(|rest| rest.strip_prefix(own).is_some_and(|rest| rest.starts_with("/src/")))
}

/// Who owns an allocation, from a backtrace, when `own` is the crate under test. Lines look like `   3: name` followed
/// by `             at path:line:col`.
///
/// The owner is the innermost frame that is the workspace's, a test's, wgpu's or Vello's. Other dependencies (a hash
/// map, a lock, a GPU memory allocator) are plumbing like std: an allocation inside one belongs to whoever called it, so
/// the workspace's own code can't hide an allocation behind a container, and wgpu's internals count as wgpu's. Only
/// an allocation with no such caller at all is [`DEPENDENCIES`].
fn owner(backtrace: &str, own: &str) -> &'static str {
    let mut in_dependency = false;
    let mut symbol = "";
    for line in backtrace.lines().map(str::trim_start) {
        // `   3: name` names a frame; the `at path` lines after it say where it is.
        let Some(path) = line.strip_prefix("at ") else {
            symbol = line.split_once(": ").map_or("", |(_, name)| name);
            continue;
        };
        // alloc, core and std plumbing, this hook, and the global allocator's shims (`__rg_alloc`, which the compiler
        // writes where the test declares the allocator): look further out for who asked.
        if path.starts_with("/rustc/")
            || path.contains(file!())
            || ALLOCATOR_SHIMS.iter().any(|shim| symbol.rsplit("::").next().is_some_and(|name| name.starts_with(shim)))
        {
            continue;
        }
        // The crate under test is compiled from its own directory, so its files have relative paths.
        if path.starts_with("./src/") || in_crate(path, own) {
            return OWN;
        } else if path.starts_with("./tests/") || path.contains("-mock/") || path.contains("/tests/") {
            return TESTS;
        } else if path.contains("crates/omuzaki-") {
            return WORKSPACE;
        } else if path.contains("/wgpu") || path.contains("/naga-") {
            return WGPU;
        } else if path.contains("/vello") || path.contains("/peniko-") || path.contains("/kurbo-") {
            return VELLO;
        }
        in_dependency = true;
    }
    if in_dependency { DEPENDENCIES } else { UNATTRIBUTED }
}

/// Where allocations made while a dependency captures its own backtrace are filed: they are that dependency's
/// debugging aid, not part of what a frame costs, so no owner assertion looks at them.
pub const IN_DEPENDENCY_BACKTRACE: &str = "inside a dependency's backtrace capture";

fn count(own: &str) {
    let counted = MEASURING.try_with(Cell::get).unwrap_or(false) && !IN_HOOK.try_with(Cell::get).unwrap_or(true);
    if counted {
        IN_HOOK.with(|h| h.set(true));
        let who = if unwinding::inside_capture() {
            IN_DEPENDENCY_BACKTRACE
        } else {
            owner(&Backtrace::force_capture().to_string(), own)
        };
        BY_OWNER.with(|m| *m.borrow_mut().entry(who).or_insert(0) += 1);
        IN_HOOK.with(|h| h.set(false));
    }
}

/// Raw stack inspection through the unwinder std itself uses, which takes no lock of std's.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod unwinding {
    use std::ffi::{CStr, c_char, c_int, c_void};

    /// How many of the innermost frames to look at. A capture's own frames (the allocation, the vector growth, the
    /// capture callback, the unwinder) are the innermost ones, so this is far more than enough.
    const DEPTH: usize = 32;
    const CONTINUE: c_int = 0;
    const STOP: c_int = 5;

    #[repr(C)]
    struct DlInfo {
        file: *const c_char,
        base: *mut c_void,
        symbol: *const c_char,
        symbol_address: *mut c_void,
    }

    unsafe extern "C" {
        fn _Unwind_Backtrace(trace: extern "C" fn(*mut c_void, *mut c_void) -> c_int, argument: *mut c_void) -> c_int;
        fn _Unwind_GetIP(context: *mut c_void) -> usize;
        fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
    }

    struct Walk {
        seen: usize,
        inside: bool,
    }

    extern "C" fn visit(context: *mut c_void, argument: *mut c_void) -> c_int {
        // SAFETY: `argument` is the `Walk` that `inside_capture` passes and keeps alive for the whole walk, and
        // `context` is valid for the duration of this callback, as `_Unwind_GetIP` requires.
        let (walk, ip) = unsafe { (&mut *argument.cast::<Walk>(), _Unwind_GetIP(context)) };
        let mut info = DlInfo {
            file: std::ptr::null(),
            base: std::ptr::null_mut(),
            symbol: std::ptr::null(),
            symbol_address: std::ptr::null_mut(),
        };
        // SAFETY: `info` is a valid, writable `Dl_info`; `dladdr` only reads the address.
        let known = unsafe { dladdr(ip as *const c_void, &raw mut info) } != 0 && !info.symbol.is_null();
        // SAFETY: a non-null `dli_sname` is a NUL-terminated string that lives as long as the library.
        if known && unsafe { CStr::from_ptr(info.symbol) }.to_bytes() == b"_Unwind_Backtrace" {
            walk.inside = true;
            return STOP;
        }
        walk.seen += 1;
        if walk.seen >= DEPTH { STOP } else { CONTINUE }
    }

    /// Whether this thread is inside a call to `_Unwind_Backtrace`, which is where every std (or `backtrace` crate)
    /// capture runs its callback, and therefore holds the backtrace lock. Allocates nothing.
    pub(super) fn inside_capture() -> bool {
        let mut walk = Walk { seen: 0, inside: false };
        // SAFETY: `visit` matches the callback type and only touches the `Walk` it is given.
        unsafe { _Unwind_Backtrace(visit, (&raw mut walk).cast()) };
        walk.inside
    }
}

/// Other targets have no unwinder to ask; they keep the plain hook (and the same hazard if a dependency captures).
#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
mod unwinding {
    pub(super) fn inside_capture() -> bool {
        false
    }
}

// SAFETY: every method forwards to `System` unchanged; the counting is the only addition, and it cannot recurse.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        count(self.own);
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Growing a vector is an allocation as far as the hot loop is concerned.
        count(self.own);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

/// Whether backtraces here carry source paths, which attribution needs. They do with debug info (the default test
/// profile keeps line tables); a `--release` test build has none, and every allocation would be "unattributed".
pub fn can_attribute() -> bool {
    Backtrace::force_capture().to_string().contains(file!())
}

/// The longest a measured section may take before the process is aborted. A frame loop of a few dozen frames takes
/// seconds even under heavy load, so this only ever trips on a hang.
pub const DEADLINE: Duration = Duration::from_secs(300);

/// Runs `work` and returns how many allocations each owner made during it. Aborts the process, saying why, if `work`
/// takes longer than [`DEADLINE`].
pub fn measure(work: impl FnOnce()) -> BTreeMap<&'static str, usize> {
    measure_within(DEADLINE, work)
}

/// [`measure`] with a deadline of the caller's choosing.
pub fn measure_within(deadline: Duration, work: impl FnOnce()) -> BTreeMap<&'static str, usize> {
    // The watchdog is a thread of its own, started before counting begins, so it is never counted.
    let (finished, waiting) = mpsc::channel::<()>();
    let who = std::thread::current().name().unwrap_or("unnamed").to_owned();
    let watchdog = std::thread::spawn(move || {
        if waiting.recv_timeout(deadline) == Err(mpsc::RecvTimeoutError::Timeout) {
            // Straight to stderr, past the harness's output capture, which an abort would otherwise swallow.
            let message =
                format!("counting::measure: `{who}` is still measuring after {deadline:?}; a hang, aborting\n");
            let _ = std::io::stderr().write_all(message.as_bytes());
            std::process::abort();
        }
    });
    BY_OWNER.with(|m| m.borrow_mut().clear());
    MEASURING.with(|m| m.set(true));
    work();
    MEASURING.with(|m| m.set(false));
    // A send error means the watchdog already gave up, which aborts; either way it is done.
    let _ = finished.send(());
    let _ = watchdog.join();
    BY_OWNER.with(|m| std::mem::take(&mut *m.borrow_mut()))
}
