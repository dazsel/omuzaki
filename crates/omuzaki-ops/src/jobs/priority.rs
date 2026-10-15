//! Job threads run below normal priority (ADR 0036), so heavy work never competes with the interactive threads.
//!
//! The calls are the platform's own, declared here against the C library and `kernel32` that the standard library
//! already links, so no crate is added for two functions.

/// The nice value a job thread runs at on Linux, at least: "below normal" on the scale from -20 to 19.
#[cfg(target_os = "linux")]
pub(super) const NICE: i32 = 10;

/// Lowers the calling thread's priority; `false` if the platform refused or has no per-thread call.
///
/// Linux: `setpriority(PRIO_PROCESS, 0, …)` names the calling thread, not the process. The nice value only rises
/// (a process that already runs nicer than [`NICE`] keeps its value), so it never needs a privilege.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)] // an OS shim: the two libc calls std does not wrap
pub(super) fn lower() -> bool {
    use std::ffi::{c_int, c_uint};

    const PRIO_PROCESS: c_int = 0;
    unsafe extern "C" {
        fn getpriority(which: c_int, who: c_uint) -> c_int;
        fn setpriority(which: c_int, who: c_uint, prio: c_int) -> c_int;
    }
    // SAFETY: both calls take plain integers and touch only the calling thread's scheduling value.
    let current = unsafe { getpriority(PRIO_PROCESS, 0) };
    let target = current.clamp(NICE, 19);
    // SAFETY: as above.
    unsafe { setpriority(PRIO_PROCESS, 0, target) == 0 }
}

/// Lowers the calling thread's priority; `false` if the platform refused.
///
/// Windows: `SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL)`. The pseudo-handle needs no closing.
#[cfg(windows)]
#[allow(unsafe_code)] // an OS shim: the two kernel32 calls std does not wrap
pub(super) fn lower() -> bool {
    use std::ffi::c_void;

    const THREAD_PRIORITY_BELOW_NORMAL: i32 = -1;
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetCurrentThread() -> *mut c_void;
        fn SetThreadPriority(thread: *mut c_void, priority: i32) -> i32;
    }
    // SAFETY: `GetCurrentThread` returns a pseudo-handle that is always valid on the calling thread.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) != 0 }
}

/// Lowers the calling thread's priority: other platforms have no per-thread call the job manager uses yet (on macOS
/// `setpriority` would lower the whole process), so the thread keeps normal priority.
#[cfg(not(any(target_os = "linux", windows)))]
pub(super) fn lower() -> bool {
    false
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// The calling thread's nice value, from `/proc/thread-self/stat` (field 19).
    fn nice() -> i32 {
        let stat = std::fs::read_to_string("/proc/thread-self/stat").unwrap_or_default();
        let after = stat.rsplit_once(')').map_or("", |(_, rest)| rest);
        after.split_whitespace().nth(16).and_then(|n| n.parse().ok()).unwrap_or(i32::MIN)
    }

    #[test]
    fn lowering_changes_the_calling_thread_only() {
        let before = nice();
        let lowered = std::thread::spawn(|| (lower(), nice())).join().unwrap_or((false, i32::MIN));
        assert_eq!(lowered, (true, before.max(NICE)));
        assert_eq!(nice(), before, "the spawning thread keeps its priority");
    }
}
